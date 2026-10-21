//! SFTP on a connected session (`russh-sftp` over a `session` channel with
//! the `sftp` subsystem).
//!
//! Invariants:
//! - One SFTP client per session, opened lazily and shared by every caller.
//!   It is retired (and reopened on next use) only after a *transport*
//!   failure, never because one request was refused.
//! - Transfers live under the session's token, stream in [`CHUNK`] blocks,
//!   report progress at most every [`PROGRESS_EVERY`] and are cancellable
//!   while data is copied. The final step (rename into place) is not: it
//!   either completes or restores the previous destination.
//! - Data goes to a partial file unique to the transfer
//!   (`<name>.<id>.part`), created exclusively (never through a symlink,
//!   never truncating someone else's file) and removed on failure.
//! - File modes are carried across like `scp`/`sftp` (`perm & 0o777`).
//! - Names and messages from the server are untrusted: listings drop names
//!   that are not a single path component, downloads build their local path
//!   with [`local_destination`], and UIs display them with [`display_safe`].
//! - Local file access goes through `tokio::fs` (blocking pool).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use russh::ChannelMsg;
use russh_sftp::client::error::Error as SftpLibError;
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::events::{EngineEvent, SftpEvent};
use crate::ids::{SessionId, SftpId, TransferId};
use crate::logs::LogLevel;
use crate::session::Attached;

/// Transfer block size.
pub const CHUNK: usize = 256 * 1024;
/// Minimum interval between two progress events of one transfer.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// Time allowed to open the subsystem.
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Per-request timeout inside the SFTP protocol (seconds).
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Longest a directory listing may take (a hostile server could stream
/// names forever; russh-sftp offers no entry cap).
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// Names longer than this are dropped from listings (`PATH_MAX`).
const MAX_NAME_BYTES: usize = 4096;
/// Bound on cleanup after a failure (the channel may be what failed).
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
/// A download may exceed its announced size (a growing log) but not by more
/// than this factor plus [`SIZE_SLACK`]: an endless file is refused.
const SIZE_FACTOR: u64 = 2;
const SIZE_SLACK: u64 = 64 * 1024 * 1024;
/// Suffix of in-progress files.
const PART_SUFFIX: &str = ".part";
/// Mode for created files when the source's is unknown.
const DEFAULT_MODE: u32 = 0o644;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One row of a file browser (remote or local). `name` is raw (use it for
/// operations); display it with [`display_safe`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix permission bits.
    pub mode: Option<u32>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub modified: Option<OffsetDateTime>,
}

/// Sorting used by both UIs: directories first, then case-insensitive name.
pub fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|a, b| {
        let dir = |e: &RemoteEntry| e.kind != EntryKind::Dir;
        dir(a)
            .cmp(&dir(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Conflict policy when the destination exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Fail with `destination_exists` so the UI can ask.
    #[default]
    Ask,
    Overwrite,
    Skip,
    /// Keep both: `name (1).ext`, `name (2).ext`…
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Upload,
    Download,
}

/// A file transfer request. Directories are not transferred recursively
/// (yet): the UI expands them.
///
/// For downloads, `local` must come from [`local_destination`] (or a native
/// file picker): never join a server-chosen name yourself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRequest {
    pub direction: Direction,
    /// Local file path.
    pub local: PathBuf,
    /// Remote file path.
    pub remote: String,
    #[serde(default)]
    pub on_conflict: OnConflict,
}

/// Why a remote operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteFailure {
    NotFound,
    PermissionDenied,
    /// The server refused the request (status code): the client is fine.
    Refused,
    /// The SFTP channel itself failed (I/O, timeout, protocol): reopen.
    Transport,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SftpError {
    #[error("the server does not offer SFTP: {message}")]
    Unavailable { message: String },
    #[error("cannot {op} {path} on the server: {message}")]
    Remote {
        op: &'static str,
        path: String,
        failure: RemoteFailure,
        message: String,
    },
    #[error("cannot {op} {}: {source}", path.display())]
    Local {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} already exists")]
    DestinationExists { path: String },
    #[error("unknown or finished transfer {0}")]
    UnknownTransfer(TransferId),
    #[error("{path} is not a regular file")]
    NotAFile { path: String },
    #[error("refusing the server-provided name {name:?}: not a single file name")]
    UnsafeName { name: String },
    #[error("{path} is larger than announced ({announced} bytes): stopped")]
    SizeMismatch { path: String, announced: u64 },
}

impl SftpError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Unavailable { .. } => (
                "sftp_unavailable",
                Some("The server may have no SFTP subsystem (Subsystem sftp in sshd_config)."),
            ),
            Self::Remote {
                failure: RemoteFailure::NotFound,
                ..
            } => ("not_found", None),
            Self::Remote {
                failure: RemoteFailure::PermissionDenied,
                ..
            } => (
                "permission_denied",
                Some("Your account on the server cannot access this path."),
            ),
            Self::Remote {
                failure: RemoteFailure::Transport,
                ..
            } => (
                "sftp_transport",
                Some("The SFTP channel failed; it is reopened on the next action."),
            ),
            Self::Remote { .. } => ("sftp_remote", None),
            Self::Local { source, .. } => match source.kind() {
                std::io::ErrorKind::NotFound => ("not_found", None),
                std::io::ErrorKind::PermissionDenied => (
                    "permission_denied",
                    Some("Check the permissions of the local file or folder."),
                ),
                _ => ("local_io", None),
            },
            Self::DestinationExists { .. } => (
                "destination_exists",
                Some("Choose to overwrite, keep both or skip."),
            ),
            Self::UnknownTransfer(_) => ("unknown_transfer", None),
            Self::NotAFile { .. } => ("not_a_file", None),
            Self::UnsafeName { .. } => (
                "unsafe_remote_name",
                Some("The server sent a name that could write outside the chosen folder."),
            ),
            Self::SizeMismatch { .. } => (
                "size_mismatch",
                Some("The remote file kept growing far beyond its announced size."),
            ),
        }
    }

    /// True when the SFTP channel should be retired.
    pub(crate) fn is_transport(&self) -> bool {
        matches!(
            self,
            Self::Remote {
                failure: RemoteFailure::Transport,
                ..
            }
        )
    }
}

