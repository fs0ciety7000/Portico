//! The [`Engine`]: single entry point for UIs.
//!
//! ## Model
//!
//! - **Commands** are async methods on a cheap-to-clone [`Engine`] handle.
//! - **Control plane**: [`EngineEvent`]s on a `tokio::sync::broadcast`
//!   channel ([`Engine::subscribe`]). Every UI renders state from events, so
//!   several views (or both UIs in tests) stay consistent.
//! - **Data plane**: each terminal gets a [`TerminalHandle`] with a bounded
//!   input `mpsc` (keystrokes, resize) and a bounded output `mpsc` (PTY bytes).
//!   Backpressure flows to the SSH channel window, never to the event bus.
//! - **Prompts**: when an operation needs the user (host key, password,
//!   passphrase), the owning task parks on a `oneshot` registered under a
//!   [`PromptId`], emits [`EngineEvent::Prompt`] and resumes when a UI calls
//!   [`Engine::respond`]. Only that operation waits; the engine stays live.
//! - **Actors**: one task per session owning the russh `Handle`; children
//!   (PTY channels, SFTP, forwards) are tasks under the session's
//!   `CancellationToken`, so closing a session tears everything down.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use secrecy::SecretString;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::{Error, Result};
use crate::events::{EngineEvent, Prompt, PromptKind};
use crate::forward::{ForwardSpec, TunnelInfo};
use crate::hosts::{HostEntry, inventory};
use crate::ids::{PromptId, SessionId, TerminalId, TransferId, TunnelId};
use crate::logs::LogLevel;
use crate::logs::{LogRecord, LogRing};
use crate::secrets::SecretStore;
use crate::session::{self, Attached, SessionCommand, SessionCtx};
use crate::sftp::{RemoteEntry, SftpClient, TransferRequest};
use crate::ssh_config::{ConfigWarning, ResolveContext, SshConfig};
use crate::store::Paths;

/// Capacity of the control-plane broadcast channel. Slow subscribers get
/// `RecvError::Lagged` and must resync from [`Engine`] queries.
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;
/// Max in-flight PTY output chunks per terminal (data plane).
pub const TERMINAL_OUTPUT_CAPACITY: usize = 256;
/// Max queued keystroke / paste chunks per terminal.
pub const TERMINAL_INPUT_CAPACITY: usize = 1024;

/// Default `ConnectTimeout` (per hop: TCP connect and SSH negotiation).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Default keepalive / latency probe interval (`ServerAliveInterval`).
pub const DEFAULT_KEEPALIVE: Duration = Duration::from_secs(15);
/// Default `ServerAliveCountMax`.
pub const DEFAULT_KEEPALIVE_MAX: u32 = 3;
/// Log lines kept per session for the debug panel.
pub const SESSION_LOG_CAPACITY: usize = 500;
/// Ended sessions whose logs stay readable (oldest forgotten first).
pub const ENDED_SESSIONS_KEPT: usize = 20;

/// Engine construction parameters.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub paths: Paths,
    pub local_user: String,
    pub event_capacity: usize,
    /// Where passwords and passphrases are remembered.
    pub secrets: Arc<dyn SecretStore>,
    /// `SSH_AUTH_SOCK` captured at startup (`IdentityAgent` may override it).
    pub agent_socket: Option<PathBuf>,
    pub connect_timeout: Duration,
    pub keepalive: Duration,
}

impl EngineConfig {
    /// Platform defaults. Secrets go to the system keyring, or to the
    /// encrypted vault when no keyring is available (Phase 3).
    pub fn discover() -> Result<Self> {
        let paths = Paths::discover()?;
        Ok(Self {
            secrets: crate::secrets::default_store(&paths),
            paths,
            local_user: local_user(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            agent_socket: std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            keepalive: DEFAULT_KEEPALIVE,
        })
    }

    pub fn resolve_context(&self, user_override: Option<String>) -> ResolveContext {
        ResolveContext {
            local_user: self.local_user.clone(),
            home: self.paths.home.clone(),
            user_override,
        }
    }
}

fn local_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_owned())
}

/// Terminal geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// What to connect to.
#[derive(Debug, Clone)]
pub struct ConnectRequest {
    /// Host alias (resolved through `ssh_config`) or raw host name.
    pub alias: String,
    pub user_override: Option<String>,
    /// Log in to the target with this private key only (copy-id
    /// verification): `publickey` alone, `IdentitiesOnly`, no certificate.
    /// Jump hosts authenticate as usual.
    pub only_identity: Option<std::path::PathBuf>,
}

impl ConnectRequest {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            user_override: None,
            only_identity: None,
        }
    }
}

/// Input side of a terminal (data plane). Cheap to clone.
///
/// Keystrokes and pastes go through a bounded queue drained by a dedicated
/// writer task (never starved by output). Resizes use a `watch` (only the
/// latest size matters, it can never be lost) and closing is a token (it can
/// never be lost either, and it is also triggered when the output side is
/// dropped).
#[derive(Debug, Clone)]
pub struct TerminalInputs {
    data: mpsc::Sender<Bytes>,
    size: tokio::sync::watch::Sender<PtySize>,
    close: CancellationToken,
}

/// Bytes the terminal could not queue (the remote side stopped reading).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("terminal input queue is full or closed")]
pub struct InputRejected;

impl TerminalInputs {
    pub(crate) fn new(
        data: mpsc::Sender<Bytes>,
        size: tokio::sync::watch::Sender<PtySize>,
        close: CancellationToken,
    ) -> Self {
        Self { data, size, close }
    }

    /// Queues keystrokes / pasted bytes / terminal replies without waiting.
    pub fn try_send(&self, bytes: Bytes) -> std::result::Result<(), InputRejected> {
        self.data.try_send(bytes).map_err(|_| InputRejected)
    }

    /// Queues bytes, waiting for room.
    pub async fn send(&self, bytes: Bytes) -> std::result::Result<(), InputRejected> {
        self.data.send(bytes).await.map_err(|_| InputRejected)
    }

    /// Announces a new size (`window-change`); the latest value always wins.
    pub fn resize(&self, size: PtySize) {
        self.size.send_if_modified(|current| {
            let changed = *current != size;
            *current = size;
            changed
        });
    }

    /// Closes the channel (EOF + CLOSE), idempotent.
    pub fn close(&self) {
        self.close.cancel();
    }

    pub fn is_closed(&self) -> bool {
        self.close.is_cancelled()
    }

    /// Resolves once the terminal is closed (by any side).
    pub async fn closed(&self) {
        self.close.cancelled().await;
    }
}

/// Data-plane endpoints of one PTY channel. Owned by exactly one UI view;
/// dropping `output` closes the channel.
#[derive(Debug)]
pub struct TerminalHandle {
    pub id: TerminalId,
    pub session: SessionId,
    pub input: TerminalInputs,
    pub output: mpsc::Receiver<Bytes>,
}

/// Answer to a [`crate::events::Prompt`].
#[derive(Debug)]
pub enum PromptResponse {
    /// Trust an unknown host key (and record it in `known_hosts`).
    AcceptHostKey,
    /// Password or passphrase. `remember` stores it in the secret store.
    Secret { value: SecretString, remember: bool },
    /// Keyboard-interactive answers, in prompt order.
    Answers(Vec<SecretString>),
    /// Abort the operation that asked.
    Cancel,
}

/// Result of loading the host inventory.
#[derive(Debug, Clone)]
pub struct HostInventory {
    pub hosts: Vec<HostEntry>,
    pub warnings: Vec<ConfigWarning>,
}

