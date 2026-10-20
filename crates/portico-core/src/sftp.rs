//! SFTP on a connected session (`russh-sftp` over a `session` channel with
//! the `sftp` subsystem).
//!
//! - One SFTP client per session, opened lazily and shared by every caller;
//!   it is a child of the session (closing the session closes it).
//! - Transfers are streamed in [`CHUNK`] blocks, report progress at most
//!   every [`PROGRESS_EVERY`], are cancellable, and never leave a partial
//!   file under the final name: data goes to `<name>.part`, renamed at the
//!   end (removed on failure or cancel).
//! - Local file access goes through `tokio::fs` (blocking pool).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpLibError;
use russh_sftp::protocol::StatusCode;
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
/// Suffix of in-progress files.
const PART_SUFFIX: &str = ".part";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One row of a file browser (remote or local).
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

/// Why a remote operation failed (from the SFTP status code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteFailure {
    NotFound,
    PermissionDenied,
    Other,
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
        }
    }
}

fn remote(op: &'static str, path: &str, e: &SftpLibError) -> SftpError {
    let failure = match e {
        SftpLibError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => RemoteFailure::NotFound,
            StatusCode::PermissionDenied => RemoteFailure::PermissionDenied,
            _ => RemoteFailure::Other,
        },
        _ => RemoteFailure::Other,
    };
    let message = match e {
        SftpLibError::Status(s) if !s.error_message.is_empty() => s.error_message.clone(),
        other => other.to_string(),
    };
    SftpError::Remote {
        op,
        path: path.to_owned(),
        failure,
        message,
    }
}

fn local(op: &'static str, path: &Path, source: std::io::Error) -> SftpError {
    SftpError::Local {
        op,
        path: path.to_owned(),
        source,
    }
}

/// The SFTP client of one session.
pub(crate) struct SftpClient {
    pub(crate) id: SftpId,
    pub(crate) session: SessionId,
    sftp: SftpSession,
    /// Child of the session token; cancelled when the client is unusable.
    pub(crate) cancel: CancellationToken,
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
    let unavailable = |message: String| crate::Error::from(SftpError::Unavailable { message });
    let opening = async {
        let channel = attached
            .handle
            .channel_open_session()
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| unavailable(e.to_string()))?;
        let sftp = SftpSession::new(channel.into_stream())
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
    sftp.set_timeout(REQUEST_TIMEOUT_SECS);
    let client = SftpClient {
        id: SftpId::next(),
        session: ctx.id,
        sftp,
        cancel: ctx.cancel.child_token(),
    };
    ctx.log(LogLevel::Info, "sftp", format!("sftp {} opened", client.id));
    Ok(client)
}