fn remote(op: &'static str, path: &str, e: &SftpLibError) -> SftpError {
    let failure = match e {
        SftpLibError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => RemoteFailure::NotFound,
            StatusCode::PermissionDenied => RemoteFailure::PermissionDenied,
            // The server says the channel is gone.
            StatusCode::NoConnection | StatusCode::ConnectionLost => RemoteFailure::Transport,
            _ => RemoteFailure::Refused,
        },
        SftpLibError::Limited(_) => RemoteFailure::Refused,
        SftpLibError::IO(_)
        | SftpLibError::Timeout
        | SftpLibError::UnexpectedPacket
        | SftpLibError::UnexpectedBehavior(_) => RemoteFailure::Transport,
    };
    // The server writes status messages: never show them raw.
    let message = match e {
        SftpLibError::Status(s) if !s.error_message.is_empty() => {
            display_safe(&s.error_message, 256).into_owned()
        }
        other => display_safe(&other.to_string(), 256).into_owned(),
    };
    SftpError::Remote {
        op,
        path: path.to_owned(),
        failure,
        message,
    }
}

/// Errors from the russh-sftp `File` stream (it speaks `io::Error`).
fn remote_io(op: &'static str, path: &str, e: &std::io::Error) -> SftpError {
    SftpError::Remote {
        op,
        path: path.to_owned(),
        failure: RemoteFailure::Transport,
        message: display_safe(&e.to_string(), 256).into_owned(),
    }
}

fn local(op: &'static str, path: &Path, source: std::io::Error) -> SftpError {
    SftpError::Local {
        op,
        path: path.to_owned(),
        source,
    }
}

/// Text chosen by a server (file names, status messages, peer addresses),
/// made safe to display: controls and bidi overrides become U+FFFD so a name
/// cannot disguise itself (`invoice\u{202E}fdp.sh`), and the length is capped.
pub fn display_safe(text: &str, max_chars: usize) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| {
        c.is_control()
            || matches!(
                c,
                '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            )
    };
    let too_long = text.chars().count() > max_chars;
    if !too_long && !text.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out: String = text
        .chars()
        .take(max_chars)
        .map(|c| if unsafe_char(c) { '\u{FFFD}' } else { c })
        .collect();
    if too_long {
        out.push('…');
    }
    std::borrow::Cow::Owned(out)
}