/// Cheap-to-clone handle on the engine.
#[derive(Debug, Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: EngineConfig,
    /// One SFTP client per session, opened on first use.
    sftp: Mutex<HashMap<SessionId, Arc<tokio::sync::OnceCell<Arc<SftpClient>>>>>,
    /// Running transfers (cancel tokens).
    transfers: Mutex<HashMap<TransferId, CancellationToken>>,
    /// Running tunnels.
    tunnels: Mutex<HashMap<TunnelId, (TunnelInfo, CancellationToken)>>,
    events: broadcast::Sender<EngineEvent>,
    prompts: Mutex<HashMap<PromptId, oneshot::Sender<PromptResponse>>>,
    sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    shutdown: CancellationToken,
    /// Session actors, so a shutdown can wait for clean closes.
    tasks: TaskTracker,
    // ── Phase 3: snippets ──
    snippets: Arc<crate::snippets::SnippetStore>,
    // ── Phase 6: open terminals and their recording taps ──
    /// Weak: only the terminal tasks keep a tap (and so a recording's
    /// queue) alive; if they are dropped without closing, the writer ends.
    terminals:
        Mutex<HashMap<TerminalId, (SessionId, std::sync::Weak<crate::recording::TerminalTap>)>>,
    /// Names of the recordings whose file is still open for writing (they
    /// cannot be deleted).
    recordings_active: Arc<Mutex<std::collections::HashSet<String>>>,
}

#[derive(Debug)]
struct SessionEntry {
    alias: String,
    user_override: Option<String>,
    commands: mpsc::Sender<SessionCommand>,
    cancel: CancellationToken,
    logs: Arc<Mutex<LogRing>>,
    probe: bool,
    started: time::OffsetDateTime,
    /// Set when the actor returned; the entry then only serves its logs.
    ended: Option<(time::OffsetDateTime, crate::logs::SessionOutcome)>,
}

impl Engine {
    /// Creates the engine. Must be called inside a Tokio runtime once sessions
    /// are implemented (they spawn tasks).
    pub fn new(config: EngineConfig) -> Self {
        let (events, _) = broadcast::channel(config.event_capacity.max(16));
        let snippets = Arc::new(crate::snippets::SnippetStore::new(config.paths.snippets()));
        Self {
            inner: Arc::new(Inner {
                snippets,
                config,
                sftp: Mutex::default(),
                transfers: Mutex::default(),
                tunnels: Mutex::default(),
                events,
                prompts: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                shutdown: CancellationToken::new(),
                tasks: TaskTracker::new(),
                terminals: Mutex::default(),
                recordings_active: Arc::default(),
            }),
        }
    }

    pub fn config(&self) -> &EngineConfig {
        &self.inner.config
    }