fn entry_from(
    name: String,
    path: String,
    meta: &russh_sftp::protocol::FileAttributes,
) -> RemoteEntry {
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

impl SftpClient {
    pub(crate) async fn home(&self) -> crate::Result<String> {
        Ok(self
            .sftp
            .canonicalize(".")
            .await
            .map_err(|e| remote("resolve", ".", &e))?)
    }

    pub(crate) async fn list(&self, path: &str) -> crate::Result<Vec<RemoteEntry>> {
        let dir = self
            .sftp
            .read_dir(path)
            .await
            .map_err(|e| remote("list", path, &e))?;
        let mut entries: Vec<RemoteEntry> = dir
            .filter(|e| {
                let name = e.file_name();
                name != "." && name != ".."
            })
            .map(|e| {
                let name = e.file_name();
                entry_from(name.clone(), join_remote(path, &name), &e.metadata())
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
        self.sftp.metadata(path).await.is_ok()
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
        // The partial file in progress, known even if the transfer future is
        // dropped by the cancellation below.
        let part: PartSlot = std::sync::Mutex::new(None);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(crate::Error::Cancelled),
            r = self.run_transfer(id, &request, &part, &emit) => r,
        };
        match result {
            Ok(Some(dest)) => {
                ctx.log(
                    LogLevel::Info,
                    "sftp",
                    format!("transfer {id} finished: {dest}"),
                );
                emit(SftpEvent::TransferFinished {
                    transfer: id,
                    destination: dest,
                });
            }
            Ok(None) => emit(SftpEvent::TransferSkipped { transfer: id }),
            Err(e) => {
                // Never leave a partial file behind.
                let leftover = part
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.discard(leftover).await;
                if matches!(e, crate::Error::Cancelled) {
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

    /// Returns the final destination, or `None` when skipped.
    async fn run_transfer(
        &self,
        id: TransferId,
        request: &TransferRequest,
        part: &PartSlot,
        emit: &(impl Fn(SftpEvent) + Sync),
    ) -> crate::Result<Option<String>> {
        match request.direction {
            Direction::Download => self.download(id, request, part, emit).await,
            Direction::Upload => self.upload(id, request, part, emit).await,
        }
    }

    async fn download(
        &self,
        id: TransferId,
        request: &TransferRequest,
        part_slot: &PartSlot,
        emit: &(impl Fn(SftpEvent) + Sync),
    ) -> crate::Result<Option<String>> {
        let source = &request.remote;
        let meta = self
            .sftp
            .metadata(source)
            .await
            .map_err(|e| remote("read", source, &e))?;
        if meta.is_dir() {
            return Err(SftpError::NotAFile {
                path: source.clone(),
            }
            .into());
        }
        let Some(dest) = resolve_local_conflict(&request.local, request.on_conflict).await? else {
            return Ok(None);
        };
        let total = meta.size;
        emit(SftpEvent::TransferStarted {
            transfer: id,
            direction: Direction::Download,
            name: remote_name(source).to_owned(),
            total,
        });
        let mut file = self
            .sftp
            .open(source.as_str())
            .await
            .map_err(|e| remote("open", source, &e))?;
        let part = local_part(&dest);
        set_part(part_slot, Part::Local(part.clone()));
        let mut out = tokio::fs::File::create(&part)
            .await
            .map_err(|e| local("create", &part, e))?;
        let mut progress = Progress::new(id, total);
        let mut buf = vec![0_u8; CHUNK];
        loop {
            let n = file.read(&mut buf).await.map_err(|e| SftpError::Remote {
                op: "read",
                path: source.clone(),
                failure: RemoteFailure::Other,
                message: e.to_string(),
            })?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            out.write_all(chunk)
                .await
                .map_err(|e| local("write", &part, e))?;
            progress.advance(n, emit);
        }
        out.flush().await.map_err(|e| local("write", &part, e))?;
        out.sync_all().await.map_err(|e| local("write", &part, e))?;
        drop(out);
        let _ = file.shutdown().await;
        tokio::fs::rename(&part, &dest)
            .await
            .map_err(|e| local("rename", &part, e))?;
        clear_part(part_slot);
        progress.finish(emit);
        Ok(Some(dest.display().to_string()))
    }

    async fn upload(
        &self,
        id: TransferId,
        request: &TransferRequest,
        part_slot: &PartSlot,
        emit: &(impl Fn(SftpEvent) + Sync),
    ) -> crate::Result<Option<String>> {
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
            return Ok(None);
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
        let part = format!("{dest}{PART_SUFFIX}");
        set_part(part_slot, Part::Remote(part.clone()));
        let mut file = self
            .sftp
            .create(part.as_str())
            .await
            .map_err(|e| remote("create", &part, &e))?;
        let mut progress = Progress::new(id, total);
        let mut buf = vec![0_u8; CHUNK];
        loop {
            let n = input
                .read(&mut buf)
                .await
                .map_err(|e| local("read", source, e))?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            file.write_all(chunk).await.map_err(|e| SftpError::Remote {
                op: "write",
                path: part.clone(),
                failure: RemoteFailure::Other,
                message: e.to_string(),
            })?;
            progress.advance(n, emit);
        }
        file.shutdown().await.map_err(|e| SftpError::Remote {
            op: "write",
            path: part.clone(),
            failure: RemoteFailure::Other,
            message: e.to_string(),
        })?;
        // SFTP v3 rename refuses an existing destination: an overwrite was
        // already decided, remove it first.
        if self.exists(&dest).await {
            self.sftp
                .remove_file(dest.as_str())
                .await
                .map_err(|e| remote("replace", &dest, &e))?;
        }
        self.sftp
            .rename(part.as_str(), dest.as_str())
            .await
            .map_err(|e| remote("rename", &part, &e))?;
        clear_part(part_slot);
        progress.finish(emit);
        Ok(Some(dest))
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

async fn resolve_local_conflict(dest: &Path, policy: OnConflict) -> crate::Result<Option<PathBuf>> {
    let exists = |p: PathBuf| async move { tokio::fs::symlink_metadata(&p).await.is_ok() };
    if !exists(dest.to_owned()).await {
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
            let text = dest.to_string_lossy().into_owned();
            for n in 1..=999 {
                let candidate = PathBuf::from(numbered(&text, n));
                if !exists(candidate.clone()).await {
                    return Ok(Some(candidate));
                }
            }
            Err(SftpError::DestinationExists { path: text }.into())
        }
    }
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

fn local_part(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(PART_SUFFIX);
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
    fn local_part_appends_suffix() {
        assert_eq!(
            local_part(Path::new("/tmp/a.bin")),
            PathBuf::from("/tmp/a.bin.part")
        );
    }
}