/// A remote file name that is safe to use as a single local path component:
/// no separator, no NUL or control character, not `.`/`..`, not empty, not
/// absurdly long. A hostile server can send any bytes as a name (the class of
/// CVE-2019-6111); never join an unchecked one to a local directory.
pub fn safe_local_name(name: &str) -> Option<&str> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control);
    ok.then_some(name)
}

/// The only way to build a download destination from a server-provided name.
pub fn local_destination(dir: &Path, remote_name: &str) -> Result<PathBuf, SftpError> {
    safe_local_name(remote_name)
        .map(|name| dir.join(name))
        .ok_or_else(|| SftpError::UnsafeName {
            name: display_safe(remote_name, 128).into_owned(),
        })
}

/// `dir/name` with POSIX separators (remote paths are always `/`).
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Last path component of a remote path.
pub fn remote_name(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// The SFTP client of one session.
pub(crate) struct SftpClient {
    pub(crate) id: SftpId,
    pub(crate) session: SessionId,
    sftp: SftpSession,
    /// Set after a transport failure: the next caller opens a new client.
    retired: AtomicBool,
}

impl std::fmt::Debug for SftpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpClient")
            .field("id", &self.id)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// Opens the `sftp` subsystem on the session.
pub(crate) async fn open(attached: &Attached) -> crate::Result<SftpClient> {
    let ctx = &attached.ctx;
    let unavailable = |message: String| {
        crate::Error::from(SftpError::Unavailable {
            message: display_safe(&message, 256).into_owned(),
        })
    };
    let opening = async {
        let mut channel = attached
            .handle
            .channel_open_session()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        // russh only sends the request: wait for the server's answer, or a
        // refusal would be mistaken for a silent server.
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => break,
                Some(ChannelMsg::Failure) => {
                    return Err(unavailable("the server refused the subsystem".into()));
                }
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                    return Err(unavailable("the server closed the channel".into()));
                }
                Some(_) => {}
            }
        }
        let config = SftpConfig {
            // Like OpenSSH's sftp: 32 KiB writes, 64 in flight.
            max_concurrent_writes: 64,
            max_concurrent_reads: 32,
            request_timeout_secs: REQUEST_TIMEOUT_SECS,
            ..SftpConfig::default()
        };
        let sftp = SftpSession::new_with_config(channel.into_stream(), config)
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        Ok::<_, crate::Error>(sftp)
    };
    let sftp = tokio::select! {
        biased;
        () = ctx.cancel.cancelled() => return Err(crate::Error::SessionEnded(ctx.id)),
        r = tokio::time::timeout(OPEN_TIMEOUT, opening) => r.map_err(|_| crate::Error::Timeout {
            phase: "opening SFTP on",
            target: ctx.engine.session_alias(ctx.id).unwrap_or_default(),
        })??,
    };
    let client = SftpClient {
        id: SftpId::next(),
        session: ctx.id,
        sftp,
        retired: AtomicBool::new(false),
    };
    ctx.log(LogLevel::Info, "sftp", format!("sftp {} opened", client.id));
    Ok(client)
}

fn entry_from(name: String, path: String, meta: &FileAttributes) -> RemoteEntry {
    let kind = if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_symlink() {
        EntryKind::Symlink
    } else if meta.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    RemoteEntry {
        name,
        path,
        kind,
        size: meta.size.unwrap_or(0),
        mode: meta.permissions.map(|p| p & 0o7777),
        modified: meta
            .mtime
            .and_then(|t| OffsetDateTime::from_unix_timestamp(i64::from(t)).ok()),
    }
}

/// How a transfer ended (for the event and the log).
enum Outcome {
    Finished(String),
    Skipped,
}

impl SftpClient {
    pub(crate) fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// Marks the client unusable after a transport failure.
    pub(crate) fn note(&self, error: &crate::Error) {
        if let crate::Error::Sftp(e) = error
            && e.is_transport()
        {
            self.retired.store(true, Ordering::Release);
        }
    }

