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

    #[error("{0} is not supported yet")]
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

    #[error("the session ended before the request could be served")]
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

    #[error(transparent)]
    Key(#[from] crate::keys::KeyError),

    // ── Phase 3: vault and snippets ─────────────────────────────────────
    #[error(transparent)]
    Vault(#[from] crate::vault::VaultError),

    #[error(transparent)]
    Snippet(#[from] crate::snippets::SnippetError),
    // ── end Phase 3 ─────────────────────────────────────────────────────

    // ── Phase 4: metadata, settings, recent sessions ────────────────────
    #[error("{} is larger than {max} bytes", path.display())]
    FileTooLarge { path: PathBuf, max: u64 },

    /// A write was refused before touching the disk: the new content would
    /// exceed what the reader accepts (`file_too_large`).
    #[error("{} would grow larger than {max} bytes; the change was not saved", path.display())]
    WriteTooLarge { path: PathBuf, max: u64 },

    /// A Portico file is a symlink (writes only), a FIFO, a directory or a
    /// device: it is neither replaced nor opened.
    #[error("{} is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },

    #[error(transparent)]
    HostMeta(#[from] crate::hosts::HostMetaError),

    #[error(transparent)]
    Settings(#[from] crate::settings::SettingsError),

    // ── Phase 5: imports ────────────────────────────────────────────────
    #[error(transparent)]
    Import(#[from] crate::import::ImportError),

    // ── Phase 6: recording ──────────────────────────────────────────────
    #[error(transparent)]
    Recording(#[from] crate::recording::RecordingError),

    // ── Phase 6b: hosts created in Portico ──────────────────────────────
    #[error(transparent)]
    ManagedHost(#[from] crate::managed_hosts::ManagedHostError),
}

/// Host key verification failures. Never recoverable by a click: see
/// `CLAUDE.md` §11.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostKeyError {
    #[error(
        "host key for {} has CHANGED (now {actual}, expected {expected} per {}:{line}): possible man-in-the-middle attack", crate::error::prose_host(host), file.display())]
    Changed {
        host: String,
        actual: String,
        expected: String,
        file: PathBuf,
        line: usize,
    },
    #[error(
        "{} presented a new key type while known_hosts has {known}: refused under StrictHostKeyChecking accept-new/no",
        crate::error::prose_host(host)
    )]
    NewAlgorithm { host: String, known: String },
    #[error("host key for {} was not trusted", crate::error::prose_host(host))]
    Rejected { host: String },
    #[error(
        "host key for {} is unknown and StrictHostKeyChecking is enabled",
        crate::error::prose_host(host)
    )]
    UnknownStrict { host: String },
    #[error("host key for {} is revoked ({}:{line})", crate::error::prose_host(host), file.display())]
    Revoked {
        host: String,
        file: PathBuf,
        line: usize,
    },
    #[error(
        "invalid host certificate for {}: {reason}",
        crate::error::prose_host(host)
    )]
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
    // One arm per error variant: splitting the table would scatter the codes.
    #[allow(clippy::too_many_lines)]
    pub fn summary(&self) -> ErrorSummary {
        if let Self::Key(crate::keys::KeyError::ProbeFailed { summary }) = self {
            return summary.clone();
        }
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
                Some(
                    "Portico refuses rather than connect differently from ssh. Use ProxyJump for this host if you can.",
                ),
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
            Self::SessionEnded(_) => (
                "session_ended",
                Some("Reconnect to the host, then try again."),
            ),
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
            Self::Key(e) => e.code_and_hint(),

            // ── Phase 3: vault and snippets ──
            Self::Vault(e) => e.code_and_hint(),
            Self::Snippet(e) => e.code_and_hint(),

            // ── Phase 4 ──
            Self::FileTooLarge { .. } => (
                "file_too_large",
                Some("This Portico file was edited by hand and is too large: trim it."),
            ),
            Self::WriteTooLarge { .. } => (
                "file_too_large",
                Some(
                    "Portico would not be able to read this file back: remove entries you no longer need (long notes, old hosts) and retry.",
                ),
            ),
            Self::NotRegularFile { .. } => (
                "not_regular_file",
                Some(
                    "Portico does not replace a symlinked hosts.toml or settings.toml: edit the target directly or replace the link with a regular file.",
                ),
            ),
            Self::HostMeta(e) => e.code_and_hint(),
            Self::Settings(e) => e.code_and_hint(),

            // ── Phase 5 ──
            Self::Import(e) => e.code_and_hint(),

            // ── Phase 6 ──
            Self::Recording(e) => e.code_and_hint(),

            // ── Phase 6b ──
            Self::ManagedHost(e) => e.code_and_hint(),
        };
        ErrorSummary {
            code: code.to_owned(),
            message: without_os_codes(&self.to_string()),
            hint: hint.map(str::to_owned),
        }
    }
}

/// `Connection refused (os error 111)` → `Connection refused`: the errno
/// says nothing more to a user (it stays in `Debug` for logs).
fn without_os_codes(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(start) = rest.find(" (os error ") {
        let after = rest.get(start..).unwrap_or_default();
        let Some(end) = after.find(')') else { break };
        let code = after.get(" (os error ".len()..end).unwrap_or_default();
        out.push_str(rest.get(..start).unwrap_or_default());
        if code.is_empty() || !code.chars().all(|c| c.is_ascii_digit()) {
            out.push_str(after.get(..=end).unwrap_or_default());
        }
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_error_codes_are_dropped_from_summaries() {
        assert_eq!(
            without_os_codes("cannot connect to db:22: Connection refused (os error 111)"),
            "cannot connect to db:22: Connection refused"
        );
        assert_eq!(without_os_codes("a (os error x) b"), "a (os error x) b");
        assert_eq!(without_os_codes("plain"), "plain");
    }
}

/// `[host]:port` (the `known_hosts` form) written for prose: `host:port`.
/// IPv6 addresses keep their brackets (`[::1]:2222`), and other names are
/// returned as they are.
pub(crate) fn prose_host(name: &str) -> String {
    name.strip_prefix('[')
        .and_then(|rest| rest.split_once("]:"))
        .filter(|(host, _)| !host.contains(':'))
        .map_or_else(|| name.to_owned(), |(host, port)| format!("{host}:{port}"))
}

#[cfg(test)]
mod prose_host_tests {
    use super::*;

    #[test]
    fn host_key_messages_write_host_and_port_in_prose() {
        assert_eq!(prose_host("[127.0.0.1]:41985"), "127.0.0.1:41985");
        assert_eq!(prose_host("[::1]:2222"), "[::1]:2222");
        assert_eq!(prose_host("web.example"), "web.example");
        let error = HostKeyError::Rejected {
            host: "[127.0.0.1]:41985".into(),
        };
        assert_eq!(
            error.to_string(),
            "host key for 127.0.0.1:41985 was not trusted"
        );
    }
}
