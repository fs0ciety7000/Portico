//! Transport to the local `ssh-agent`: a Unix domain socket, or on Windows
//! the named pipe of the OpenSSH for Windows agent (Phase 8).
//!
//! Endpoint rules:
//!
//! - **Unix**: `SSH_AUTH_SOCK` (or `IdentityAgent`) is a socket path, used as
//!   is (unchanged since Phase 1).
//! - **Windows**: `SSH_AUTH_SOCK` (or `IdentityAgent`) when it names a named
//!   pipe (`\\.\pipe\…`, or `//./pipe/…`); when the variable is unset or
//!   empty, the pipe of the `ssh-agent` service shipped with Windows,
//!   [`OPENSSH_WINDOWS_PIPE`]. Any other value (an MSYS/Cygwin socket file of
//!   Git Bash, an `AF_UNIX` path) is refused with
//!   [`AgentConnectError::Unsupported`]: opening an arbitrary path as a pipe
//!   would open a *file*. Pageant is not supported (decision of Phase 8).
//!
//! The connection itself never blocks the runtime (tokio named pipe / Unix
//! stream); callers bound it with a timeout.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::session::signer::Agent;

/// The pipe of the OpenSSH for Windows agent service (`ssh-agent`).
pub const OPENSSH_WINDOWS_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// Why no agent connection was made.
#[derive(Debug)]
pub(crate) enum AgentConnectError {
    /// The endpoint is not of a kind this platform can reach (never on
    /// Unix, where every path is tried as a socket).
    #[cfg_attr(unix, allow(dead_code))]
    Unsupported,
    /// The endpoint exists in principle but the connection failed.
    Io(std::io::Error),
}

impl std::fmt::Display for AgentConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("only named pipes (\\\\.\\pipe\\…) are supported"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// The agent endpoint of this process, from the value of `SSH_AUTH_SOCK`.
pub(crate) fn default_socket(env: Option<OsString>) -> Option<PathBuf> {
    default_socket_for(env, cfg!(windows))
}

/// [`default_socket`] for a given platform (testable everywhere).
fn default_socket_for(env: Option<OsString>, windows: bool) -> Option<PathBuf> {
    if windows {
        match env {
            Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
            _ => Some(PathBuf::from(OPENSSH_WINDOWS_PIPE)),
        }
    } else {
        env.map(PathBuf::from)
    }
}

/// Whether `path` names a local Windows named pipe (`\\.\pipe\<name>`,
/// either slash, case-insensitive, non-empty name).
pub fn is_named_pipe(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let Some(prefix) = text.get(..9) else {
        return false;
    };
    let normalized: String = prefix
        .chars()
        .map(|c| {
            if c == '/' {
                '\\'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    normalized == r"\\.\pipe\" && text.len() > 9
}

/// Connects to the agent at `path`.
#[cfg(unix)]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    russh::keys::agent::client::AgentClient::connect_uds(path)
        .await
        .map(russh::keys::agent::client::AgentClient::dynamic)
        .map_err(|e| AgentConnectError::Io(agent_io(e)))
}

/// Connects to the agent at `path` (a named pipe, see the module doc).
#[cfg(windows)]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    if !is_named_pipe(path) {
        return Err(AgentConnectError::Unsupported);
    }
    russh::keys::agent::client::AgentClient::connect_named_pipe(path.as_os_str())
        .await
        .map(russh::keys::agent::client::AgentClient::dynamic)
        .map_err(|e| AgentConnectError::Io(agent_io(e)))
}

/// Neither Unix nor Windows: no agent transport.
#[cfg(not(any(unix, windows)))]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    let _ = path;
    Err(AgentConnectError::Unsupported)
}

/// russh's error as an I/O error (its message carries no secret).
fn agent_io(e: russh::keys::Error) -> std::io::Error {
    match e {
        russh::keys::Error::IO(io) => io,
        other => std::io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_pipe_paths() {
        for ok in [
            r"\\.\pipe\openssh-ssh-agent",
            r"\\.\PIPE\x",
            "//./pipe/openssh-ssh-agent",
            r"\\./pipe\mixed",
        ] {
            assert!(is_named_pipe(Path::new(ok)), "{ok}");
        }
        for bad in [
            r"\\.\pipe\",
            r"\\server\pipe\x",
            r"\\?\C:\agent.sock",
            r"C:\Users\me\agent.sock",
            "/tmp/ssh-XXXX/agent.42",
            "",
            r"\\.\pip",
        ] {
            assert!(!is_named_pipe(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn default_socket_per_platform() {
        // Unix: SSH_AUTH_SOCK as is, nothing when unset.
        assert_eq!(default_socket_for(None, false), None);
        assert_eq!(
            default_socket_for(Some("/tmp/a.sock".into()), false),
            Some(PathBuf::from("/tmp/a.sock"))
        );
        // Windows: the OpenSSH pipe unless SSH_AUTH_SOCK names another one.
        assert_eq!(
            default_socket_for(None, true),
            Some(PathBuf::from(OPENSSH_WINDOWS_PIPE))
        );
        assert_eq!(
            default_socket_for(Some(OsString::new()), true),
            Some(PathBuf::from(OPENSSH_WINDOWS_PIPE))
        );
        assert_eq!(
            default_socket_for(Some(r"\\.\pipe\custom".into()), true),
            Some(PathBuf::from(r"\\.\pipe\custom"))
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_file_path_is_never_opened_as_a_pipe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("agent.sock");
        std::fs::write(&file, b"").expect("write");
        assert!(matches!(
            connect(&file).await,
            Err(AgentConnectError::Unsupported)
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_missing_pipe_is_an_io_error() {
        let pipe = format!(
            r"\\.\pipe\portico-test-missing-{:016x}",
            rand::random::<u64>()
        );
        assert!(matches!(
            connect(Path::new(&pipe)).await,
            Err(AgentConnectError::Io(_))
        ));
    }
}