    /// Subscribes to the control plane.
    pub fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.inner.events.subscribe()
    }

    /// Parses `~/.ssh/config` (+ includes) and merges Portico metadata.
    pub async fn load_hosts(&self) -> Result<HostInventory> {
        let config = self.inner.config.clone();
        tokio::task::spawn_blocking(move || {
            let ssh = SshConfig::load(&config.paths.ssh_config(), &config.paths.ssh_dir)?;
            let (meta, meta_warnings) = crate::hosts::load_meta(&config.paths.hosts_meta())?;
            let hosts = inventory(&ssh, &meta, &config.resolve_context(None));
            let mut warnings = ssh.warnings().to_vec();
            warnings.extend(meta_warnings);
            warnings.extend(
                hosts
                    .iter()
                    .flat_map(|h| h.resolved.warnings.iter().cloned()),
            );
            Ok(HostInventory { hosts, warnings })
        })
        .await
        .map_err(|_| Error::EngineClosed)?
    }

    /// Starts an SSH session and returns immediately. Progress, prompts and
    /// the outcome are reported as [`EngineEvent`]s for the returned id.
    pub fn connect(&self, request: ConnectRequest) -> Result<SessionId> {
        if self.inner.shutdown.is_cancelled() {
            return Err(Error::EngineClosed);
        }
        let id = SessionId::next();
        let cancel = self.inner.shutdown.child_token();
        let logs = Arc::new(Mutex::new(LogRing::new(SESSION_LOG_CAPACITY)));
        let (tx, rx) = mpsc::channel(8);
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                id,
                SessionEntry {
                    alias: request.alias.clone(),
                    user_override: request.user_override.clone(),
                    commands: tx,
                    cancel: cancel.clone(),
                    logs: Arc::clone(&logs),
                    probe: request.only_identity.is_some(),
                    started: time::OffsetDateTime::now_utc(),
                    ended: None,
                },
            );
        let ctx = SessionCtx::new(self.clone(), id, cancel, logs);
        let engine = self.clone();
        self.inner.tasks.spawn(async move {
            let outcome = session::run(ctx, request, rx).await;
            engine.session_finished(id, outcome);
        });
        Ok(id)
    }

    /// Closes a session, or aborts it while connecting (pending prompts are
    /// withdrawn). Cancelling the session token is enough: the actor maps it
    /// to `Closed { UserRequested }` and closes channels and transport.
    // Kept `async` for API stability (callers already await it).
    #[allow(clippy::unused_async)]
    pub async fn disconnect(&self, id: SessionId) -> Result<()> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        sessions
            .get(&id)
            .ok_or(Error::UnknownSession(id))?
            .cancel
            .cancel();
        Ok(())
    }

    /// Closes every session but keeps the engine usable (a UI that reloads
    /// and lost track of them), unlike [`Engine::shutdown`].
    pub fn disconnect_all(&self) {
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for entry in sessions.values() {
            entry.cancel.cancel();
        }
    }

    /// Opens an interactive shell in a PTY on `session`. If the session is
    /// still connecting, the request is served as soon as it is connected;
    /// it fails if the session fails or closes first.
    pub async fn open_terminal(&self, session: SessionId, size: PtySize) -> Result<TerminalHandle> {
        let tx = {
            let sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            sessions
                .get(&session)
                .ok_or(Error::UnknownSession(session))?
                .commands
                .clone()
        };
        let (reply, answer) = oneshot::channel();
        tx.send(SessionCommand::OpenTerminal { size, reply })
            .await
            .map_err(|_| Error::SessionEnded(session))?;
        answer.await.map_err(|_| Error::SessionEnded(session))?
    }

    /// The connected transport of `session` (waits while it connects).
    pub(crate) async fn attach(&self, session: SessionId) -> Result<Attached> {
        let tx = {
            let sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            sessions
                .get(&session)
                .ok_or(Error::UnknownSession(session))?
                .commands
                .clone()
        };
        let (reply, answer) = oneshot::channel();
        tx.send(SessionCommand::Attach { reply })
            .await
            .map_err(|_| Error::SessionEnded(session))?;
        answer.await.map_err(|_| Error::SessionEnded(session))
    }

    // ── SFTP ────────────────────────────────────────────────────────────

    /// The session's SFTP client, opened on first use and reopened if the
    /// previous one died.
    async fn sftp(&self, session: SessionId) -> Result<(Arc<SftpClient>, Attached)> {
        let attached = match self.attach(session).await {
            Ok(a) => a,
            Err(e) => {
                // The session is gone: forget its client.
                self.inner
                    .sftp
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&session);
                return Err(e);
            }
        };
        loop {
            let cell = Arc::clone(
                self.inner
                    .sftp
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .entry(session)
                    .or_default(),
            );
            let client = cell
                .get_or_try_init(|| async { crate::sftp::open(&attached).await.map(Arc::new) })
                .await?;
            if !client.is_retired() {
                return Ok((Arc::clone(client), attached));
            }
            // Dead client: forget it and open a new one.
            let mut map = self
                .inner
                .sftp
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if map.get(&session).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
                map.remove(&session);
            }
        }
    }

    /// Runs an SFTP operation. Only a transport failure retires the client
    /// (a refused request, e.g. mkdir on an existing name, does not).
    async fn with_sftp<T, F, Fut>(&self, session: SessionId, op: F) -> Result<T>
    where
        F: FnOnce(Arc<SftpClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let (client, attached) = self.sftp(session).await?;
        let result = tokio::select! {
            biased;
            () = attached.ctx.cancel.cancelled() => Err(Error::SessionEnded(session)),
            r = op(Arc::clone(&client)) => r,
        };
        if let Err(e) = &result {
            client.note(e);
        }
        result
    }

    /// The remote home directory (where browsing starts).
    pub async fn sftp_home(&self, session: SessionId) -> Result<String> {
        self.with_sftp(session, |c| async move { c.home().await })
            .await
    }

    /// Lists a remote directory: directories first, then by name.
    pub async fn sftp_list(&self, session: SessionId, path: &str) -> Result<Vec<RemoteEntry>> {
        let path = path.to_owned();
        self.with_sftp(session, |c| async move { c.list(&path).await })
            .await
    }

    pub async fn sftp_stat(&self, session: SessionId, path: &str) -> Result<RemoteEntry> {
        let path = path.to_owned();
        self.with_sftp(session, |c| async move { c.stat(&path).await })
            .await
    }

    pub async fn sftp_mkdir(&self, session: SessionId, path: &str) -> Result<()> {
        let path = path.to_owned();
        self.with_sftp(session, |c| async move { c.mkdir(&path).await })
            .await
    }

    /// Renames or moves; refuses to replace an existing destination.
    pub async fn sftp_rename(&self, session: SessionId, from: &str, to: &str) -> Result<()> {
        let (from, to) = (from.to_owned(), to.to_owned());
        self.with_sftp(session, |c| async move { c.rename(&from, &to).await })
            .await
    }

    /// Removes a file, a symlink or an empty directory.
    pub async fn sftp_remove(&self, session: SessionId, path: &str) -> Result<()> {
        let path = path.to_owned();
        self.with_sftp(session, |c| async move { c.remove(&path).await })
            .await
    }

    /// Starts a transfer; progress and outcome arrive as
    /// [`EngineEvent::Sftp`] events carrying the returned id.
    pub async fn transfer(
        &self,
        session: SessionId,
        request: TransferRequest,
    ) -> Result<TransferId> {
        let (client, attached) = self.sftp(session).await?;
        let id = TransferId::next();
        // Under the session (closing it stops the transfer), never under the
        // SFTP client: one refused request must not cancel other transfers.
        let cancel = attached.ctx.cancel.child_token();
        self.inner
            .transfers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, cancel.clone());
        let engine = self.clone();
        tokio::spawn(async move {
            client.transfer(attached, id, request, cancel).await;
            engine
                .inner
                .transfers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        });
        Ok(id)
    }

    /// Cancels a transfer; its partial file is removed.
    pub fn cancel_transfer(&self, id: TransferId) -> Result<()> {
        self.inner
            .transfers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .ok_or(crate::sftp::SftpError::UnknownTransfer(id))?
            .cancel();
        Ok(())
    }

    // ── Tunnels ─────────────────────────────────────────────────────────

    /// Starts a `-L` / `-R` / `-D` tunnel on `session` (waits while it connects).
    /// `Listening` or `Failed` follows on the event bus.
    /// A spec reachable from other machines ([`ForwardSpec::is_exposed`])
    /// is refused unless `allow_exposed` (the UI asked the user).
    pub async fn start_tunnel(
        &self,
        session: SessionId,
        spec: ForwardSpec,
        allow_exposed: bool,
    ) -> Result<TunnelId> {
        if spec.is_exposed() && !allow_exposed {
            return Err(crate::forward::TunnelError::ExposedNotConfirmed {
                bind: spec.bind().to_string(),
            }
            .into());
        }
        let attached = self.attach(session).await?;
        let id = TunnelId::next();
        let cancel = attached.ctx.cancel.child_token();
        self.inner
            .tunnels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                id,
                (
                    TunnelInfo {
                        id,
                        session,
                        spec: spec.clone(),
                    },
                    cancel.clone(),
                ),
            );
        attached
            .ctx
            .log(LogLevel::Info, "tunnel", format!("starting {spec}"));
        tokio::spawn(crate::forward::run(attached, id, spec, cancel));
        Ok(id)
    }

    pub fn stop_tunnel(&self, id: TunnelId) -> Result<()> {
        self.inner
            .tunnels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .ok_or(crate::forward::TunnelError::UnknownTunnel(id))?
            .1
            .cancel();
        Ok(())
    }

    /// Running tunnels (all sessions).
    pub fn tunnels(&self) -> Vec<TunnelInfo> {
        let mut list: Vec<TunnelInfo> = self
            .inner
            .tunnels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|(info, _)| info.clone())
            .collect();
        list.sort_by_key(|t| t.id);
        list
    }

    pub(crate) fn forget_tunnel(&self, id: TunnelId) {
        self.inner
            .tunnels
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// Log lines recorded for a session (debug panel), live or among the
    /// last [`ENDED_SESSIONS_KEPT`] ended ones.
    pub fn session_logs(&self, id: SessionId) -> Result<Vec<LogRecord>> {
        let sessions = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let entry = sessions.get(&id).ok_or(Error::UnknownSession(id))?;
        let ring = entry.logs.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(ring.iter().cloned().collect())
    }

    /// Alias a session was opened for.
    /// What a session was opened for (alias and user override), to open
    /// another one to the same account.
    pub(crate) fn session_target(&self, id: SessionId) -> Option<ConnectRequest> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|e| {
                let mut request = ConnectRequest::new(e.alias.clone());
                request.user_override.clone_from(&e.user_override);
                request
            })
    }

    /// The token that ends a session (and everything it runs).
    pub(crate) fn session_cancel(&self, id: SessionId) -> Option<CancellationToken> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|e| e.cancel.clone())
    }

    pub fn session_alias(&self, id: SessionId) -> Option<String> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .map(|e| e.alias.clone())
    }

    /// Asks the user a question and waits for the answer, or for `cancel`.
    pub(crate) async fn ask(
        &self,
        session: SessionId,
        kind: PromptKind,
        cancel: &CancellationToken,
    ) -> Result<PromptResponse> {
        let id = PromptId::next();
        let rx = self.register_prompt(id);
        // Withdraws the prompt however this future ends (cancelled, or
        // dropped because the whole connection attempt was aborted).
        let _withdraw = WithdrawPrompt { engine: self, id };
        self.emit(EngineEvent::Prompt(Prompt {
            id,
            session: Some(session),
            kind,
        }));
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(Error::Cancelled),
            answer = rx => answer.map_err(|_| Error::EngineClosed),
        }
    }

    /// Answers a pending prompt. Errors if it was already answered/cancelled.
    pub fn respond(&self, id: PromptId, response: PromptResponse) -> Result<()> {
        let sender = self
            .inner
            .prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)
            .ok_or(Error::UnknownPrompt(id))?;
        // The waiting task may have been cancelled meanwhile; that is fine.
        let _ = sender.send(response);
        self.emit(EngineEvent::PromptResolved { id });
        Ok(())
    }

    /// Registers a prompt and returns the receiver the asking task awaits.
    pub(crate) fn register_prompt(&self, id: PromptId) -> oneshot::Receiver<PromptResponse> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, tx);
        rx
    }

    /// Publishes a control-plane event. No subscriber is not an error.
    pub(crate) fn emit(&self, event: EngineEvent) {
        let _ = self.inner.events.send(event);
    }

    /// Cancels every session and background task.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
    }

    /// [`Engine::shutdown`], then waits up to `limit` for the sessions to
    /// close cleanly (channel EOF/CLOSE, SSH DISCONNECT). Returns whether
    /// they all did. For app exit.
    pub async fn shutdown_and_wait(&self, limit: std::time::Duration) -> bool {
        self.shutdown();
        self.inner.tasks.close();
        tokio::time::timeout(limit, self.inner.tasks.wait())
            .await
            .is_ok()
    }
}

