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

use crate::error::{Error, Result};
use crate::events::{EngineEvent, Prompt, PromptKind};
use crate::hosts::{HostEntry, HostMetaFile, inventory};
use crate::ids::{PromptId, SessionId, TerminalId};
use crate::logs::{LogRecord, LogRing};
use crate::secrets::{KeyringStore, SecretStore};
use crate::session::{self, SessionCommand, SessionCtx};
use crate::ssh_config::{ConfigWarning, ResolveContext, SshConfig};
use crate::store::{Paths, load_toml};

/// Capacity of the control-plane broadcast channel. Slow subscribers get
/// `RecvError::Lagged` and must resync from [`Engine`] queries.
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;
/// Max in-flight PTY output chunks per terminal (data plane).
pub const TERMINAL_OUTPUT_CAPACITY: usize = 256;

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
}

impl ConnectRequest {
    pub fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            user_override: None,
        }
    }
}

/// Input sent to a terminal (data plane).
#[derive(Debug)]
pub enum TerminalInput {
    Data(Bytes),
    Resize(PtySize),
    Close,
}

/// Data-plane endpoints of one PTY channel. Owned by exactly one UI view.
#[derive(Debug)]
pub struct TerminalHandle {
    pub id: TerminalId,
    pub session: SessionId,
    pub input: mpsc::Sender<TerminalInput>,
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
    events: broadcast::Sender<EngineEvent>,
    prompts: Mutex<HashMap<PromptId, oneshot::Sender<PromptResponse>>>,
    sessions: Mutex<HashMap<SessionId, SessionEntry>>,
    shutdown: CancellationToken,
}

#[derive(Debug)]
struct SessionEntry {
    alias: String,
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
                events,
                prompts: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                shutdown: CancellationToken::new(),
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
                    commands: tx,
                    cancel: cancel.clone(),
                    logs: Arc::clone(&logs),
                },
            );
        let ctx = SessionCtx::new(self.clone(), id, cancel, logs);
        tokio::spawn(session::run(ctx, request, rx));
        Ok(id)
    }

    /// Closes a session (or aborts it while connecting).
    pub async fn disconnect(&self, id: SessionId) -> Result<()> {
        let (tx, cancel) = {
            let sessions = self
                .inner
                .sessions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let entry = sessions.get(&id).ok_or(Error::UnknownSession(id))?;
            (entry.commands.clone(), entry.cancel.clone())
        };
        if tx.send(SessionCommand::Disconnect).await.is_err() {
            // The actor is still connecting or already gone: cancel it.
            cancel.cancel();
        }
        Ok(())
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
        self.emit(EngineEvent::Prompt(Prompt {
            id,
            session: Some(session),
            kind,
        }));
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                self.inner
                    .prompts
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                self.emit(EngineEvent::PromptResolved { id });
                Err(Error::Cancelled)
            }
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
