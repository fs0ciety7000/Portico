//! Transport to the local `ssh-agent`: a Unix domain socket, or on Windows
//! the named pipe of the OpenSSH for Windows agent (Phase 8).
//!
//! Endpoint rules:
//!
//! - **Unix**: `SSH_AUTH_SOCK` (or `IdentityAgent`) is a socket path, used as
//!   is (unchanged since Phase 1).
//! - **Windows**: `SSH_AUTH_SOCK` (or `IdentityAgent`) when it names a named
//!   pipe (`\\.\pipe\…`, `\\?\pipe\…`, or `//./pipe/…`); when the
//!   variable is unset or empty, the pipe of the `ssh-agent` service shipped
//!   with Windows, [`OPENSSH_WINDOWS_PIPE`]. Any other value (an MSYS/Cygwin
//!   socket file of Git Bash, an `AF_UNIX` path, a remote `\\server\pipe\…`,
//!   a name with a `.`/`..` segment, which Win32 would normalise into a
//!   file path) is refused with [`AgentConnectError::Unsupported`]: opening
//!   an arbitrary path as a pipe would open a *file*. Pageant is not
//!   supported (decision of Phase 8).
//!
//! **Windows: the server is verified before anything is sent.** The
//! service's pipe is disabled by default, so any local account could create
//! the first instance of `\\.\pipe\openssh-ssh-agent` and receive the
//! private keys `ssh-add` would send it. The pipe is opened here (bounded
//! retry while it is busy), checked to be a pipe, then its server must be
//! the agent service (`SYSTEM`) or the current user (an agent the user runs:
//! 1Password, `KeePassXC`…), from the pipe's owner and the server process's
//! token ([`portico_winsec::agent_server_trusted`]). Otherwise:
//! [`AgentConnectError::Untrusted`].
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
    /// Windows: the pipe is served by another account (or could not be
    /// verified). Nothing was sent to it. Never on Unix (the socket's
    /// directory is the user's, see `ssh-agent`).
    #[cfg_attr(unix, allow(dead_code))]
    Untrusted(&'static str),
    /// The endpoint exists in principle but the connection failed.
    Io(std::io::Error),
}

impl AgentConnectError {
    /// Nothing listens there (no agent running).
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self, Self::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
    }
}

impl std::fmt::Display for AgentConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("only named pipes (\\\\.\\pipe\\…) are supported"),
            Self::Untrusted(why) => write!(f, "refused: {why}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Whether `path` is the endpoint used when nothing was configured: the
/// Windows service pipe (no agent there is the normal case when the service
/// is disabled, which it is by default).
pub(crate) fn is_implicit_default(path: &Path) -> bool {
    cfg!(windows)
        && path
            .to_string_lossy()
            .eq_ignore_ascii_case(OPENSSH_WINDOWS_PIPE)
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

/// Whether `path` names a local Windows named pipe: `\\.\pipe\<name>` or
/// `\\?\pipe\<name>`, either slash, case-insensitive, a non-empty name
/// without any `.` / `..` segment (nor one of dots and spaces only: Win32
/// trims those). `\\.\pipe\..\C:\x` would be normalised to the *file*
/// `C:\x`; a remote `\\server\pipe\…` is refused too.
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
    if normalized != r"\\.\pipe\" && normalized != r"\\?\pipe\" {
        return false;
    }
    let Some(name) = text.get(9..) else {
        return false;
    };
    let dotted =
        |segment: &str| !segment.is_empty() && segment.chars().all(|c| c == '.' || c == ' ');
    let mut segments = name.split(['\\', '/']);
    !name.split(['\\', '/']).all(str::is_empty) && !segments.any(dotted)
}

/// Connects to the agent at `path`.
#[cfg(unix)]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    russh::keys::agent::client::AgentClient::connect_uds(path)
        .await
        .map(russh::keys::agent::client::AgentClient::dynamic)
        .map_err(|e| AgentConnectError::Io(agent_io(e)))
}

/// Opening attempts while every instance of the pipe is busy
/// (`ERROR_PIPE_BUSY`), [`PIPE_BUSY_DELAY`] apart: about 2 s in all (russh's
/// own loop retries forever).
#[cfg(windows)]
const PIPE_BUSY_ATTEMPTS: u32 = 40;
#[cfg(windows)]
const PIPE_BUSY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);
/// `ERROR_PIPE_BUSY`.
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