// ════ Phase 3: vault and snippets (additive block) ═══════════════════════
//
// Every operation here is blocking underneath (Argon2, file I/O) and runs on
// `spawn_blocking`, so no async task ever waits on it.

impl Engine {
    /// State of the fallback vault: [`VaultStatus::NotInUse`] when the
    /// system keyring is the secret store.
    ///
    /// [`VaultStatus::NotInUse`]: crate::vault::VaultStatus::NotInUse
    pub async fn vault_status(&self) -> Result<crate::vault::VaultStatus> {
        let store = Arc::clone(&self.inner.config.secrets);
        tokio::task::spawn_blocking(move || {
            store.vault().map_or(
                crate::vault::VaultStatus::NotInUse,
                crate::vault::VaultStore::status,
            )
        })
        .await
        .map_err(|_| Error::EngineClosed)
    }

    /// Creates the vault (refused if one exists) and leaves it unlocked.
    pub async fn vault_create(&self, passphrase: SecretString) -> Result<()> {
        self.with_vault(move |vault| vault.create_in_place(&passphrase))
            .await
    }

    /// Unlocks the vault: remembered secrets become readable and new ones
    /// can be remembered. While it is locked, sessions still connect; they
    /// only skip remembering (and say so in the session log).
    pub async fn vault_unlock(&self, passphrase: SecretString) -> Result<()> {
        self.with_vault(move |vault| vault.unlock_in_place(&passphrase))
            .await
    }

    /// Forgets the vault key and every decrypted secret.
    pub async fn vault_lock(&self) -> Result<()> {
        self.with_vault(|vault| {
            vault.lock();
            Ok(())
        })
        .await
    }

    /// Re-keys the vault under a new master passphrase.
    pub async fn vault_change_passphrase(
        &self,
        old: SecretString,
        new: SecretString,
    ) -> Result<()> {
        self.with_vault(move |vault| vault.change_passphrase(&old, &new))
            .await
    }

    async fn with_vault<T, F>(&self, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&crate::vault::VaultStore) -> Result<T> + Send + 'static,
    {
        let store = Arc::clone(&self.inner.config.secrets);
        tokio::task::spawn_blocking(move || {
            let vault = store.vault().ok_or(crate::vault::VaultError::NotInUse)?;
            op(vault)
        })
        .await
        .map_err(|_| Error::EngineClosed)?
    }