    pub(crate) async fn home(&self) -> crate::Result<String> {
        Ok(self
            .sftp
            .canonicalize(".")
            .await
            .map_err(|e| remote("resolve", ".", &e))?)
    }

    pub(crate) async fn list(&self, path: &str) -> crate::Result<Vec<RemoteEntry>> {
        let dir = tokio::time::timeout(LIST_TIMEOUT, self.sftp.read_dir(path))
            .await
            .map_err(|_| crate::Error::Timeout {
                phase: "listing",
                target: path.to_owned(),
            })?
            .map_err(|e| remote("list", path, &e))?;
        let mut entries: Vec<RemoteEntry> = dir
            .filter_map(|e| {
                // Hostile names (`../x`, `/abs`, NUL…) never reach the UIs.
                let name = e.file_name();
                safe_local_name(&name)?;
                Some(entry_from(
                    name.clone(),
                    join_remote(path, &name),
                    &e.metadata(),
                ))
            })
            .collect();
        sort_entries(&mut entries);
        Ok(entries)
    }

    pub(crate) async fn stat(&self, path: &str) -> crate::Result<RemoteEntry> {
        let meta = self
            .sftp
            .metadata(path)
            .await
            .map_err(|e| remote("stat", path, &e))?;
        Ok(entry_from(
            remote_name(path).to_owned(),
            path.to_owned(),
            &meta,
        ))
    }

    async fn exists(&self, path: &str) -> bool {
        self.sftp.symlink_metadata(path).await.is_ok()
    }

    pub(crate) async fn mkdir(&self, path: &str) -> crate::Result<()> {
        Ok(self
            .sftp
            .create_dir(path)
            .await
            .map_err(|e| remote("create", path, &e))?)
    }

    pub(crate) async fn rename(&self, from: &str, to: &str) -> crate::Result<()> {
        if self.exists(to).await {
            return Err(SftpError::DestinationExists {
                path: to.to_owned(),
            }
            .into());
        }
        Ok(self
            .sftp
            .rename(from, to)
            .await
            .map_err(|e| remote("rename", from, &e))?)
    }

    /// Removes a file, a symlink or an empty directory.
    pub(crate) async fn remove(&self, path: &str) -> crate::Result<()> {
        let meta = self
            .sftp
            .symlink_metadata(path)
            .await
            .map_err(|e| remote("remove", path, &e))?;
        let removed = if meta.is_dir() {
            self.sftp.remove_dir(path).await
        } else {
            self.sftp.remove_file(path).await
        };
        Ok(removed.map_err(|e| remote("remove", path, &e))?)
    }

    /// Runs one transfer to completion, reporting on the event bus.
    pub(crate) async fn transfer(
        self: Arc<Self>,
        attached: Attached,
        id: TransferId,
        request: TransferRequest,
        cancel: CancellationToken,
    ) {
        let ctx = &attached.ctx;
        let emit = |event: SftpEvent| {
            ctx.engine.emit(EngineEvent::Sftp {
                id: self.id,
                session: self.session,
                event,
            });
        };
        // The partial file in progress (to clean up on failure).
        let part: PartSlot = std::sync::Mutex::new(None);
        let result = match request.direction {
            Direction::Download => self.download(id, &request, &part, &cancel, &emit).await,
            Direction::Upload => self.upload(id, &request, &part, &cancel, &emit).await,
        };
        match result {
            Ok(Outcome::Finished(destination)) => {
                ctx.log(
                    LogLevel::Info,
                    "sftp",
                    format!("transfer {id} finished: {destination}"),
                );
                emit(SftpEvent::TransferFinished {
                    transfer: id,
                    destination,
                });
            }
            Ok(Outcome::Skipped) => emit(SftpEvent::TransferSkipped { transfer: id }),
            Err(e) => {
                self.note(&e);
                let leftover = part
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let _ = tokio::time::timeout(CLEANUP_TIMEOUT, self.discard(leftover)).await;
                if ctx.cancel.is_cancelled() {
                    // The session ended: that is a failure, not a user cancel.
                    let error = crate::Error::SessionEnded(ctx.id).summary();
                    emit(SftpEvent::TransferFailed {
                        transfer: id,
                        error,
                    });
                } else if matches!(e, crate::Error::Cancelled) {
                    ctx.log(LogLevel::Info, "sftp", format!("transfer {id} cancelled"));
                    emit(SftpEvent::TransferCancelled { transfer: id });
                } else {
                    ctx.log(LogLevel::Warn, "sftp", format!("transfer {id}: {e}"));
                    emit(SftpEvent::TransferFailed {
                        transfer: id,
                        error: e.summary(),
                    });
                }
            }
        }
    }

