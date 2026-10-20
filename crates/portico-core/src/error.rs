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
            Self::Unsupported(_) => ("unsupported", None),
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
