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
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use secrecy::SecretString;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::events::EngineEvent;
use crate::hosts::{HostEntry, HostMetaFile, inventory};
use crate::ids::{PromptId, SessionId, TerminalId};
use crate::ssh_config::{ConfigWarning, ResolveContext, SshConfig};
use crate::store::{Paths, load_toml};

/// Capacity of the control-plane broadcast channel. Slow subscribers get
/// `RecvError::Lagged` and must resync from [`Engine`] queries.
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;
/// Max in-flight PTY output chunks per terminal (data plane).
pub const TERMINAL_OUTPUT_CAPACITY: usize = 256;

/// Engine construction parameters.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub paths: Paths,
    pub local_user: String,
    pub event_capacity: usize,
}

impl EngineConfig {
    /// Platform defaults.
    pub fn discover() -> Result<Self> {
        Ok(Self {
            paths: Paths::discover()?,
            local_user: local_user(),
            event_capacity: DEFAULT_EVENT_CAPACITY,
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
    /// Open an interactive shell right away with this size.
    pub pty: Option<PtySize>,
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
    shutdown: CancellationToken,
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

    /// Opens an SSH session (Phase 1b: russh transport, `ProxyJump` chain,
    /// `known_hosts`, agent/cert/key/password auth, keepalive latency).
    #[allow(clippy::unused_async)] // async contract; the body lands in Phase 1b
    pub async fn connect(
        &self,
        request: ConnectRequest,
    ) -> Result<(SessionId, Option<TerminalHandle>)> {
        if self.inner.shutdown.is_cancelled() {
            return Err(Error::EngineClosed);
        }
        tracing::debug!(alias = %request.alias, "connect requested");
        Err(Error::Unsupported("SSH sessions (Phase 1b)"))
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
    #[allow(dead_code)] // used by the session layer (Phase 1b)
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