    /// Every snippet of `snippets.toml`, in file order.
    pub async fn snippets(&self) -> Result<Vec<crate::snippets::Snippet>> {
        let store = Arc::clone(&self.inner.snippets);
        tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Creates or updates (by id) a snippet after validation; an empty id
    /// is derived from the name. Returns the snippet as stored.
    pub async fn save_snippet(
        &self,
        snippet: crate::snippets::Snippet,
    ) -> Result<crate::snippets::Snippet> {
        let store = Arc::clone(&self.inner.snippets);
        tokio::task::spawn_blocking(move || store.save(snippet))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    pub async fn delete_snippet(&self, id: &str) -> Result<()> {
        let store = Arc::clone(&self.inner.snippets);
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.delete(&id))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Snippets whose scope covers `host_alias` (global ones, those naming
    /// the alias, those sharing one of its inventory tags). An alias absent
    /// from the inventory has no tags.
    pub async fn snippets_for(&self, host_alias: &str) -> Result<Vec<crate::snippets::Snippet>> {
        let inventory = self.load_hosts().await?;
        let tags = inventory
            .hosts
            .iter()
            .find(|h| h.alias == host_alias)
            .map(|h| h.meta.tags.clone())
            .unwrap_or_default();
        Ok(self
            .snippets()
            .await?
            .into_iter()
            .filter(|s| s.applies_to_alias(host_alias, &tags))
            .collect())
    }
}

// ════ end Phase 3 ════════════════════════════════════════════════════════

// ════ Phase 4: metadata, recent sessions, effective config, debug logs,
// settings (additive block) ══════════════════════════════════════════════
//
// File operations run on `spawn_blocking` under an inter-process lock (see
// `hosts`, `recent`, `settings`); none of them can block a session.

impl Engine {
    /// Marks `alias` as a favorite (or not) in `hosts.toml`.
    pub async fn set_favorite(
        &self,
        alias: &str,
        favorite: bool,
    ) -> Result<crate::hosts::HostMeta> {
        self.edit_meta(alias, crate::hosts::MetaChange::Favorite(favorite))
            .await
    }

    /// Replaces the tags of `alias` (normalized: see [`crate::hosts`]).
    pub async fn set_tags(&self, alias: &str, tags: Vec<String>) -> Result<crate::hosts::HostMeta> {
        self.edit_meta(alias, crate::hosts::MetaChange::Tags(tags))
            .await
    }

    /// Sets or clears (`None` or blank) the group of `alias`.
    pub async fn set_group(
        &self,
        alias: &str,
        group: Option<String>,
    ) -> Result<crate::hosts::HostMeta> {
        self.edit_meta(alias, crate::hosts::MetaChange::Group(group))
            .await
    }

    /// Sets or clears (`None` or blank) the notes of `alias`.
    pub async fn set_notes(
        &self,
        alias: &str,
        notes: Option<String>,
    ) -> Result<crate::hosts::HostMeta> {
        self.edit_meta(alias, crate::hosts::MetaChange::Notes(notes))
            .await
    }

    /// One `hosts.toml` edit, then [`EngineEvent::HostMeta`].
    async fn edit_meta(
        &self,
        alias: &str,
        change: crate::hosts::MetaChange,
    ) -> Result<crate::hosts::HostMeta> {
        let path = self.inner.config.paths.hosts_meta();
        let key = alias.to_owned();
        let meta =
            tokio::task::spawn_blocking(move || crate::hosts::update_meta(&path, &key, change))
                .await
                .map_err(|_| Error::EngineClosed)??;
        self.emit(EngineEvent::HostMeta {
            alias: alias.to_owned(),
            meta: meta.clone(),
        });
        Ok(meta)
    }

    /// The `limit` most recently connected hosts, most recent first.
    pub async fn recent(&self, limit: usize) -> Result<Vec<crate::recent::RecentHost>> {
        let path = self.inner.config.paths.recent();
        tokio::task::spawn_blocking(move || crate::recent::list(&path, limit))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Empties the recent-sessions list.
    pub async fn clear_recent(&self) -> Result<()> {
        let path = self.inner.config.paths.recent();
        tokio::task::spawn_blocking(move || crate::recent::clear(&path))
            .await
            .map_err(|_| Error::EngineClosed)??;
        self.emit(EngineEvent::RecentChanged);
        Ok(())
    }

    /// Removes `alias` from the recent sessions; `false` if it was absent.
    pub async fn forget_recent(&self, alias: &str) -> Result<bool> {
        let path = self.inner.config.paths.recent();
        let alias = alias.to_owned();
        let removed = tokio::task::spawn_blocking(move || crate::recent::forget(&path, &alias))
            .await
            .map_err(|_| Error::EngineClosed)??;
        if removed {
            self.emit(EngineEvent::RecentChanged);
        }
        Ok(removed)
    }

    /// Records a connected session in the background: a failure is only
    /// logged (session log + tracing), the session never waits for it.
    pub(crate) fn note_recent(&self, ctx: &SessionCtx, alias: &str) {
        let path = self.inner.config.paths.recent();
        let alias = alias.to_owned();
        let ctx = ctx.clone();
        let engine = self.clone();
        tokio::spawn(async move {
            let at = time::OffsetDateTime::now_utc();
            let written =
                tokio::task::spawn_blocking(move || crate::recent::record(&path, &alias, at))
                    .await
                    .map_err(|_| Error::EngineClosed)
                    .and_then(|r| r);
            match written {
                Ok(()) => engine.emit(EngineEvent::RecentChanged),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot record the recent session");
                    ctx.log(
                        LogLevel::Warn,
                        "recent",
                        format!("not added to recent sessions: {}", e.summary().message),
                    );
                }
            }
        });
    }

    /// The `ssh -G` equivalent for `alias`: every effective option with
    /// its origin, Portico's defaults, and the resolved `ProxyJump` route.
    pub async fn effective_config(&self, alias: &str) -> Result<crate::effective::EffectiveConfig> {
        let config = self.inner.config.clone();
        let alias = alias.to_owned();
        tokio::task::spawn_blocking(move || {
            let ssh = SshConfig::load(&config.paths.ssh_config(), &config.paths.ssh_dir)?;
            Ok(crate::effective::compute(&ssh, &config, &alias, None))
        })
        .await
        .map_err(|_| Error::EngineClosed)?
    }

    /// Sessions whose logs [`Engine::session_logs`] can return: live ones
    /// and the last [`ENDED_SESSIONS_KEPT`] ended ones, oldest first.
    pub fn sessions_with_logs(&self) -> Vec<crate::logs::SessionLogInfo> {
        let mut list: Vec<crate::logs::SessionLogInfo> = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(id, entry)| crate::logs::SessionLogInfo {
                id: *id,
                alias: entry.alias.clone(),
                probe: entry.probe,
                started: entry.started,
                ended: entry.ended.as_ref().map(|(at, _)| *at),
                outcome: entry.ended.as_ref().map(|(_, outcome)| outcome.clone()),
            })
            .collect();
        list.sort_by_key(|info| info.id);
        list
    }

    /// Called when a session actor returns: keeps its logs and forgets
    /// the oldest ended sessions beyond [`ENDED_SESSIONS_KEPT`].
    fn session_finished(&self, id: SessionId, outcome: crate::logs::SessionOutcome) {
        let now = time::OffsetDateTime::now_utc();
        let forgotten: Vec<SessionId> = {
            let mut sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = sessions.get_mut(&id) {
                entry.ended = Some((now, outcome));
            }
            let mut ended: Vec<(time::OffsetDateTime, SessionId)> = sessions
                .iter()
                .filter_map(|(id, e)| e.ended.as_ref().map(|(at, _)| (*at, *id)))
                .collect();
            ended.sort();
            let excess = ended.len().saturating_sub(ENDED_SESSIONS_KEPT);
            let forgotten: Vec<SessionId> =
                ended.into_iter().take(excess).map(|(_, id)| id).collect();
            for id in &forgotten {
                sessions.remove(id);
            }
            forgotten
        };
        let mut sftp = self
            .inner
            .sftp
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for id in &forgotten {
            sftp.remove(id);
        }
    }

    /// `settings.toml` (defaults when missing; an invalid file is an error
    /// and the UIs fall back to their defaults).
    pub async fn settings(&self) -> Result<crate::settings::Settings> {
        let path = self.inner.config.paths.settings();
        tokio::task::spawn_blocking(move || crate::settings::load(&path))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Validates and saves `settings` (other keys of the file are kept);
    /// returns them as stored.
    pub async fn save_settings(
        &self,
        settings: crate::settings::Settings,
    ) -> Result<crate::settings::Settings> {
        let path = self.inner.config.paths.settings();
        tokio::task::spawn_blocking(move || crate::settings::save(&path, &settings))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Remembers the theme without touching the rest of `settings.toml`
    /// (read and write under one lock: two UIs never lose each other's
    /// changes).
    pub async fn set_theme(&self, theme: Option<String>) -> Result<crate::settings::Settings> {
        let path = self.inner.config.paths.settings();
        tokio::task::spawn_blocking(move || crate::settings::set_theme(&path, theme.as_deref()))
            .await
            .map_err(|_| Error::EngineClosed)?
    }
}

// ════ end Phase 4 ════════════════════════════════════════════════════════

/// Removes a pending prompt and tells the UIs, when the asking side stops
/// waiting for any reason.
struct WithdrawPrompt<'a> {
    engine: &'a Engine,
    id: PromptId,
}

impl Drop for WithdrawPrompt<'_> {
    fn drop(&mut self) {
        let pending = self
            .engine
            .inner
            .prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id)
            .is_some();
        // Answered prompts were already resolved by `respond`.
        if pending {
            self.engine
                .emit(EngineEvent::PromptResolved { id: self.id });
        }
    }
}

// ════ Phase 5: imports (additive block) ═══════════════════════════════════

impl Engine {
    /// Reads a Termius export and returns what importing it would do.
    /// Nothing is written (see [`crate::import`]).
    pub async fn plan_termius_import(
        &self,
        path: impl Into<PathBuf>,
        options: crate::import::ImportOptions,
    ) -> Result<crate::import::ImportPlan> {
        let paths = self.inner.config.paths.clone();
        let path = path.into();
        tokio::task::spawn_blocking(move || crate::import::termius::plan(&paths, &path, options))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Writes `plan`: `<config_dir>/imports/termius.conf` (replaced in
    /// full) and the metadata in `hosts.toml` (merged). Emits
    /// [`EngineEvent::HostMeta`] for each alias whose metadata changed;
    /// UIs reload the inventory afterwards (new aliases appear once
    /// `~/.ssh/config` includes the file, see [`crate::import::ImportReport`]).
    pub async fn apply_import(
        &self,
        plan: crate::import::ImportPlan,
    ) -> Result<crate::import::ImportReport> {
        let paths = self.inner.config.paths.clone();
        let now = time::OffsetDateTime::now_utc();
        let (report, changed) =
            tokio::task::spawn_blocking(move || crate::import::apply(&paths, &plan, now))
                .await
                .map_err(|_| Error::EngineClosed)??;
        for (alias, meta) in changed {
            self.emit(EngineEvent::HostMeta { alias, meta });
        }
        Ok(report)
    }
}

// ════ end Phase 5 ════════════════════════════════════════════════════════

// ════ Phase 6: session recording (additive block) ════════════════════════
//
// The file side lives in `crate::recording`; here: the terminal registry and
// the public commands. Short file I/O runs on `spawn_blocking`; each
// recording's writer runs on its own `portico-rec` thread (never a blocking
// slot of the runtime), whose end is tracked so `shutdown_and_wait` waits
// for the flush.

impl Engine {
    /// Called by a terminal when it opens: makes it recordable.
    pub(crate) fn register_terminal(
        &self,
        id: TerminalId,
        session: SessionId,
        tap: &Arc<crate::recording::TerminalTap>,
    ) {
        self.inner
            .terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, (session, Arc::downgrade(tap)));
    }

    /// Called by a terminal when it ends.
    pub(crate) fn unregister_terminal(&self, id: TerminalId) {
        self.inner
            .terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    fn terminal_tap(
        &self,
        terminal: TerminalId,
    ) -> Result<(SessionId, Arc<crate::recording::TerminalTap>)> {
        self.inner
            .terminals
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&terminal)
            .and_then(|(session, tap)| Some((*session, tap.upgrade()?)))
            .ok_or(Error::Recording(
                crate::recording::RecordingError::UnknownTerminal(terminal),
            ))
    }

    /// Adds a line to a session's debug log (no-op for an unknown session).
    fn session_log(&self, session: SessionId, level: LogLevel, target: &str, message: String) {
        let logs = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .map(|entry| Arc::clone(&entry.logs));
        let record = LogRecord {
            at: time::OffsetDateTime::now_utc(),
            level,
            session: Some(session),
            target: target.to_owned(),
            message,
        };
        if let Some(logs) = logs {
            logs.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(record.clone());
        }
        self.emit(EngineEvent::Log(record));
    }

    /// Starts recording the output of `terminal` (asciicast v2, opt-in).
    /// Emits [`RecordingState::Started`], and later exactly one
    /// [`RecordingState::Stopped`] (stop requested, terminal closed, writer
    /// behind, write error or size cap).
    ///
    /// Cancel-safe: the start runs as a tracked task. If the caller stops
    /// waiting before the file exists, the file is removed and the terminal
    /// released; once the recording is active (`Started` emitted) it stays
    /// active.
    ///
    /// Errors: `recording_unknown_terminal`, `recording_already_active`,
    /// `recording_unsafe_path` (symlinked folder), `recording_foreign_owner`,
    /// `recording_create_failed`.
    ///
    /// [`RecordingState::Started`]: crate::events::RecordingState::Started
    /// [`RecordingState::Stopped`]: crate::events::RecordingState::Stopped
    pub async fn start_recording(
        &self,
        terminal: TerminalId,
    ) -> Result<crate::recording::RecordingInfo> {
        if self.inner.shutdown.is_cancelled() {
            return Err(Error::EngineClosed);
        }
        let (session, tap) = self.terminal_tap(terminal)?;
        // Released when dropped, whatever ends the start (error, cancel).
        let reservation = tap.begin(terminal).map_err(Error::Recording)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let engine = self.clone();
        self.inner.tasks.spawn(async move {
            let result = engine
                .run_start(terminal, session, reservation, &reply_tx)
                .await;
            let _ = reply_tx.send(result);
        });
        reply_rx.await.map_err(|_| Error::EngineClosed)?
    }

    /// The body of [`Engine::start_recording`], on a tracked task.
    async fn run_start(
        &self,
        terminal: TerminalId,
        session: SessionId,
        reservation: crate::recording::StartReservation,
        reply: &oneshot::Sender<Result<crate::recording::RecordingInfo>>,
    ) -> Result<crate::recording::RecordingInfo> {
        use crate::recording::{
            MAX_RECORDING_BYTES, NewRecording, RECORDING_QUEUE_CAPACITY, RecordingError,
            RecordingInfo, StopCell,
        };

        let size = reservation.size();
        let alias = self
            .session_alias(session)
            .unwrap_or_else(|| "session".to_owned());
        let dir = self.recordings_dir();
        let started = time::OffsetDateTime::now_utc();
        let NewRecording { name, path, writer } = tokio::task::spawn_blocking(move || {
            crate::recording::create_recording(&dir, &alias, started, size, MAX_RECORDING_BYTES)
        })
        .await
        .map_err(|_| Error::EngineClosed)??;
        if reply.is_closed() {
            // The caller stopped waiting: no recording it would not know of.
            // The file goes first, then the reservation (on return).
            drop(writer);
            remove_orphan(path).await;
            return Err(Error::EngineClosed);
        }

        let (tx, rx) = mpsc::channel(RECORDING_QUEUE_CAPACITY);
        let stop = Arc::new(StopCell::default());
        let path_text = path.display().to_string();
        let job = RecordingJob {
            terminal,
            session,
            name: name.clone(),
            path: path.clone(),
            path_text: path_text.clone(),
            active: ActiveName::register(&self.inner.recordings_active, &name),
        };
        // A dedicated thread per recording: it lives as long as the
        // recording, so it must not hold one of the runtime's blocking slots.
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let engine = self.clone();
        let job_stop = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name(RECORDING_THREAD_NAME.to_owned())
            .spawn(move || {
                job.run(&engine, rx, writer, &job_stop);
                drop(done_tx);
            });
        if let Err(source) = spawned {
            // The closure (file, job) is dropped with the error.
            remove_orphan(path.clone()).await;
            return Err(Error::Recording(RecordingError::Create { path, source }));
        }
        // `shutdown_and_wait` waits for the writer to flush and close.
        self.inner.tasks.spawn(async move {
            let _ = done_rx.await;
        });

        let info = RecordingInfo {
            terminal,
            session,
            name: name.clone(),
            path,
            started,
        };
        self.emit(EngineEvent::Recording {
            terminal,
            session,
            state: crate::events::RecordingState::Started {
                name,
                path: path_text.clone(),
            },
        });
        self.session_log(
            session,
            LogLevel::Info,
            "recording",
            format!("terminal {terminal}: recording to {path_text}"),
        );
        // A terminal closed meanwhile drops `tx`: the writer then ends at
        // once with `terminal_closed`, after the header.
        reservation.activate(tx, stop);
        Ok(info)
    }

    /// Stops the recording of `terminal` (`requested`); the file is flushed
    /// and closed by its writer, then `Stopped` is emitted.
    /// Errors: `recording_unknown_terminal`, `recording_not_active`.
    pub fn stop_recording(&self, terminal: TerminalId) -> Result<()> {
        let (_, tap) = self.terminal_tap(terminal)?;
        if tap.stop(crate::recording::RecordingStopReason::Requested) {
            Ok(())
        } else {
            Err(Error::Recording(
                crate::recording::RecordingError::NotRecording(terminal),
            ))
        }
    }

    /// Whether `terminal` is being recorded right now.
    pub fn is_recording(&self, terminal: TerminalId) -> bool {
        self.terminal_tap(terminal)
            .is_ok_and(|(_, tap)| tap.is_recording())
    }

    /// The recordings folder (`<data_dir>/recordings`).
    pub fn recordings_dir(&self) -> PathBuf {
        self.inner.config.paths.recordings()
    }

    /// Recording files, newest first (header, size, duration).
    pub async fn list_recordings(&self) -> Result<Vec<crate::recording::RecordingEntry>> {
        let dir = self.recordings_dir();
        tokio::task::spawn_blocking(move || crate::recording::list(&dir))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Deletes the recording `name` (a file name from the listing). Paths,
    /// `..`, symlinks and anything outside the recordings folder are refused,
    /// and so is a recording still being written (`recording_active`: stop
    /// it first).
    pub async fn delete_recording(&self, name: &str) -> Result<()> {
        let active = self
            .inner
            .recordings_active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(name);
        if active {
            return Err(Error::Recording(crate::recording::RecordingError::Active {
                name: name.to_owned(),
            }));
        }
        let dir = self.recordings_dir();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || crate::recording::delete(&dir, &name))
            .await
            .map_err(|_| Error::EngineClosed)?
    }

    /// Opens the recording `name` for playback: header validated, events
    /// streamed by the returned reader (blocking reads: iterate it on a
    /// blocking thread). Output is untrusted: pass it through
    /// [`crate::recording::PlaybackSanitizer`].
    pub async fn open_recording(&self, name: &str) -> Result<crate::recording::RecordingReader> {
        let dir = self.recordings_dir();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || crate::recording::open(&dir, &name))
            .await
            .map_err(|_| Error::EngineClosed)?
    }
}

/// Name of the writer thread of a recording.
const RECORDING_THREAD_NAME: &str = "portico-rec";

/// Removes a recording file that will not be used (best effort, off the
/// runtime).
async fn remove_orphan(path: PathBuf) {
    let removed = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
    if !matches!(removed, Ok(Ok(()))) {
        tracing::warn!("cannot remove an abandoned recording file");
    }
}

/// A recording name registered as being written (RAII: unregistered on
/// drop).
#[derive(Debug)]
struct ActiveName {
    set: Arc<Mutex<std::collections::HashSet<String>>>,
    name: String,
}

impl ActiveName {
    fn register(set: &Arc<Mutex<std::collections::HashSet<String>>>, name: &str) -> Self {
        set.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned());
        Self {
            set: Arc::clone(set),
            name: name.to_owned(),
        }
    }
}