    async fn discard(&self, part: Option<Part>) {
        match part {
            Some(Part::Local(path)) => {
                let _ = tokio::fs::remove_file(path).await;
            }
            Some(Part::Remote(path)) => {
                let _ = self.sftp.remove_file(path).await;
            }
            None => {}
        }
    }

    async fn download(
        &self,
        id: TransferId,
        request: &TransferRequest,
        part_slot: &PartSlot,
        cancel: &CancellationToken,
        emit: &(impl Fn(SftpEvent) + Sync),
    ) -> crate::Result<Outcome> {
        let source = &request.remote;
        let meta = self
            .sftp
            .metadata(source)
            .await
            .map_err(|e| remote("read", source, &e))?;
        if !meta.is_regular() {
            return Err(SftpError::NotAFile {
                path: source.clone(),
            }
            .into());
        }
        let Some(dest) = resolve_local_conflict(&request.local, request.on_conflict).await? else {
            return Ok(Outcome::Skipped);
        };
        let total = meta.size;
        let mode = meta.permissions.map_or(DEFAULT_MODE, |p| p & 0o777);
        emit(SftpEvent::TransferStarted {
            transfer: id,
            direction: Direction::Download,
            name: display_safe(remote_name(source), 256).into_owned(),
            total,
        });
        let mut file = self
            .sftp
            .open(source.as_str())
            .await
            .map_err(|e| remote("open", source, &e))?;
        let part = local_part(&dest, id);
        // Exclusive creation: never through a symlink, never someone else's file.
        let mut out = create_exclusive(&part, mode)
            .await
            .map_err(|e| local("create", &part, e))?;
        set_part(part_slot, Part::Local(part.clone()));
        let limit = total.map(|t| t.saturating_mul(SIZE_FACTOR).saturating_add(SIZE_SLACK));
        let mut progress = Progress::new(id, total);
        let mut buf = vec![0_u8; CHUNK];
        loop {
            let n = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(crate::Error::Cancelled),
                n = file.read(&mut buf) => n.map_err(|e| remote_io("read", source, &e))?,
            };
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            out.write_all(chunk)
                .await
                .map_err(|e| local("write", &part, e))?;
            progress.advance(n, emit);
            if let (Some(limit), Some(announced)) = (limit, total)
                && progress.done > limit
            {
                return Err(SftpError::SizeMismatch {
                    path: source.clone(),
                    announced,
                }
                .into());
            }
        }
        out.flush().await.map_err(|e| local("write", &part, e))?;
        out.sync_all().await.map_err(|e| local("write", &part, e))?;
        drop(out);
        let _ = file.shutdown().await;
        // From here on, not cancellable: put the file in place or fail cleanly.
        let dest = finish_local(&part, &dest, request.on_conflict).await?;
        clear_part(part_slot);
        progress.finish(emit);
        Ok(Outcome::Finished(dest.display().to_string()))
    }

    async fn upload(
        &self,
        id: TransferId,
        request: &TransferRequest,
        part_slot: &PartSlot,
        cancel: &CancellationToken,
        emit: &(impl Fn(SftpEvent) + Sync),
    ) -> crate::Result<Outcome> {
        let source = &request.local;
        let meta = tokio::fs::metadata(source)
            .await
            .map_err(|e| local("read", source, e))?;
        if !meta.is_file() {
            return Err(SftpError::NotAFile {
                path: source.display().to_string(),
            }
            .into());
        }
        let Some(dest) = self
            .resolve_remote_conflict(&request.remote, request.on_conflict)
            .await?
        else {
            return Ok(Outcome::Skipped);
        };
        let total = Some(meta.len());
        emit(SftpEvent::TransferStarted {
            transfer: id,
            direction: Direction::Upload,
            name: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            total,
        });
        let mut input = tokio::fs::File::open(source)
            .await
            .map_err(|e| local("open", source, e))?;
        // Overwrite keeps the existing file's mode (like `sftp put`);
        // otherwise the local mode is carried over.
        let existing_mode = match self.sftp.metadata(dest.as_str()).await {
            Ok(m) => m.permissions.map(|p| p & 0o777),
            Err(_) => None,
        };
        let mode = existing_mode.unwrap_or_else(|| local_mode(&meta));
        let part = format!("{dest}.{id}{PART_SUFFIX}");
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(mode);
        let mut file = self
            .sftp
            .open_with_flags_and_attributes(
                part.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                attrs,
            )
            .await
            .map_err(|e| remote("create", &part, &e))?;
        set_part(part_slot, Part::Remote(part.clone()));
        let mut progress = Progress::new(id, total);
        let mut buf = vec![0_u8; CHUNK];
        loop {
            let n = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(crate::Error::Cancelled),
                n = input.read(&mut buf) => n.map_err(|e| local("read", source, e))?,
            };
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(crate::Error::Cancelled),
                w = file.write_all(chunk) => w.map_err(|e| remote_io("write", &part, &e))?,
            }
            progress.advance(n, emit);
        }
        // Flushes pending writes and checks their acknowledgements.
        file.shutdown()
            .await
            .map_err(|e| remote_io("write", &part, &e))?;
        // Some servers ignore the mode at creation (umask): set it again.
        let mut mode_attrs = FileAttributes::empty();
        mode_attrs.permissions = Some(mode);
        let _ = self.sftp.set_metadata(part.as_str(), mode_attrs).await;
        // From here on, not cancellable.
        self.finish_remote(&part, &dest, id).await?;
        clear_part(part_slot);
        progress.finish(emit);
        Ok(Outcome::Finished(dest))
    }

    /// Moves the uploaded part into place. SFTP v3 `rename` never replaces,
    /// so an existing destination is first moved aside and restored if the
    /// final rename fails: at no point are both versions lost.
    async fn finish_remote(&self, part: &str, dest: &str, id: TransferId) -> crate::Result<()> {
        if !self.exists(dest).await {
            return Ok(self
                .sftp
                .rename(part, dest)
                .await
                .map_err(|e| remote("rename", part, &e))?);
        }
        let aside = format!("{dest}.{id}.old");
        self.sftp
            .rename(dest, aside.as_str())
            .await
            .map_err(|e| remote("replace", dest, &e))?;
        if let Err(e) = self.sftp.rename(part, dest).await {
            // Put the previous version back.
            let _ = self.sftp.rename(aside.as_str(), dest).await;
            return Err(remote("rename", part, &e).into());
        }
        let _ = self.sftp.remove_file(aside).await;
        Ok(())
    }

    async fn resolve_remote_conflict(
        &self,
        dest: &str,
        policy: OnConflict,
    ) -> crate::Result<Option<String>> {
        if !self.exists(dest).await {
            return Ok(Some(dest.to_owned()));
        }
        match policy {
            OnConflict::Ask => Err(SftpError::DestinationExists {
                path: dest.to_owned(),
            }
            .into()),
            OnConflict::Overwrite => Ok(Some(dest.to_owned())),
            OnConflict::Skip => Ok(None),
            OnConflict::Rename => {
                for n in 1..=999 {
                    let candidate = numbered(dest, n);
                    if !self.exists(&candidate).await {
                        return Ok(Some(candidate));
                    }
                }
                Err(SftpError::DestinationExists {
                    path: dest.to_owned(),
                }
                .into())
            }
        }
    }
}

