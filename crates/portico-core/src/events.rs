//! The engine's **control plane**: every state change is published as an
//! [`EngineEvent`] on a broadcast channel that any number of UIs can observe.
//!
//! High-volume byte streams (PTY output, file transfer chunks) do **not** go
//! through this channel: they use dedicated bounded channels (the *data
//! plane*, see [`crate::engine`]) so a slow UI can never make a terminal lag
//! behind control events, and control events can never be dropped because a
//! terminal is chatty.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ErrorSummary;
use crate::ids::{PromptId, SessionId, SftpId, TerminalId, TransferId, TunnelId};
use crate::logs::LogRecord;

/// Everything the engine announces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EngineEvent {
    Session {
        id: SessionId,
        event: SessionEvent,
    },
    Terminal {
        id: TerminalId,
        session: SessionId,
        event: TerminalEvent,
    },
    Tunnel {
        id: TunnelId,
        session: SessionId,
        event: TunnelEvent,
    },
    Sftp {
        id: SftpId,
        session: SessionId,
        event: SftpEvent,
    },
    /// The engine needs an answer from the user before it can continue.
    Prompt(Prompt),
    /// A prompt was answered (by any UI) or cancelled; UIs close their dialog.
    PromptResolved {
        id: PromptId,
    },
    Log(LogRecord),
}

/// Lifecycle of an SSH connection. UIs render this as the status badge.
///
/// ```text
/// Resolving → Connecting → Handshake → VerifyingHostKey → Authenticating → Connected
///      └───────────┴────────────┴──────────────┴─────────────────┴──→ Failed
/// Connected → Disconnecting → Closed
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionPhase {
    /// Resolving `ssh_config` + DNS.
    Resolving,
    /// TCP connect (or opening the next `ProxyJump` hop: `hop` is 1-based).
    Connecting {
        hop: u8,
        hops: u8,
        target: String,
    },
    /// SSH version exchange + key exchange.
    Handshake,
    /// Checking the server key against `known_hosts` (may raise a [`Prompt`]).
    VerifyingHostKey,
    /// Trying an authentication method.
    Authenticating {
        method: AuthMethod,
    },
    Connected,
    Disconnecting,
    Closed {
        reason: CloseReason,
    },
    Failed {
        error: ErrorSummary,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Agent,
    Certificate,
    PublicKey,
    KeyboardInteractive,
    Password,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    UserRequested,
    RemoteClosed,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A session was created for `alias` (first event of every session).
    Started {
        alias: String,
    },
    Phase(SessionPhase),
    /// Pre-authentication banner sent by the server.
    Banner {
        text: String,
    },
    /// The server presented a key that differs from `known_hosts`. Emitted
    /// right before `Phase(Failed)`; UIs show a blocking red warning with
    /// **no** option to accept.
    HostKeyChanged {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        expected_fingerprint: String,
        known_hosts_file: String,
        known_hosts_line: usize,
    },
    /// Round-trip time of an SSH keepalive (`keepalive@openssh.com`).
    Latency {
        rtt: Duration,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminalEvent {
    Opened,
    /// OSC title change requested by the remote program.
    Title(String),
    /// Remote process exited with `status`, or was killed by `signal`.
    Exited {
        status: Option<u32>,
        signal: Option<String>,
    },
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TunnelEvent {
    Listening { bind: String },
    ConnectionOpened { peer: String },
    ConnectionClosed { peer: String },
    Stopped,
    Failed { error: ErrorSummary },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SftpEvent {
    Ready {
        home: String,
    },
    TransferStarted {
        transfer: TransferId,
        name: String,
        total: Option<u64>,
    },
    TransferProgress {
        transfer: TransferId,
        done: u64,
        total: Option<u64>,
    },
    TransferFinished {
        transfer: TransferId,
    },
    TransferFailed {
        transfer: TransferId,
        error: ErrorSummary,
    },
    Closed,
}

/// A question the engine needs the user to answer.
///
/// The engine blocks the relevant operation (never the whole engine) until
/// [`crate::Engine::respond`] is called with the same [`PromptId`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub id: PromptId,
    pub session: Option<SessionId>,
    pub kind: PromptKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PromptKind {
    /// First connection to this host: Trust-On-First-Use decision.
    /// `other_algorithms` lists key types already known for this host (the UI
    /// warns: this is unusual).
    UnknownHostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        other_algorithms: Vec<String>,
    },
    /// `attempt` starts at 1; above 1 the previous answer was wrong.
    Password {
        user: String,
        host: String,
        attempt: u32,
        attempts: u32,
    },
    KeyPassphrase {
        key_path: String,
        attempt: u32,
        attempts: u32,
    },
    KeyboardInteractive {
        name: String,
        instructions: String,
        prompts: Vec<InteractivePrompt>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractivePrompt {
    pub text: String,
    pub echo: bool,
}