impl Drop for ActiveName {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.name);
    }
}

/// The writer side of one recording (runs on its own thread).
#[derive(Debug)]
struct RecordingJob {
    terminal: TerminalId,
    session: SessionId,
    name: String,
    path: PathBuf,
    path_text: String,
    active: ActiveName,
}

impl RecordingJob {
    fn run(
        self,
        engine: &Engine,
        mut rx: mpsc::Receiver<crate::recording::TapEvent>,
        mut writer: crate::recording::CastWriter<std::fs::File>,
        stop: &crate::recording::StopCell,
    ) {
        use crate::recording::{RecordingStopReason as Reason, WriteStop};

        let Self {
            terminal,
            session,
            name,
            path,
            path_text,
            active,
        } = self;
        let failed = crate::recording::run_writer(&mut rx, &mut writer);
        drop(rx);
        let duration = writer.last_time();
        let finished = match failed {
            Some(WriteStop::Io(e)) => Err(WriteStop::Io(e)),
            Some(WriteStop::SizeLimit) => {
                // The cap: what fits is written (the decoder's tail may not).
                let _ = writer.finish_in_place();
                Err(WriteStop::SizeLimit)
            }
            None => writer.finish_in_place(),
        };
        let reason = match finished {
            Err(WriteStop::Io(_)) => {
                // Disk full or I/O error: no flush retried, the file is cut
                // back to its complete lines.
                if crate::recording::truncate_to_committed(writer).is_err() {
                    tracing::warn!("cannot cut a failed recording back to complete lines");
                }
                Reason::WriteFailed
            }
            Err(stopped) => {
                // The cap: the file ends on a complete line.
                let _ = writer.into_sink().sync_all();
                stopped.reason()
            }
            Ok(()) => match writer.into_sink().sync_all() {
                Ok(()) => stop.get().unwrap_or(Reason::TerminalClosed),
                Err(_) => Reason::WriteFailed,
            },
        };
        let bytes = std::fs::symlink_metadata(&path).map_or(0, |m| m.len());
        // The file is closed: it may be deleted from now on.
        drop(active);
        let duration = Duration::try_from_secs_f64(duration).unwrap_or(Duration::ZERO);
        let level = if matches!(reason, Reason::Requested | Reason::TerminalClosed) {
            LogLevel::Info
        } else {
            LogLevel::Warn
        };
        engine.session_log(
            session,
            level,
            "recording",
            format!(
                "terminal {terminal}: recording stopped ({}), {bytes} bytes, {:.1} s",
                reason.code(),
                duration.as_secs_f64()
            ),
        );
        engine.emit(EngineEvent::Recording {
            terminal,
            session,
            state: crate::events::RecordingState::Stopped {
                reason,
                name,
                path: path_text,
                bytes,
                duration,
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(root: &std::path::Path) -> Engine {
        Engine::new(EngineConfig {
            paths: Paths::rooted(root),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: Arc::new(crate::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            keepalive: DEFAULT_KEEPALIVE,
        })
    }

    #[tokio::test]
    async fn loads_hosts_with_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
        std::fs::create_dir_all(root.join("config")).expect("mkdir");
        std::fs::write(root.join(".ssh/config"), "Host prod\n  HostName 10.0.0.1\n").expect("w");
        std::fs::write(
            root.join("config/hosts.toml"),
            "[hosts.prod]\ntags = [\"prod\"]\nfavorite = true\n",
        )
        .expect("w");

        let inv = engine(root).load_hosts().await.expect("load");
        assert_eq!(inv.hosts.len(), 1);
        assert_eq!(inv.hosts[0].resolved.hostname, "10.0.0.1");
        assert!(inv.hosts[0].meta.favorite);
    }

    #[tokio::test]
    async fn prompt_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine(dir.path());
        let mut events = engine.subscribe();
        let id = PromptId::next();
        let rx = engine.register_prompt(id);

        engine.respond(id, PromptResponse::Cancel).expect("respond");
        assert!(matches!(rx.await, Ok(PromptResponse::Cancel)));
        assert!(
            matches!(events.recv().await, Ok(EngineEvent::PromptResolved { id: got }) if got == id)
        );
        assert!(matches!(
            engine.respond(id, PromptResponse::Cancel),
            Err(Error::UnknownPrompt(_))
        ));
    }

    // ── Phase 3: vault and snippets ──

    fn vault_engine(root: &std::path::Path) -> Engine {
        let paths = Paths::rooted(root);
        Engine::new(EngineConfig {
            secrets: Arc::new(crate::vault::VaultStore::open_for_tests(paths.vault())),
            ..engine(root).config().clone()
        })
    }

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    #[tokio::test]
    async fn vault_lifecycle() {
        use crate::secrets::SecretKey;
        use crate::vault::VaultStatus;
        use secrecy::ExposeSecret;

        let dir = tempfile::tempdir().expect("tempdir");
        let engine = vault_engine(dir.path());
        assert_eq!(
            engine.vault_status().await.expect("status"),
            VaultStatus::Absent
        );
        let e = engine
            .vault_unlock(secret("correct horse"))
            .await
            .expect_err("missing");
        assert_eq!(e.summary().code, "vault_missing");

        engine
            .vault_create(secret("correct horse"))
            .await
            .expect("create");
        assert_eq!(
            engine.vault_status().await.expect("status"),
            VaultStatus::Unlocked
        );
        let e = engine
            .vault_create(secret("correct horse"))
            .await
            .expect_err("exists");
        assert_eq!(e.summary().code, "vault_exists");

        // The session code path: secrets go through `config().secrets`.
        let key = SecretKey::KeyPassphrase {
            fingerprint: "SHA256:k".into(),
        };
        let store = Arc::clone(&engine.config().secrets);
        store.set(&key, &secret("pw")).expect("set");

        engine.vault_lock().await.expect("lock");
        assert_eq!(
            engine.vault_status().await.expect("status"),
            VaultStatus::Locked
        );
        // Locked: lookups fail softly (the session treats it as "nothing
        // remembered") and remembering is refused with a typed error.
        assert_eq!(
            store.get(&key).expect_err("locked").summary().code,
            "vault_locked"
        );
        assert_eq!(
            store
                .set(&key, &secret("x"))
                .expect_err("locked")
                .summary()
                .code,
            "vault_locked"
        );

        let e = engine
            .vault_unlock(secret("wrong horse"))
            .await
            .expect_err("wrong");
        assert_eq!(e.summary().code, "vault_wrong_passphrase");
        engine
            .vault_unlock(secret("correct horse"))
            .await
            .expect("unlock");
        assert_eq!(
            store.get(&key).expect("get").expect("some").expose_secret(),
            "pw"
        );

        engine
            .vault_change_passphrase(secret("correct horse"), secret("battery staple"))
            .await
            .expect("rekey");
        engine.vault_lock().await.expect("lock");
        engine
            .vault_unlock(secret("battery staple"))
            .await
            .expect("unlock");
    }

    #[tokio::test]
    async fn vault_not_in_use_with_another_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine(dir.path());
        assert_eq!(
            engine.vault_status().await.expect("status"),
            crate::vault::VaultStatus::NotInUse
        );
        let e = engine
            .vault_unlock(secret("correct horse"))
            .await
            .expect_err("n/a");
        assert_eq!(e.summary().code, "vault_not_in_use");
    }

    #[tokio::test]
    async fn snippet_crud_and_scope() {
        use crate::snippets::{Snippet, SnippetScope};

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
        std::fs::create_dir_all(root.join("config")).expect("mkdir");
        std::fs::write(
            root.join(".ssh/config"),
            "Host prod\n  HostName 10.0.0.1\nHost dev\n",
        )
        .expect("w");
        std::fs::write(
            root.join("config/hosts.toml"),
            "[hosts.prod]\ntags = [\"Prod\"]\n",
        )
        .expect("w");
        let engine = engine(root);
        assert!(engine.snippets().await.expect("list").is_empty());

        let make = |name: &str, scope| Snippet {
            id: String::new(),
            name: name.into(),
            command: "uptime".into(),
            description: None,
            scope,
            run: true,
        };
        let global = engine
            .save_snippet(make("Up", SnippetScope::Global))
            .await
            .expect("save");
        assert_eq!(global.id, "up");
        let tagged = engine
            .save_snippet(make("Up", SnippetScope::Tags(vec!["prod".into()])))
            .await
            .expect("save");
        assert_eq!(tagged.id, "up-2");
        engine
            .save_snippet(make("Dev only", SnippetScope::Hosts(vec!["dev".into()])))
            .await
            .expect("save");

        let ids = |list: Vec<Snippet>| list.into_iter().map(|s| s.id).collect::<Vec<_>>();
        assert_eq!(
            ids(engine.snippets_for("prod").await.expect("for")),
            ["up", "up-2"]
        );
        assert_eq!(
            ids(engine.snippets_for("dev").await.expect("for")),
            ["up", "dev-only"]
        );
        assert_eq!(
            ids(engine.snippets_for("unknown").await.expect("for")),
            ["up"]
        );

        engine.delete_snippet("up-2").await.expect("delete");
        assert_eq!(engine.snippets().await.expect("list").len(), 2);
        let e = engine.delete_snippet("up-2").await.expect_err("gone");
        assert_eq!(e.summary().code, "snippet_not_found");
    }

    // ── Phase 4 ──

    #[tokio::test]
    async fn metadata_edits_emit_events_and_show_in_the_inventory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
        std::fs::write(root.join(".ssh/config"), "Host prod\n  HostName 10.0.0.1\n").expect("w");
        let engine = engine(root);
        let mut events = engine.subscribe();

        let meta = engine.set_favorite("prod", true).await.expect("favorite");
        assert!(meta.favorite);
        match events.recv().await.expect("event") {
            EngineEvent::HostMeta { alias, meta } => {
                assert_eq!(alias, "prod");
                assert!(meta.favorite);
            }
            other => panic!("unexpected {other:?}"),
        }
        engine
            .set_tags("prod", vec!["#DB".into(), "db".into()])
            .await
            .expect("tags");
        engine
            .set_group("prod", Some("Data".into()))
            .await
            .expect("group");
        let meta = engine
            .set_notes("prod", Some("on call: ops".into()))
            .await
            .expect("notes");
        assert_eq!(meta.tags, ["DB"]);
        let e = engine
            .set_group("prod", Some("x".repeat(65)))
            .await
            .expect_err("long");
        assert_eq!(e.summary().code, "host_group");

        let inv = engine.load_hosts().await.expect("load");
        let host = inv.hosts.first().expect("prod");
        assert_eq!(host.meta.group.as_deref(), Some("Data"));
        assert_eq!(host.meta.notes.as_deref(), Some("on call: ops"));
        assert!(root.join("config/hosts.toml").exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".ssh/config")).expect("read"),
            "Host prod\n  HostName 10.0.0.1\n",
            "the ssh config is never written"
        );
        let json = serde_json::to_string(&EngineEvent::HostMeta {
            alias: "prod".into(),
            meta: host.meta.clone(),
        })
        .expect("json");
        assert!(
            json.starts_with(r#"{"type":"host_meta","alias":"prod","meta":{"#),
            "{json}"
        );
    }