#[cfg(unix)]
fn local_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn local_mode(_meta: &std::fs::Metadata) -> u32 {
    DEFAULT_MODE
}

async fn create_exclusive(path: &Path, mode: u32) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path).await
}

async fn local_exists(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

async fn resolve_local_conflict(dest: &Path, policy: OnConflict) -> crate::Result<Option<PathBuf>> {
    if !local_exists(dest).await {
        return Ok(Some(dest.to_owned()));
    }
    match policy {
        OnConflict::Ask => Err(SftpError::DestinationExists {
            path: dest.display().to_string(),
        }
        .into()),
        OnConflict::Overwrite => Ok(Some(dest.to_owned())),
        OnConflict::Skip => Ok(None),
        OnConflict::Rename => {
            for n in 1..=999 {
                let candidate = numbered_path(dest, n);
                if !local_exists(&candidate).await {
                    return Ok(Some(candidate));
                }
            }
            Err(SftpError::DestinationExists {
                path: dest.display().to_string(),
            }
            .into())
        }
    }
}

/// Puts a finished download in place. Without `Overwrite`, a destination
/// that appeared meanwhile is never replaced: a hard link fails if it
/// exists (no check-then-act race); `Rename` then takes the next free name.
async fn finish_local(part: &Path, dest: &Path, policy: OnConflict) -> crate::Result<PathBuf> {
    if policy == OnConflict::Overwrite {
        tokio::fs::rename(part, dest)
            .await
            .map_err(|e| local("rename", part, e))?;
        return Ok(dest.to_owned());
    }
    let mut candidate = dest.to_owned();
    for n in 1..=999 {
        match tokio::fs::hard_link(part, &candidate).await {
            Ok(()) => {
                let _ = tokio::fs::remove_file(part).await;
                return Ok(candidate);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if policy != OnConflict::Rename {
                    return Err(SftpError::DestinationExists {
                        path: candidate.display().to_string(),
                    }
                    .into());
                }
                candidate = numbered_path(dest, n);
            }
            // No hard links here (some file systems): re-check, then rename.
            Err(_) => {
                if local_exists(&candidate).await {
                    return Err(SftpError::DestinationExists {
                        path: candidate.display().to_string(),
                    }
                    .into());
                }
                tokio::fs::rename(part, &candidate)
                    .await
                    .map_err(|e| local("rename", part, e))?;
                return Ok(candidate);
            }
        }
    }
    Err(SftpError::DestinationExists {
        path: dest.display().to_string(),
    }
    .into())
}

/// A partial file being written.
enum Part {
    Local(PathBuf),
    Remote(String),
}

type PartSlot = std::sync::Mutex<Option<Part>>;

fn set_part(slot: &PartSlot, part: Part) {
    *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(part);
}

fn clear_part(slot: &PartSlot) {
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
}

/// `<dest>.<transfer id>.part`, unique per transfer.
fn local_part(dest: &Path, id: TransferId) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(format!(".{id}{PART_SUFFIX}"));
    PathBuf::from(name)
}

