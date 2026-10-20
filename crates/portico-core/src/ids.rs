//! Strongly typed, process-unique identifiers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            /// Allocates a fresh identifier, unique for the lifetime of the process.
            pub fn next() -> Self {
                Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
            }

            /// Raw numeric value (for IPC boundaries only).
            pub fn get(self) -> u64 {
                self.0
            }

            /// Rebuilds an id received over IPC. Unknown ids are rejected by
            /// the engine calls that take them.
            pub fn from_raw(raw: u64) -> Self {
                Self(raw)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "-{}"), self.0)
            }
        }
    };
}

define_id!(
    /// An SSH connection (one transport, possibly several channels).
    SessionId,
    "ses"
);
define_id!(
    /// An interactive PTY channel (a terminal tab).
    TerminalId,
    "term"
);
define_id!(
    /// A port forward.
    TunnelId,
    "tun"
);
define_id!(
    /// An SFTP subsystem handle.
    SftpId,
    "sftp"
);
define_id!(
    /// A file transfer (upload / download).
    TransferId,
    "xfer"
);
define_id!(
    /// A question the engine asks the user (host key, password, passphrase…).
    PromptId,
    "prompt"
);
