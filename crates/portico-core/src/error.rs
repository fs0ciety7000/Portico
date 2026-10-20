//! Typed errors for the whole engine.
//!
//! Every module owns a precise error enum; [`Error`] aggregates them. UIs never
//! match on error strings: they use [`Error::summary`] which yields a stable
//! machine code, a human message and an optional actionable hint.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{PromptId, SessionId};
use crate::ssh_config::ConfigError;

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Top-level engine error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not (de)serialize {what}: {message}")]
    Serde { what: &'static str, message: String },

    #[error("secret store error: {0}")]
    Secrets(String),

    #[error("no home directory could be determined for the current user")]
    NoHomeDir,

    #[error("unknown session {0}")]
    UnknownSession(SessionId),

    #[error("unknown or already answered prompt {0}")]
    UnknownPrompt(PromptId),

    #[error("the engine has shut down")]
    EngineClosed,

    #[error("not supported yet: {0}")]
    Unsupported(&'static str),

    #[error("cannot resolve {host}: {source}")]
    Resolve {
        host: String,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot connect to {target}: {source}")]
    Connect {
        target: String,
        #[source]
        source: std::io::Error,
    },

    #[error("timed out while {phase} {target}")]
    Timeout { phase: &'static str, target: String },

    #[error(transparent)]
    HostKey(#[from] HostKeyError),

    #[error("authentication failed for {user}@{host} (tried: {tried})")]
    AuthFailed {
        user: String,
        host: String,
        tried: String,
    },

    #[error("ProxyJump loop detected: {chain}")]
    ProxyJumpCycle { chain: String },

    #[error("ProxyJump chain is longer than {max} hops")]
    ProxyJumpTooDeep { max: usize },

    #[error("ssh error with {target}: {message}")]
    Transport { target: String, message: String },

    #[error("operation cancelled")]
    Cancelled,

    #[error("session {0} ended before the request could be served")]
    SessionEnded(SessionId),

    #[error("the server refused to open a {what}")]
    TerminalRefused { what: &'static str },

    #[error("{host} closed the connection during authentication")]
    AuthDisconnected { host: String },

    #[error("invalid host name {host:?} for known_hosts")]
    InvalidHostName { host: String },

    #[error(transparent)]
    Sftp(#[from] crate::sftp::SftpError),

    #[error(transparent)]
    Tunnel(#[from] crate::forward::TunnelError),
}

/// Host key verification failures. Never recoverable by a click: see
/// `CLAUDE.md` §11.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostKeyError {
    #[error(
        "host key for {host} has CHANGED (now {actual}, expected {expected} per {}:{line}): possible man-in-the-middle attack",
        file.display()
    )]
    Changed {
        host: String,
        actual: String,
        expected: String,
        file: PathBuf,
        line: usize,
    },
    #[error(
        "{host} presented a new key type while known_hosts has {known}: refused under StrictHostKeyChecking accept-new/no"
    )]
    NewAlgorithm { host: String, known: String },
    #[error("host key for {host} was not trusted")]
    Rejected { host: String },
    #[error("host key for {host} is unknown and StrictHostKeyChecking is enabled")]
    UnknownStrict { host: String },
    #[error("host key for {host} is revoked ({}:{line})", file.display())]
    Revoked {
        host: String,
        file: PathBuf,
        line: usize,
    },
    #[error("invalid host certificate for {host}: {reason}")]
    InvalidCertificate { host: String, reason: String },
}

impl Error {
    /// Helper to build an [`Error::Io`] with the offending path attached.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Stable, serializable view of the error for UIs and event streams.
    pub fn summary(&self) -> ErrorSummary {
        let (code, hint) = match self {
            Self::Config(_) => (
                "config",
                Some("Check the reported line in your ssh config."),
            ),
            Self::Io { .. } => ("io", None),
            Self::Serde { .. } => ("serde", Some("The Portico data file may be corrupted.")),
            Self::Secrets(_) => (
                "secrets",
                Some("Is a Secret Service / Keychain available and unlocked?"),
            ),
            Self::NoHomeDir => ("no_home", None),
            Self::UnknownSession(_) => ("unknown_session", None),
            Self::UnknownPrompt(_) => ("unknown_prompt", None),
            Self::EngineClosed => ("engine_closed", None),
            Self::Unsupported(_) => (
                "unsupported",
                Some("Portico refuses rather than connecting differently from ssh."),
            ),
            Self::Resolve { .. } => ("dns", Some("Check the HostName and your network/DNS.")),
            Self::Connect { source, .. } => (
                "connect",
                Some(match source.kind() {
                    std::io::ErrorKind::ConnectionRefused => {
                        "Nothing listens on that port: is sshd running, is the Port right?"
                    }
                    _ => "Check that the host is reachable (VPN, firewall, ProxyJump).",
                }),
            ),
            Self::Timeout { .. } => (
                "timeout",
                Some("The host did not answer in time; check ConnectTimeout and the network."),
            ),
            Self::HostKey(e) => match e {
                HostKeyError::Changed { .. } => (
                    "host_key_changed",
                    Some(
                        "Do not connect. Verify the new key with the server administrator; if legitimate, remove the old line from known_hosts.",
                    ),
                ),
                HostKeyError::NewAlgorithm { .. } => (
                    "host_key_new_algorithm",
                    Some(
                        "This host is known with another key type. Verify the new key with the administrator, then add it to known_hosts.",
                    ),
                ),
                HostKeyError::Rejected { .. } => ("host_key_rejected", None),
                HostKeyError::UnknownStrict { .. } => (
                    "host_key_unknown",
                    Some(
                        "Add the host key to known_hosts or relax StrictHostKeyChecking for this host.",
                    ),
                ),
                HostKeyError::Revoked { .. } => (
                    "host_key_revoked",
                    Some("This key was explicitly revoked: contact the server administrator."),
                ),
                HostKeyError::InvalidCertificate { .. } => (
                    "host_cert_invalid",
                    Some("The host certificate is expired or not issued for this name."),
                ),
            },
            Self::AuthFailed { .. } => (
                "auth_failed",
                Some("Check User, IdentityFile and your agent (ssh-add -l)."),
            ),
            Self::ProxyJumpCycle { .. } => (
                "proxy_jump_cycle",
                Some("Fix the ProxyJump chain in your ssh config."),
            ),
            Self::ProxyJumpTooDeep { .. } => ("proxy_jump_too_deep", None),
            Self::Transport { .. } => ("ssh", None),
            Self::Cancelled => ("cancelled", None),
            Self::SessionEnded(_) => ("session_ended", None),
            Self::TerminalRefused { .. } => (
                "terminal_refused",
                Some("The account may have no shell, or the server forbids PTYs (PermitTTY no)."),
            ),
            Self::AuthDisconnected { .. } => (
                "auth_disconnected",
                Some(
                    "Too many keys offered? Set IdentitiesOnly yes with a specific IdentityFile, or PreferredAuthentications.",
                ),
            ),
            Self::InvalidHostName { .. } => (
                "invalid_host",
                Some("HostName / HostKeyAlias must not contain spaces, commas or wildcards."),
            ),
            Self::Sftp(e) => e.code_and_hint(),
            Self::Tunnel(e) => e.code_and_hint(),
        };
        ErrorSummary {
            code: code.to_owned(),
            message: self.to_string(),
            hint: hint.map(str::to_owned),
        }
    }
}

/// UI-facing description of an error. Cheap to clone, safe to display and log
/// (must never contain secret material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorSummary {
    /// Stable `snake_case` code, suitable for i18n keys and tests.
    pub code: String,
    /// Human-readable, single-line message.
    pub message: String,
    /// Optional actionable advice shown under the message.
    pub hint: Option<String>,
}