/// `dir/name (n).ext` (the extension is kept; dotfiles keep their name).
pub fn numbered(path: &str, n: u32) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => path.split_at(i.saturating_add(1)),
        None => ("", path),
    };
    match file.rfind('.') {
        Some(dot) if dot > 0 => {
            let (stem, ext) = file.split_at(dot);
            format!("{dir}{stem} ({n}){ext}")
        }
        _ => format!("{dir}{file} ({n})"),
    }
}

/// Same as [`numbered`] for local paths, without lossy conversions.
fn numbered_path(path: &Path, n: u32) -> PathBuf {
    let Some(file) = path.file_name() else {
        return path.to_owned();
    };
    let (stem, ext) = match (Path::new(file).file_stem(), Path::new(file).extension()) {
        (Some(stem), Some(ext)) => (stem.to_owned(), Some(ext.to_owned())),
        _ => (file.to_owned(), None),
    };
    let mut name: OsString = stem;
    name.push(format!(" ({n})"));
    if let Some(ext) = ext {
        name.push(".");
        name.push(ext);
    }
    path.with_file_name(name)
}

/// Throttled progress reporting.
struct Progress {
    id: TransferId,
    total: Option<u64>,
    done: u64,
    last: Instant,
}

impl Progress {
    fn new(id: TransferId, total: Option<u64>) -> Self {
        Self {
            id,
            total,
            done: 0,
            last: Instant::now(),
        }
    }

    fn advance(&mut self, n: usize, emit: &impl Fn(SftpEvent)) {
        self.done = self
            .done
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.last = Instant::now();
            emit(SftpEvent::TransferProgress {
                transfer: self.id,
                done: self.done,
                total: self.total,
            });
        }
    }

    fn finish(&self, emit: &impl Fn(SftpEvent)) {
        emit(SftpEvent::TransferProgress {
            transfer: self.id,
            done: self.done,
            total: self.total,
        });
    }
}