/// Connects to the agent at `path` (a named pipe whose server is verified,
/// see the module doc).
#[cfg(windows)]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    if !is_named_pipe(path) {
        return Err(AgentConnectError::Unsupported);
    }
    let mut attempt = 0u32;
    // `ClientOptions` asks for `SECURITY_IDENTIFICATION` only: the server
    // cannot act as the user with this connection.
    let pipe = loop {
        match ClientOptions::new().open(path.as_os_str()) {
            Ok(pipe) => break pipe,
            Err(e)
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && attempt.saturating_add(1) < PIPE_BUSY_ATTEMPTS =>
            {
                attempt = attempt.saturating_add(1);
                tokio::time::sleep(PIPE_BUSY_DELAY).await;
            }
            Err(e) => return Err(AgentConnectError::Io(e)),
        }
    };
    // A pipe, not a file or a device that the name led to.
    pipe.info()
        .map_err(|_| AgentConnectError::Untrusted("not a named pipe"))?;
    verify_server(&pipe)?;
    Ok(russh::keys::agent::client::AgentClient::connect(pipe).dynamic())
}

/// The pipe's owner and its server process's user must be trusted (see
/// [`portico_winsec::agent_server_trusted`]).
#[cfg(windows)]
fn verify_server(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
) -> Result<(), AgentConnectError> {
    let owner = portico_winsec::audit(pipe)
        .map_err(|_| AgentConnectError::Untrusted("the pipe's owner cannot be read"))?
        .owner;
    let server = portico_winsec::pipe_server_user(pipe)
        .map_err(|_| AgentConnectError::Untrusted("the pipe's server cannot be identified"))?;
    if portico_winsec::agent_server_trusted(owner, server) {
        Ok(())
    } else {
        tracing::warn!(
            ?owner,
            ?server,
            "ssh-agent pipe served by another account: refused"
        );
        Err(AgentConnectError::Untrusted(
            "the pipe is served by another account, not the ssh-agent service nor you",
        ))
    }
}

/// Neither Unix nor Windows: no agent transport.
#[cfg(not(any(unix, windows)))]
pub(crate) async fn connect(path: &Path) -> Result<Agent, AgentConnectError> {
    let _ = path;
    Err(AgentConnectError::Unsupported)
}

/// russh's error as an I/O error (its message carries no secret).
#[cfg(unix)]
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
            r"\\?\pipe\openssh-ssh-agent",
            r"\\?\PiPe\x",
            r"\\.\pipe\a\b",
            r"\\.\pipe\.hidden",
            r"\\.\pipe\a..b",
        ] {
            assert!(is_named_pipe(Path::new(ok)), "{ok}");
        }
        for bad in [
            r"\\.\pipe\",
            r"\\.\pipe\\",
            r"\\server\pipe\x",
            r"\\?\C:\agent.sock",
            r"C:\Users\me\agent.sock",
            "/tmp/ssh-XXXX/agent.42",
            "",
            r"\\.\pip",
            // Win32 normalises these into a file path.
            r"\\.\pipe\..\C:\x",
            r"\\.\pipe\..",
            r"\\.\pipe\.",
            r"\\.\pipe\a\..\..\C:\x",
            "//./pipe/../C:/x",
            r"\\.\pipe\x\.\y",
            r"\\.\pipe\...",
            r"\\.\pipe\. .",
            r"\\?\pipe\..\C:\x",
        ] {
            assert!(!is_named_pipe(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn the_implicit_default_is_the_windows_service_pipe() {
        assert_eq!(
            is_implicit_default(Path::new(OPENSSH_WINDOWS_PIPE)),
            cfg!(windows)
        );
        assert!(!is_implicit_default(Path::new(r"\\.\pipe\custom")));
        assert!(!is_implicit_default(Path::new("/tmp/agent.sock")));
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

    /// An agent run by the current user (here: a pipe this test creates)
    /// passes the server check.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_pipe_served_by_this_user_is_accepted() {
        if crate::winfs::tests::under_wine() {
            return; // no pipe security descriptor under wine
        }
        let pipe = format!(
            r"\\.\pipe\portico-test-agent-{:016x}",
            rand::random::<u64>()
        );
        let _server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)
            .expect("pipe server");
        let result = connect(Path::new(&pipe)).await;
        assert!(result.is_ok(), "{:?}", result.err());
    }

    /// Every instance busy: a bounded wait, then an I/O error (russh's own
    /// loop would retry forever).
    #[cfg(windows)]
    #[tokio::test]
    async fn a_busy_pipe_is_given_up() {
        if crate::winfs::tests::under_wine() {
            return;
        }
        let pipe = format!(r"\\.\pipe\portico-test-busy-{:016x}", rand::random::<u64>());
        let _server = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(true)
            .max_instances(1)
            .create(&pipe)
            .expect("pipe server");
        // The only instance, taken.
        let _first = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe)
            .expect("first client");
        let started = std::time::Instant::now();
        let result = connect(Path::new(&pipe)).await;
        assert!(matches!(result, Err(AgentConnectError::Io(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_missing_pipe_is_an_io_error() {
        let pipe = format!(
            r"\\.\pipe\portico-test-missing-{:016x}",
            rand::random::<u64>()
        );
        let result = connect(Path::new(&pipe)).await;
        assert!(matches!(result, Err(AgentConnectError::Io(_))));
        assert!(result.err().is_some_and(|e| e.is_not_found()));
    }
}
