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
use crate::hosts::{HostEntry, HostMetaFile, inventory};
use crate::ids::{PromptId, SessionId, TerminalId, TransferId, TunnelId};
use crate::logs::LogLevel;
use crate::logs::{LogRecord, LogRing};
use crate::secrets::{KeyringStore, SecretStore};
use crate::session::{self, Attached, SessionCommand, SessionCtx};
use crate::sftp::{RemoteEntry, SftpClient, TransferRequest};
use crate::ssh_config::{ConfigWarning, ResolveContext, SshConfig};
use crate::store::{Paths, load_toml};

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
    /// Platform defaults.
    pub fn discover() -> Result<Self> {
        Ok(Self {
            paths: Paths::discover()?,
            local_user: local_user(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
            secrets: Arc::new(KeyringStore),
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
}

#[derive(Debug)]
struct SessionEntry {
    alias: String,
    user_override: Option<String>,
    commands: mpsc::Sender<SessionCommand>,
    cancel: CancellationToken,
    logs: Arc<Mutex<LogRing>>,
}

impl Engine {
    /// Creates the engine. Must be called inside a Tokio runtime once sessions
    /// are implemented (they spawn tasks).
    pub fn new(config: EngineConfig) -> Self {
        let (events, _) = broadcast::channel(config.event_capacity.max(16));
        Self {
            inner: Arc::new(Inner {
                config,
                sftp: Mutex::default(),
                transfers: Mutex::default(),
                tunnels: Mutex::default(),
                events,
                prompts: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                shutdown: CancellationToken::new(),
                tasks: TaskTracker::new(),
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
            let meta: HostMetaFile = load_toml(&config.paths.config_dir.join("hosts.toml"))?;
            let hosts = inventory(&ssh, &meta, &config.resolve_context(None));
            let mut warnings = ssh.warnings().to_vec();
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
                },
            );
        let ctx = SessionCtx::new(self.clone(), id, cancel, logs);
        self.inner.tasks.spawn(session::run(ctx, request, rx));
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

    /// Starts a `-L` / `-R` tunnel on `session` (waits while it connects).
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
            let (ForwardSpec::Local { bind, .. } | ForwardSpec::Remote { bind, .. }) = &spec;
            return Err(crate::forward::TunnelError::ExposedNotConfirmed {
                bind: bind.to_string(),
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

    /// Log lines recorded for a session (debug panel).
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
}