    #[tokio::test]
    async fn settings_and_effective_config_through_the_engine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
        std::fs::write(root.join(".ssh/config"), "Host p\n  Port 2200\n").expect("w");
        let engine = engine(root);

        assert_eq!(
            engine.settings().await.expect("defaults"),
            crate::settings::Settings::default()
        );
        let mut settings = crate::settings::Settings {
            theme: Some("basalt".into()),
            ..crate::settings::Settings::default()
        };
        settings.keymap.tui.insert("quit".into(), "q".into());
        let stored = engine.save_settings(settings.clone()).await.expect("save");
        assert_eq!(stored, settings);
        assert_eq!(engine.settings().await.expect("load"), settings);

        let view = engine.effective_config("p").await.expect("view");
        let port = view.options.iter().find(|o| o.key == "port").expect("port");
        assert_eq!(port.values, ["2200"]);
        assert_eq!(view.route.len(), 1);
    }

    // ── Phase 6: recording (security review) ──

    /// A recordable terminal without SSH: the tap a terminal task holds.
    fn fake_terminal(engine: &Engine) -> (TerminalId, Arc<crate::recording::TerminalTap>) {
        let id = TerminalId::next();
        let tap = Arc::new(crate::recording::TerminalTap::new(PtySize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        }));
        engine.register_terminal(id, SessionId::next(), &tap);
        (id, tap)
    }

    async fn wait_stopped(events: &mut broadcast::Receiver<EngineEvent>, terminal: TerminalId) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("event in time")
                .expect("event stream open");
            if let EngineEvent::Recording {
                terminal: t,
                state: crate::events::RecordingState::Stopped { .. },
                ..
            } = event
                && t == terminal
            {
                return;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_start_recording_releases_the_terminal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine(dir.path());
        let (id, tap) = fake_terminal(&engine);
        // Dropped right after its first poll, while the file is created.
        let cancelled = tokio::time::timeout(Duration::ZERO, engine.start_recording(id)).await;
        assert!(cancelled.is_err(), "the start was cancelled");
        // The terminal is released (never stuck in "starting")…
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let info = loop {
            match engine.start_recording(id).await {
                Ok(info) => break info,
                Err(e)
                    if e.summary().code == "recording_already_active"
                        && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(e) => panic!("the terminal stays reserved: {e}"),
            }
        };
        // …and the abandoned file is gone: only the new recording exists.
        let names: Vec<String> = engine
            .list_recordings()
            .await
            .expect("list")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, [info.name]);
        tap.close();
        assert!(engine.shutdown_and_wait(Duration::from_secs(5)).await);
    }

    #[test]
    fn many_recordings_do_not_starve_spawn_blocking() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let engine = engine(dir.path());
            let mut taps = Vec::new();
            for _ in 0..4 {
                let (id, tap) = fake_terminal(&engine);
                engine.start_recording(id).await.expect("start");
                taps.push(tap);
            }
            // As many recordings as blocking threads: the runtime still
            // has room for short blocking work.
            let listed = tokio::time::timeout(Duration::from_secs(1), engine.list_recordings())
                .await
                .expect("list_recordings answers within 1 s")
                .expect("list");
            assert_eq!(listed.len(), 4);
            for tap in &taps {
                tap.close();
            }
            assert!(engine.shutdown_and_wait(Duration::from_secs(5)).await);
        });
    }

    #[tokio::test]
    async fn deleting_an_active_recording_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = engine(dir.path());
        let mut events = engine.subscribe();
        let (id, tap) = fake_terminal(&engine);
        let info = engine.start_recording(id).await.expect("start");
        let err = engine
            .delete_recording(&info.name)
            .await
            .expect_err("still being written");
        assert_eq!(err.summary().code, "recording_active");
        assert!(info.path.exists());

        engine.stop_recording(id).expect("stop");
        wait_stopped(&mut events, id).await;
        engine
            .delete_recording(&info.name)
            .await
            .expect("deleted once stopped");
        assert!(!info.path.exists());
        tap.close();
    }
}