/// Lists a local directory (same model as remote listings).
pub async fn list_local(path: &Path) -> crate::Result<Vec<RemoteEntry>> {
    let mut dir = tokio::fs::read_dir(path)
        .await
        .map_err(|e| local("list", path, e))?;
    let mut entries = Vec::new();
    while let Some(entry) = dir.next_entry().await.map_err(|e| local("list", path, e))? {
        let Ok(meta) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        let ft = meta.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_symlink() {
            // Follow the link to know whether it opens as a folder.
            match tokio::fs::metadata(entry.path()).await {
                Ok(m) if m.is_dir() => EntryKind::Dir,
                _ => EntryKind::Symlink,
            }
        } else if ft.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            Some(meta.permissions().mode() & 0o7777)
        };
        #[cfg(not(unix))]
        let mode = None;
        entries.push(RemoteEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
            kind,
            size: meta.len(),
            mode,
            modified: meta.modified().ok().map(OffsetDateTime::from),
        });
    }
    sort_entries(&mut entries);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_keeps_extension_and_dotfiles() {
        assert_eq!(
            numbered("/srv/a/report.tar.gz", 1),
            "/srv/a/report.tar (1).gz"
        );
        assert_eq!(numbered("/srv/a/notes.txt", 2), "/srv/a/notes (2).txt");
        assert_eq!(numbered("/srv/.bashrc", 1), "/srv/.bashrc (1)");
        assert_eq!(numbered("README", 3), "README (3)");
        assert_eq!(
            numbered_path(Path::new("/tmp/notes.txt"), 2),
            PathBuf::from("/tmp/notes (2).txt")
        );
        assert_eq!(
            numbered_path(Path::new("/tmp/.bashrc"), 1),
            PathBuf::from("/tmp/.bashrc (1)")
        );
    }

    #[test]
    fn remote_paths() {
        assert_eq!(join_remote("/", "etc"), "/etc");
        assert_eq!(join_remote("/home/a", "b"), "/home/a/b");
        assert_eq!(remote_name("/home/a/b.txt"), "b.txt");
        assert_eq!(remote_name("/home/a/"), "a");
        assert_eq!(remote_name("x"), "x");
    }

    #[test]
    fn display_safe_neutralises_bidi_and_truncates() {
        assert_eq!(display_safe("report.pdf", 64), "report.pdf");
        assert!(matches!(
            display_safe("report.pdf", 64),
            std::borrow::Cow::Borrowed(_)
        ));
        let spoof = display_safe("invoice\u{202E}fdp.sh", 64);
        assert!(!spoof.contains('\u{202E}'));
        assert!(spoof.contains('\u{FFFD}'));
        assert!(!display_safe("a\x1b[31mb", 64).contains('\x1b'));
        let input = "x".repeat(10_000);
        assert_eq!(display_safe(&input, 256).chars().count(), 257);
    }

    #[test]
    fn hostile_names_are_not_local_components() {
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b",
            "/etc/passwd",
            "a\\b",
            "nul\0",
            "bell\u{7}",
        ] {
            assert_eq!(safe_local_name(bad), None, "{bad:?}");
            assert!(local_destination(Path::new("/dl"), bad).is_err());
        }
        assert_eq!(safe_local_name("report.pdf"), Some("report.pdf"));
        assert_eq!(safe_local_name(".bashrc"), Some(".bashrc"));
        assert_eq!(safe_local_name("..foo"), Some("..foo"));
        assert_eq!(safe_local_name("été 2026.txt"), Some("été 2026.txt"));
        assert_eq!(
            local_destination(Path::new("/dl"), "a.txt").unwrap(),
            PathBuf::from("/dl/a.txt")
        );
        assert!(safe_local_name(&"x".repeat(5000)).is_none());
    }

    #[test]
    fn part_names_are_unique_per_transfer() {
        let a = local_part(Path::new("/tmp/a.bin"), TransferId::next());
        let b = local_part(Path::new("/tmp/a.bin"), TransferId::next());
        assert_ne!(a, b);
        assert!(a.to_string_lossy().ends_with(".part"));
        assert!(a.to_string_lossy().starts_with("/tmp/a.bin."));
    }
}
