//! Session recording in the asciicast v2 format (phase 6).
//!
//! Recording is a **framed exception** to the rule "no terminal content in
//! clear text" (`docs/STACK.md`, ADR of 2026-10-22). Invariants:
//!
//! - **Opt-in per terminal** ([`crate::Engine::start_recording`]), never by
//!   default.
//! - **Output only**: the file holds `o` events (bytes received from the
//!   server, decoded as UTF-8 with replacement; a multi-byte sequence split
//!   across chunks is kept whole) and `r` events (`"COLSxROWS"`). What the
//!   user types is never written: the tap sits on the *output* path of the
//!   terminal task, after the bytes were handed to the UI.
//! - **Minimal header**: `version`, `width`, `height`, `timestamp`,
//!   `title` = alias, `env` = `{TERM}` only.
//! - **Private files**: `<data_dir>/recordings/` (`0700`, owned by the
//!   current user; a symlinked folder or one owned by someone else is
//!   refused; on Unix the folder is opened once with `O_NOFOLLOW` and checked
//!   and tightened through that descriptor), `<alias>-<UTC timestamp>.cast`
//!   created `0600` with `O_CREAT|O_EXCL|O_NOFOLLOW` relative to it (a
//!   planted file or symlink is never opened). Every created name passes
//!   [`validate_name`], so it can be listed, played and deleted.
//! - **The PTY never waits for the disk**: the terminal task pushes events
//!   with `try_send` into a bounded queue drained by a dedicated writer
//!   thread (`portico-rec`, one per recording, never a `spawn_blocking`
//!   slot). A full queue or a failed write **stops** the recording with a
//!   reason ([`RecordingStopReason`]) rather than leaving a file with holes.
//! - **Complete lines only**: the writer hands the file whole lines; after a
//!   failed write (disk full) the file is cut back to the last line that was
//!   fully written ([`truncate_to_committed`]).
//! - **Capped** at [`MAX_RECORDING_BYTES`]: the writer stops cleanly, the file
//!   ends on a complete line.
//! - **Playable**: widths and heights are clamped to
//!   [`MAX_PLAYBACK_DIMENSION`] when written, as when read.
//! - Times are seconds (`f64`) since the start, from a monotonic clock, never
//!   decreasing.
//! - No log line, event or error message carries recorded bytes: only
//!   names, paths, sizes and durations.
//!
//! Playback helpers: [`CastReader`] (streaming, bounded parser),
//! [`PlaybackTiming`] (idle cap and speed) and [`PlaybackSanitizer`] (a file
//! is untrusted: sequences acting outside the terminal are neutralised).

use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::mpsc;

use crate::engine::PtySize;
use crate::error::{Error, Result};
use crate::ids::{SessionId, TerminalId};

/// Largest recording written, and largest file accepted for playback.
pub const MAX_RECORDING_BYTES: u64 = 512 * 1024 * 1024;
/// Longest line accepted by the parser (the writer never produces more).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Events queued between the terminal task and the writer. Beyond, the
/// recording stops (`queue_full`): the PTY is never slowed down.
pub const RECORDING_QUEUE_CAPACITY: usize = 512;
/// Silences longer than this are shortened to it during playback.
pub const PLAYBACK_IDLE_CAP: Duration = Duration::from_secs(2);
/// Slowest playback speed.
pub const MIN_PLAYBACK_SPEED: f64 = 0.5;
/// Fastest playback speed.
pub const MAX_PLAYBACK_SPEED: f64 = 8.0;
/// Largest width or height accepted in a file (an emulator allocates the
/// grid: a hostile `65535x65535` must not reach it).
pub const MAX_PLAYBACK_DIMENSION: u16 = 1000;
/// Extension of recording files.
pub const CAST_EXTENSION: &str = "cast";

/// Characters kept from an alias in a file name.
const MAX_ALIAS_CHARS: usize = 64;
/// Same-second recordings of one alias get `-2`, `-3`… up to this.
const NAME_ATTEMPTS: u32 = 100;
/// Longest file name accepted by [`validate_name`].
const MAX_NAME_BYTES: usize = 255;
/// Recordings returned by a listing.
const MAX_LISTED: usize = 10_000;
/// Bytes read from the end of a file to find its duration.
const TAIL_PROBE_BYTES: u64 = 64 * 1024;
/// Bytes buffered by [`CastWriter`] before they are handed to the file (whole
/// lines only).
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
/// Output text per `o` event: even fully escaped (`\u001b` = 6 bytes per
/// byte) a line stays well under [`MAX_LINE_BYTES`].
const MAX_OUTPUT_PER_EVENT: usize = 64 * 1024;
/// Title kept from a file header (shown in lists).
const MAX_TITLE_CHARS: usize = 256;
/// Body of a kept OSC sequence (colour settings are short).
const MAX_KEPT_OSC: usize = 512;
/// Digits read for an OSC number.
const MAX_OSC_DIGITS: usize = 5;

// ── Errors ──────────────────────────────────────────────────────────────

/// Recording, listing and playback failures. Messages never quote recorded
/// content (only names, paths and line numbers).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecordingError {
    #[error("unknown or closed terminal {0}")]
    UnknownTerminal(TerminalId),
    #[error("terminal {0} is already being recorded")]
    AlreadyRecording(TerminalId),
    #[error("terminal {0} is not being recorded")]
    NotRecording(TerminalId),
    #[error("{name:?} is not a recording name")]
    InvalidName { name: String },
    #[error("recording {name} does not exist")]
    NotFound { name: String },
    #[error("{} is a symlink or not a regular file or folder: refused", path.display())]
    UnsafePath { path: PathBuf },
    #[error("{} is not owned by the current user: refused", path.display())]
    ForeignOwner { path: PathBuf },
    #[error("recording {name} is still being written")]
    Active { name: String },
    #[error("cannot create the recording {}: {source}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("recording {name} is larger than {max} bytes")]
    TooLarge { name: String, max: u64 },
    #[error("line {line} of recording {name} is longer than {max} bytes")]
    LineTooLong { name: String, line: u64, max: usize },
    #[error("invalid asciicast header in {name}: {reason}")]
    InvalidHeader { name: String, reason: &'static str },
    #[error("{name} is asciicast version {version}; only version 2 is supported")]
    UnsupportedVersion { name: String, version: String },
    #[error("malformed event on line {line} of {name}: {reason}")]
    MalformedEvent {
        name: String,
        line: u64,
        reason: &'static str,
    },
    #[error("time goes backwards on line {line} of {name}")]
    TimeWentBackwards { name: String, line: u64 },
    #[error("cannot read recording {name}: {source}")]
    Read {
        name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot delete recording {name}: {source}")]
    Delete {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

impl RecordingError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::UnknownTerminal(_) => (
                "recording_unknown_terminal",
                Some("The terminal is closed: open it again, then start recording."),
            ),
            Self::AlreadyRecording(_) => (
                "recording_already_active",
                Some("This terminal is already recorded: stop the current recording first."),
            ),
            Self::NotRecording(_) => ("recording_not_active", None),
            Self::InvalidName { .. } => (
                "recording_invalid_name",
                Some("Pick a recording from the list: only names ending in .cast are accepted."),
            ),
            Self::NotFound { .. } => (
                "recording_not_found",
                Some("It may have been deleted meanwhile: refresh the list."),
            ),
            Self::UnsafePath { .. } => (
                "recording_unsafe_path",
                Some(
                    "Portico refuses symlinks in its recordings folder: replace the link with a regular folder or file.",
                ),
            ),
            Self::ForeignOwner { .. } => (
                "recording_foreign_owner",
                Some(
                    "Another user owns the recordings folder: remove it or give it back to your account.",
                ),
            ),
            Self::Active { .. } => (
                "recording_active",
                Some("Stop the recording first, then delete it."),
            ),
            Self::Create { .. } => (
                "recording_create_failed",
                Some("Check the free disk space and the permissions of the recordings folder."),
            ),
            Self::TooLarge { .. } => (
                "recording_too_large",
                Some("Portico plays recordings up to 512 MiB."),
            ),
            Self::LineTooLong { .. } => (
                "recording_line_too_long",
                Some("The file was not written by Portico or asciinema, or it is damaged."),
            ),
            Self::InvalidHeader { .. } => (
                "recording_invalid_header",
                Some("This is not an asciicast v2 file, or it is damaged."),
            ),
            Self::UnsupportedVersion { .. } => (
                "recording_unsupported_version",
                Some("Only asciicast v2 can be played: convert it with `asciinema convert`."),
            ),
            Self::MalformedEvent { .. } => (
                "recording_malformed",
                Some("The file is damaged from this line on; what precedes it can be played."),
            ),
            Self::TimeWentBackwards { .. } => (
                "recording_time_backwards",
                Some("The file was edited or damaged: event times must never decrease."),
            ),
            Self::Read { .. } => ("recording_io", None),
            Self::Delete { .. } => (
                "recording_delete_failed",
                Some("Check the permissions of the recordings folder."),
            ),
        }
    }
}

fn rec_err(e: RecordingError) -> Error {
    Error::Recording(e)
}

// ── Public types ────────────────────────────────────────────────────────

/// Why a recording stopped. Stable codes ([`RecordingStopReason::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecordingStopReason {
    /// [`crate::Engine::stop_recording`].
    Requested,
    /// The terminal closed (exit, disconnect, view dropped, shutdown).
    TerminalClosed,
    /// The writer fell behind the terminal: stopped rather than slow the PTY.
    QueueFull,
    /// Writing to the file failed (disk full, I/O error).
    WriteFailed,
    /// [`MAX_RECORDING_BYTES`] reached.
    SizeLimit,
}

impl RecordingStopReason {
    /// Stable `snake_case` code (same as the serialized form).
    pub fn code(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::TerminalClosed => "terminal_closed",
            Self::QueueFull => "queue_full",
            Self::WriteFailed => "write_failed",
            Self::SizeLimit => "size_limit",
        }
    }
}

/// A recording that just started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    pub terminal: TerminalId,
    pub session: SessionId,
    /// File name, the id used by [`crate::Engine::open_recording`] and
    /// [`crate::Engine::delete_recording`].
    pub name: String,
    pub path: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
}

/// A recording file, as listed by [`crate::Engine::list_recordings`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEntry {
    pub name: String,
    /// Header title (sanitised), or the file name without its extension.
    pub alias: String,
    /// Header timestamp.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started: Option<OffsetDateTime>,
    pub size: u64,
    /// Time of the last event, when it could be read cheaply.
    pub duration: Option<f64>,
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// The header is a valid asciicast v2 header.
    pub valid: bool,
}

/// Header of a recording, as validated by [`CastReader`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CastHeader {
    pub width: u16,
    pub height: u16,
    /// Unix time of the start.
    pub timestamp: Option<i64>,
    /// Sanitised: control characters replaced, at most 256 characters.
    pub title: Option<String>,
    /// `env.TERM`, sanitised like the title.
    pub term: Option<String>,
}

/// One playback event.
#[derive(Clone, PartialEq)]
pub enum CastEvent {
    /// Terminal output, **untrusted**: pass it through
    /// [`PlaybackSanitizer`] before the emulator.
    Output(String),
    Resize {
        cols: u16,
        rows: u16,
    },
}

// Output is recorded terminal content: never in `Debug`.
impl fmt::Debug for CastEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(text) => write!(f, "Output({} bytes)", text.len()),
            Self::Resize { cols, rows } => write!(f, "Resize({cols}x{rows})"),
        }
    }
}

/// A playback event and its time (seconds since the start).
#[derive(Debug, Clone, PartialEq)]
pub struct TimedEvent {
    pub time: f64,
    pub event: CastEvent,
}

// ── Names and paths ─────────────────────────────────────────────────────

/// `alias` made safe for a file name: `[A-Za-z0-9._-]` kept, anything else
/// becomes `_`, runs of `.` collapse to one (a name never holds `..`, which
/// [`validate_name`] refuses), no leading `.` or `-`, at most 64 characters,
/// `session` if nothing is left.
pub fn sanitize_alias(alias: &str) -> String {
    let mut mapped = String::with_capacity(alias.len());
    for c in alias.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            c
        } else {
            '_'
        };
        if !(c == '.' && mapped.ends_with('.')) {
            mapped.push(c);
        }
    }
    let trimmed: String = mapped
        .trim_start_matches(['.', '-'])
        .chars()
        .take(MAX_ALIAS_CHARS)
        .collect();
    if trimmed.is_empty() {
        "session".to_owned()
    } else {
        trimmed
    }
}

/// `20261022T113005Z`.
fn name_timestamp(at: OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// Checks that `name` is a plain recording file name: `[A-Za-z0-9._-]`,
/// ending in `.cast`, not starting with `.`, no `..`: it can only name a
/// file directly inside the recordings folder.
pub fn validate_name(name: &str) -> std::result::Result<(), RecordingError> {
    let invalid = || RecordingError::InvalidName {
        name: name.chars().take(MAX_NAME_BYTES).collect(),
    };
    let suffix = format!(".{CAST_EXTENSION}");
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.len() > suffix.len()
        && name.ends_with(&suffix)
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok { Ok(()) } else { Err(invalid()) }
}

/// The recordings folder exists and is a real folder (`false` if missing).
fn check_dir(dir: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(rec_err(RecordingError::UnsafePath {
            path: dir.to_path_buf(),
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::io(dir, e)),
    }
}

/// The recordings folder, checked. On Unix it is held open: the checks and
/// the `chmod` go through the descriptor (never a path that could be swapped
/// meanwhile), and files are created relative to it.
#[derive(Debug)]
struct PrivateDir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

/// Creates the recordings folder (`0700`) or checks the existing one: a
/// symlink, a non-folder or a folder owned by another user is refused, loose
/// permissions are tightened.
fn ensure_private_dir(dir: &Path) -> Result<PrivateDir> {
    if !check_dir(dir)? {
        crate::store::create_private_dir_all(dir)?;
    }
    open_private_dir(dir)
}

#[cfg(unix)]
fn open_private_dir(dir: &Path) -> Result<PrivateDir> {
    use rustix::fs::{FileType, Mode, OFlags};
    let unsafe_path = || {
        rec_err(RecordingError::UnsafePath {
            path: dir.to_path_buf(),
        })
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(dir, flags, Mode::empty()) {
        Ok(fd) => fd,
        // A symlink (`ELOOP`) or not a folder: swapped after the check.
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => return Err(unsafe_path()),
        Err(e) => return Err(Error::io(dir, e.into())),
    };
    let stat = rustix::fs::fstat(&fd).map_err(|e| Error::io(dir, e.into()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(unsafe_path());
    }
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(rec_err(RecordingError::ForeignOwner {
            path: dir.to_path_buf(),
        }));
    }
    if Mode::from_raw_mode(stat.st_mode).intersects(Mode::RWXG | Mode::RWXO) {
        rustix::fs::fchmod(&fd, Mode::RWXU).map_err(|e| Error::io(dir, e.into()))?;
    }
    Ok(PrivateDir {
        path: dir.to_path_buf(),
        fd,
    })
}

#[cfg(not(unix))]
fn open_private_dir(dir: &Path) -> Result<PrivateDir> {
    if !check_dir(dir)? {
        return Err(rec_err(RecordingError::UnsafePath {
            path: dir.to_path_buf(),
        }));
    }
    Ok(PrivateDir {
        path: dir.to_path_buf(),
    })
}

impl PrivateDir {
    /// A new `0600` file `name` in the folder, `O_EXCL` (never an existing
    /// file nor a symlink).
    #[cfg(unix)]
    fn create_new(&self, name: &str) -> std::io::Result<File> {
        use rustix::fs::{Mode, OFlags};
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.fd, name, flags, Mode::RUSR | Mode::WUSR)?;
        Ok(File::from(fd))
    }

    #[cfg(not(unix))]
    fn create_new(&self, name: &str) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
    }

    /// Removes `name` from the folder (best effort: cleanup after a failure).
    fn remove(&self, name: &str) {
        #[cfg(unix)]
        let _ = rustix::fs::unlinkat(&self.fd, name, rustix::fs::AtFlags::empty());
        #[cfg(not(unix))]
        let _ = std::fs::remove_file(self.path.join(name));
    }
}

/// Opens an existing file for reading without following a symlink and
/// without blocking on a FIFO.
fn open_nofollow(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        opts.custom_flags(i32::try_from(flags.bits()).unwrap_or(0));
    }
    opts.open(path)
}

/// A freshly created recording file, header written.
#[derive(Debug)]
pub(crate) struct NewRecording {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) writer: CastWriter<File>,
}

/// Creates `<dir>/<alias>-<timestamp>.cast` and writes its header. The name
/// always passes [`validate_name`] (checked: a name the listing would hide
/// is never created).
pub(crate) fn create_recording(
    dir: &Path,
    alias: &str,
    at: OffsetDateTime,
    size: PtySize,
    max_bytes: u64,
) -> Result<NewRecording> {
    let folder = ensure_private_dir(dir)?;
    let base = format!("{}-{}", sanitize_alias(alias), name_timestamp(at));
    let mut attempt = 1u32;
    let (name, path, file) = loop {
        let name = if attempt == 1 {
            format!("{base}.{CAST_EXTENSION}")
        } else {
            format!("{base}-{attempt}.{CAST_EXTENSION}")
        };
        validate_name(&name).map_err(rec_err)?;
        let path = folder.path.join(&name);
        match folder.create_new(&name) {
            Ok(file) => break (name, path, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < NAME_ATTEMPTS => {
                attempt = attempt.saturating_add(1);
            }
            Err(source) => return Err(rec_err(RecordingError::Create { path, source })),
        }
    };
    let mut writer = CastWriter::new(file, max_bytes);
    let header = header_line(alias, size, at);
    if let Err(source) = writer.write_line(&header).and_then(|()| writer.flush()) {
        drop(writer);
        folder.remove(&name);
        return Err(rec_err(RecordingError::Create {
            path,
            source: source.into_io(),
        }));
    }
    Ok(NewRecording { name, path, writer })
}

/// A width or height as written: within `1..=MAX_PLAYBACK_DIMENSION`, so
/// that the reader (which refuses more) can always play the file back.
fn playable(dimension: u16) -> u16 {
    dimension.clamp(1, MAX_PLAYBACK_DIMENSION)
}

/// The JSON header line (no newline).
fn header_line(alias: &str, size: PtySize, at: OffsetDateTime) -> String {
    let header = serde_json::json!({
        "version": 2,
        "width": playable(size.cols),
        "height": playable(size.rows),
        "timestamp": at.unix_timestamp(),
        "title": alias,
        "env": { "TERM": crate::session::terminal::TERM },
    });
    header.to_string()
}

// ── Writer ──────────────────────────────────────────────────────────────

/// What the terminal task hands to the writer.
pub(crate) enum TapEvent {
    Output { at: f64, bytes: Bytes },
    Resize { at: f64, cols: u16, rows: u16 },
}

// Output is terminal content: never in `Debug`.
impl fmt::Debug for TapEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output { at, bytes } => write!(f, "Output({at}s, {} bytes)", bytes.len()),
            Self::Resize { at, cols, rows } => write!(f, "Resize({at}s, {cols}x{rows})"),
        }
    }
}

/// A writer failure: the cap, or an I/O error.
#[derive(Debug)]
pub(crate) enum WriteStop {
    SizeLimit,
    Io(std::io::Error),
}

impl WriteStop {
    pub(crate) fn reason(&self) -> RecordingStopReason {
        match self {
            Self::SizeLimit => RecordingStopReason::SizeLimit,
            Self::Io(_) => RecordingStopReason::WriteFailed,
        }
    }

    fn into_io(self) -> std::io::Error {
        match self {
            Self::SizeLimit => std::io::Error::other("recording size limit reached"),
            Self::Io(e) => e,
        }
    }
}

/// Incremental UTF-8 decoding: invalid bytes become U+FFFD, a sequence cut
/// at the end of a chunk waits for the next one.
#[derive(Debug, Default)]
pub(crate) struct Utf8Stream {
    /// At most 3 bytes: the start of an incomplete sequence.
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub(crate) fn decode(&mut self, bytes: &[u8]) -> String {
        let joined;
        let input: &[u8] = if self.pending.is_empty() {
            bytes
        } else {
            let mut buf = std::mem::take(&mut self.pending);
            buf.extend_from_slice(bytes);
            joined = buf;
            &joined
        };
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        loop {
            match std::str::from_utf8(rest) {
                Ok(text) => {
                    out.push_str(text);
                    break;
                }
                Err(e) => {
                    let Some((valid, after)) = rest.split_at_checked(e.valid_up_to()) else {
                        break;
                    };
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    if let Some(len) = e.error_len() {
                        out.push('\u{FFFD}');
                        rest = after.get(len..).unwrap_or_default();
                    } else {
                        self.pending = after.to_vec();
                        break;
                    }
                }
            }
        }
        out
    }

    /// An incomplete sequence left at the end becomes U+FFFD.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            self.pending.clear();
            Some("\u{FFFD}".to_owned())
        }
    }
}

/// Writes asciicast lines, counting bytes against the cap.
///
/// Lines are gathered in a buffer of whole lines and handed to the sink with
/// one `write_all` per batch: [`CastWriter::written`] counts the bytes of
/// the batches that reached the sink entirely, so it always ends on a line
/// boundary, even after a failed write.
pub(crate) struct CastWriter<W: Write> {
    sink: W,
    /// Complete lines not yet handed to the sink.
    pending: Vec<u8>,
    /// Bytes that reached the sink (whole lines).
    committed: u64,
    max_bytes: u64,
    utf8: Utf8Stream,
    /// Time of the last event written.
    last_time: f64,
}

impl<W: Write> fmt::Debug for CastWriter<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CastWriter")
            .field("committed", &self.committed)
            .field("pending", &self.pending.len())
            .field("max_bytes", &self.max_bytes)
            .field("last_time", &self.last_time)
            .finish_non_exhaustive()
    }
}

/// Rounded to the microsecond (rounding keeps the order).
fn round_time(at: f64) -> f64 {
    if at.is_finite() && at > 0.0 {
        (at * 1e6).round() / 1e6
    } else {
        0.0
    }
}

fn byte_len(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

impl<W: Write> CastWriter<W> {
    pub(crate) fn new(sink: W, max_bytes: u64) -> Self {
        Self {
            sink,
            pending: Vec::new(),
            committed: 0,
            max_bytes,
            utf8: Utf8Stream::default(),
            last_time: 0.0,
        }
    }

    pub(crate) fn last_time(&self) -> f64 {
        self.last_time
    }

    /// Bytes of complete lines that reached the sink.
    pub(crate) fn written(&self) -> u64 {
        self.committed
    }

    /// The sink, without writing what is still buffered.
    pub(crate) fn into_sink(self) -> W {
        self.sink
    }

    /// One line plus `\n`, refused whole if it would pass the cap.
    fn write_line(&mut self, line: &str) -> std::result::Result<(), WriteStop> {
        let len = byte_len(line.len()).saturating_add(1);
        let total = self
            .committed
            .saturating_add(byte_len(self.pending.len()))
            .saturating_add(len);
        if total > self.max_bytes {
            return Err(WriteStop::SizeLimit);
        }
        self.pending.extend_from_slice(line.as_bytes());
        self.pending.push(b'\n');
        if self.pending.len() >= WRITE_BUFFER_BYTES {
            self.write_pending()?;
        }
        Ok(())
    }

    /// Hands the buffered lines to the sink. On failure part of them may
    /// have reached it: [`CastWriter::written`] does not count them.
    fn write_pending(&mut self) -> std::result::Result<(), WriteStop> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.pending);
        self.sink.write_all(&batch).map_err(WriteStop::Io)?;
        self.committed = self.committed.saturating_add(byte_len(batch.len()));
        Ok(())
    }

    fn write_output(&mut self, at: f64, text: &str) -> std::result::Result<(), WriteStop> {
        let mut rest = text;
        while !rest.is_empty() {
            let mut cut = rest.len().min(MAX_OUTPUT_PER_EVENT);
            while !rest.is_char_boundary(cut) {
                cut = cut.saturating_sub(1);
            }
            let (piece, after) = rest.split_at_checked(cut).unwrap_or((rest, ""));
            let line = serde_json::to_string(&(at, "o", piece))
                .map_err(|e| WriteStop::Io(std::io::Error::other(e)))?;
            self.write_line(&line)?;
            rest = after;
        }
        self.last_time = at;
        Ok(())
    }

    pub(crate) fn write_event(&mut self, event: TapEvent) -> std::result::Result<(), WriteStop> {
        match event {
            TapEvent::Output { at, bytes } => {
                let at = round_time(at).max(self.last_time);
                let text = self.utf8.decode(&bytes);
                self.write_output(at, &text)
            }
            TapEvent::Resize { at, cols, rows } => {
                let at = round_time(at).max(self.last_time);
                let size = format!("{}x{}", playable(cols), playable(rows));
                let line = serde_json::to_string(&(at, "r", size))
                    .map_err(|e| WriteStop::Io(std::io::Error::other(e)))?;
                self.write_line(&line)?;
                self.last_time = at;
                Ok(())
            }
        }
    }

    pub(crate) fn flush(&mut self) -> std::result::Result<(), WriteStop> {
        self.write_pending()?;
        self.sink.flush().map_err(WriteStop::Io)
    }

    /// Writes what the UTF-8 decoder still holds and flushes. After an
    /// `Io` error, cut the file back with [`truncate_to_committed`].
    pub(crate) fn finish_in_place(&mut self) -> std::result::Result<(), WriteStop> {
        let tail = self.utf8.finish();
        let result = match tail {
            Some(text) => self.write_output(self.last_time, &text),
            None => Ok(()),
        };
        let flushed = self.flush();
        result.and(flushed)
    }

    /// [`CastWriter::finish_in_place`], then returns the sink.
    #[cfg(test)]
    pub(crate) fn finish(mut self) -> (W, std::result::Result<(), WriteStop>) {
        let result = self.finish_in_place();
        (self.sink, result)
    }
}

/// A sink that can be cut back to a length (`ftruncate` for a file).
pub(crate) trait Truncate {
    fn truncate_to(&mut self, len: u64) -> std::io::Result<()>;
}

impl Truncate for File {
    fn truncate_to(&mut self, len: u64) -> std::io::Result<()> {
        self.set_len(len)
    }
}

/// After a failed write (disk full…): drops what is still buffered and cuts
/// the sink back to the complete lines that reached it, so a partly written
/// last line never remains. Returns the sink.
pub(crate) fn truncate_to_committed<W: Write + Truncate>(
    writer: CastWriter<W>,
) -> std::io::Result<W> {
    let written = writer.written();
    let mut sink = writer.into_sink();
    sink.truncate_to(written)?;
    Ok(sink)
}

/// Drains `rx` into `writer` until the queue closes (`None`) or the writer
/// fails (its reason; the queue is then closed so the tap notices). Blocking:
/// runs on a dedicated blocking thread.
pub(crate) fn run_writer<W: Write>(
    rx: &mut mpsc::Receiver<TapEvent>,
    writer: &mut CastWriter<W>,
) -> Option<WriteStop> {
    while let Some(event) = rx.blocking_recv() {
        let mut result = writer.write_event(event);
        while result.is_ok() {
            match rx.try_recv() {
                Ok(event) => result = writer.write_event(event),
                Err(_) => break,
            }
        }
        // The queue is empty: what was written reaches the file now.
        if let Err(stop) = result.and_then(|()| writer.flush()) {
            rx.close();
            return Some(stop);
        }
    }
    None
}

// ── Tap (terminal side) ─────────────────────────────────────────────────

/// Set-once stop reason shared by the tap and the writer.
#[derive(Debug, Default)]
pub(crate) struct StopCell(Mutex<Option<RecordingStopReason>>);

impl StopCell {
    pub(crate) fn set(&self, reason: RecordingStopReason) {
        let mut slot = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(reason);
        }
    }

    pub(crate) fn get(&self) -> Option<RecordingStopReason> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug)]
struct ActiveRecording {
    tx: mpsc::Sender<TapEvent>,
    stop: Arc<StopCell>,
    started: Instant,
    last: f64,
}

impl ActiveRecording {
    /// Monotonic, never decreasing.
    fn now(&mut self) -> f64 {
        let at = self.started.elapsed().as_secs_f64().max(self.last);
        self.last = at;
        at
    }
}

#[derive(Debug)]
struct TapState {
    size: PtySize,
    closed: bool,
    starting: bool,
    active: Option<ActiveRecording>,
}

/// The recording hook of one terminal, shared by its reader (output) and
/// writer (resizes) tasks and the engine. Every call is a short, non-
/// blocking critical section: the PTY never waits on the recording.
#[derive(Debug)]
pub(crate) struct TerminalTap {
    state: Mutex<TapState>,
}

impl TerminalTap {
    pub(crate) fn new(size: PtySize) -> Self {
        Self {
            state: Mutex::new(TapState {
                size,
                closed: false,
                starting: false,
                active: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TapState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands `event` to the writer without waiting; stops the recording if
    /// the queue is full (`queue_full`) or the writer is gone.
    fn push(state: &mut TapState, make: impl FnOnce(f64) -> TapEvent) {
        let Some(active) = state.active.as_mut() else {
            return;
        };
        let event = make(active.now());
        match active.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                active.stop.set(RecordingStopReason::QueueFull);
                state.active = None;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => state.active = None,
        }
    }

    /// Output that was just handed to the UI.
    pub(crate) fn output(&self, bytes: &Bytes) {
        let mut state = self.lock();
        Self::push(&mut state, |at| TapEvent::Output {
            at,
            bytes: bytes.clone(),
        });
    }

    /// A size sent to the server.
    pub(crate) fn resize(&self, size: PtySize) {
        let mut state = self.lock();
        state.size = size;
        Self::push(&mut state, |at| TapEvent::Resize {
            at,
            cols: size.cols,
            rows: size.rows,
        });
    }

    /// Reserves the terminal for a new recording. The reservation releases
    /// the terminal when dropped unless it was activated: an error or a
    /// cancelled start never leaves the terminal stuck in "starting".
    pub(crate) fn begin(
        self: &Arc<Self>,
        id: TerminalId,
    ) -> std::result::Result<StartReservation, RecordingError> {
        let mut state = self.lock();
        if state.closed {
            return Err(RecordingError::UnknownTerminal(id));
        }
        let running = state.active.as_ref().is_some_and(|a| !a.tx.is_closed());
        if state.starting || running {
            return Err(RecordingError::AlreadyRecording(id));
        }
        state.active = None;
        state.starting = true;
        Ok(StartReservation {
            tap: Some(Arc::clone(self)),
            size: state.size,
        })
    }

    /// Releases a reservation that did not lead to a recording.
    fn abort_start(&self) {
        self.lock().starting = false;
    }

    /// Starts feeding `tx`. `header_size` is the size written in the header:
    /// a resize that happened meanwhile is recorded at time 0. Returns
    /// `false` if the terminal closed meanwhile (`tx` is dropped, so the
    /// writer ends with `terminal_closed`).
    fn activate(
        &self,
        tx: mpsc::Sender<TapEvent>,
        stop: Arc<StopCell>,
        header_size: PtySize,
    ) -> bool {
        let mut state = self.lock();
        state.starting = false;
        if state.closed {
            stop.set(RecordingStopReason::TerminalClosed);
            return false;
        }
        state.active = Some(ActiveRecording {
            tx,
            stop,
            started: Instant::now(),
            last: 0.0,
        });
        let size = state.size;
        if (size.cols, size.rows) != (header_size.cols, header_size.rows) {
            Self::push(&mut state, |at| TapEvent::Resize {
                at,
                cols: size.cols,
                rows: size.rows,
            });
        }
        true
    }

    /// Stops the running recording with `reason`. `false` if none was
    /// running.
    pub(crate) fn stop(&self, reason: RecordingStopReason) -> bool {
        let mut state = self.lock();
        match state.active.take() {
            Some(active) if !active.tx.is_closed() => {
                active.stop.set(reason);
                true
            }
            _ => false,
        }
    }

    /// The terminal is gone: stops any recording (`terminal_closed`) and
    /// refuses new ones.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        if let Some(active) = state.active.take() {
            active.stop.set(RecordingStopReason::TerminalClosed);
        }
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.lock()
            .active
            .as_ref()
            .is_some_and(|a| !a.tx.is_closed())
    }
}

/// A terminal reserved for a recording being started (RAII): dropped without
/// [`StartReservation::activate`] (creation failed, start cancelled, engine
/// shut down), it releases the terminal.
#[derive(Debug)]
pub(crate) struct StartReservation {
    tap: Option<Arc<TerminalTap>>,
    size: PtySize,
}

impl StartReservation {
    /// The terminal size when reserved (written in the header).
    pub(crate) fn size(&self) -> PtySize {
        self.size
    }

    /// Starts feeding `tx` (see [`TerminalTap`]): `false` if the terminal
    /// closed meanwhile. Consumes the reservation, so it is never released
    /// twice.
    pub(crate) fn activate(mut self, tx: mpsc::Sender<TapEvent>, stop: Arc<StopCell>) -> bool {
        match self.tap.take() {
            Some(tap) => tap.activate(tx, stop, self.size),
            None => false,
        }
    }
}

impl Drop for StartReservation {
    fn drop(&mut self) {
        if let Some(tap) = self.tap.take() {
            tap.abort_start();
        }
    }
}

// ── Reader ──────────────────────────────────────────────────────────────

/// Unicode general category `Cf` (format characters: bidi marks and
/// overrides, zero-width characters, invisible operators, tags…), as of
/// Unicode 16. They are invisible yet change how a label reads.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// A header field shown in lists: control characters (C0, DEL, C1)
/// replaced, format characters (`Cf`: bidi marks and overrides, zero-width
/// characters) and line / paragraph separators (U+2028, U+2029) dropped, at
/// most `max` characters kept.
fn sanitize_label(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !is_format_char(*c) && !matches!(c, '\u{2028}' | '\u{2029}'))
        .take(max)
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

fn dimension(value: Option<&Value>) -> Option<u16> {
    value
        .and_then(Value::as_u64)
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| (1..=MAX_PLAYBACK_DIMENSION).contains(v))
}

fn parse_header(name: &str, line: &[u8]) -> std::result::Result<CastHeader, RecordingError> {
    let invalid = |reason| RecordingError::InvalidHeader {
        name: name.to_owned(),
        reason,
    };
    let value: Value = serde_json::from_slice(line).map_err(|_| invalid("not JSON"))?;
    let Value::Object(map) = value else {
        return Err(invalid("not a JSON object"));
    };
    match map.get("version") {
        Some(Value::Number(n)) if n.as_u64() == Some(2) => {}
        Some(Value::Number(n)) => {
            return Err(RecordingError::UnsupportedVersion {
                name: name.to_owned(),
                version: n.to_string(),
            });
        }
        Some(_) => return Err(invalid("version is not a number")),
        None => return Err(invalid("no version")),
    }
    let width =
        dimension(map.get("width")).ok_or_else(|| invalid("width missing or out of range"))?;
    let height =
        dimension(map.get("height")).ok_or_else(|| invalid("height missing or out of range"))?;
    let timestamp = map.get("timestamp").and_then(Value::as_i64);
    let title = map
        .get("title")
        .and_then(Value::as_str)
        .map(|t| sanitize_label(t, MAX_TITLE_CHARS));
    let term = map
        .get("env")
        .and_then(|env| env.get("TERM"))
        .and_then(Value::as_str)
        .map(|t| sanitize_label(t, MAX_TITLE_CHARS));
    Ok(CastHeader {
        width,
        height,
        timestamp,
        title,
        term,
    })
}

/// `"COLSxROWS"`.
fn parse_size(text: &str) -> Option<(u16, u16)> {
    let (cols, rows) = text.split_once('x')?;
    let parse = |s: &str| {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<u16>()
            .ok()
            .filter(|v| (1..=MAX_PLAYBACK_DIMENSION).contains(v))
    };
    Some((parse(cols)?, parse(rows)?))
}

/// Streaming asciicast v2 parser. Bounded: the whole input
/// ([`MAX_RECORDING_BYTES`]), each line ([`MAX_LINE_BYTES`]). Unknown event
/// types are skipped, times must not decrease, empty lines are ignored.
/// After an error the iterator ends.
///
/// Reading is blocking I/O: iterate on a blocking thread
/// (`spawn_blocking`) or from a synchronous UI loop, never in an async task.
pub struct CastReader<R> {
    reader: R,
    name: String,
    header: CastHeader,
    line: u64,
    consumed: u64,
    max_bytes: u64,
    last_time: f64,
    buf: Vec<u8>,
    done: bool,
}

impl<R> fmt::Debug for CastReader<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CastReader")
            .field("name", &self.name)
            .field("header", &self.header)
            .field("line", &self.line)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// The reader [`crate::Engine::open_recording`] returns.
pub type RecordingReader = CastReader<BufReader<File>>;

impl<R: BufRead> CastReader<R> {
    /// Reads and validates the header. `name` is used in errors.
    pub fn new(reader: R, name: &str) -> Result<Self> {
        Self::with_limit(reader, name, MAX_RECORDING_BYTES)
    }

    /// [`CastReader::new`] with another size limit.
    pub fn with_limit(reader: R, name: &str, max_bytes: u64) -> Result<Self> {
        let mut this = Self {
            reader,
            name: name.to_owned(),
            header: CastHeader {
                width: 1,
                height: 1,
                timestamp: None,
                title: None,
                term: None,
            },
            line: 0,
            consumed: 0,
            max_bytes,
            last_time: 0.0,
            buf: Vec::new(),
            done: false,
        };
        let header = match this.read_line().map_err(rec_err)? {
            None => Err(RecordingError::InvalidHeader {
                name: name.to_owned(),
                reason: "empty file",
            }),
            Some(()) if this.buf.is_empty() => Err(RecordingError::InvalidHeader {
                name: name.to_owned(),
                reason: "empty first line",
            }),
            Some(()) => parse_header(name, &this.buf),
        }
        .map_err(rec_err)?;
        this.header = header;
        Ok(this)
    }

    pub fn header(&self) -> &CastHeader {
        &self.header
    }

    /// Reads the next line into `buf` (newline and `\r` removed).
    fn read_line(&mut self) -> std::result::Result<Option<()>, RecordingError> {
        self.buf.clear();
        let limit = u64::try_from(MAX_LINE_BYTES)
            .unwrap_or(u64::MAX)
            .saturating_add(2);
        let read = (&mut self.reader)
            .take(limit)
            .read_until(b'\n', &mut self.buf)
            .map_err(|source| RecordingError::Read {
                name: self.name.clone(),
                source,
            })?;
        if read == 0 {
            return Ok(None);
        }
        self.line = self.line.saturating_add(1);
        self.consumed = self
            .consumed
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if self.consumed > self.max_bytes {
            return Err(RecordingError::TooLarge {
                name: self.name.clone(),
                max: self.max_bytes,
            });
        }
        if self.buf.last() == Some(&b'\n') {
            self.buf.pop();
        }
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(RecordingError::LineTooLong {
                name: self.name.clone(),
                line: self.line,
                max: MAX_LINE_BYTES,
            });
        }
        Ok(Some(()))
    }

    /// Parses the line in `buf`; `None` for an event type that is skipped.
    fn parse_event(&mut self) -> std::result::Result<Option<TimedEvent>, RecordingError> {
        let malformed = |reason| RecordingError::MalformedEvent {
            name: self.name.clone(),
            line: self.line,
            reason,
        };
        let value: Value = serde_json::from_slice(&self.buf).map_err(|_| malformed("not JSON"))?;
        let Value::Array(items) = value else {
            return Err(malformed("not a JSON array"));
        };
        let mut items = items.into_iter();
        let (Some(time), Some(kind), Some(data)) = (items.next(), items.next(), items.next())
        else {
            return Err(malformed("fewer than three fields"));
        };
        let time = time
            .as_f64()
            .filter(|t| t.is_finite() && *t >= 0.0)
            .ok_or_else(|| malformed("invalid time"))?;
        if time < self.last_time {
            return Err(RecordingError::TimeWentBackwards {
                name: self.name.clone(),
                line: self.line,
            });
        }
        let Value::String(kind) = kind else {
            return Err(malformed("event type is not a string"));
        };
        let event = match kind.as_str() {
            "o" => match data {
                Value::String(text) => CastEvent::Output(text),
                _ => return Err(malformed("output data is not a string")),
            },
            "r" => {
                let (cols, rows) = data
                    .as_str()
                    .and_then(parse_size)
                    .ok_or_else(|| malformed("invalid resize"))?;
                CastEvent::Resize { cols, rows }
            }
            // `i` (input), `m` (marker) and future types: not played.
            _ => {
                self.last_time = time;
                return Ok(None);
            }
        };
        self.last_time = time;
        Ok(Some(TimedEvent { time, event }))
    }
}

impl<R: BufRead> Iterator for CastReader<R> {
    type Item = Result<TimedEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            let step = match self.read_line() {
                Ok(None) => {
                    self.done = true;
                    return None;
                }
                Ok(Some(())) if self.buf.is_empty() => continue,
                Ok(Some(())) => self.parse_event(),
                Err(e) => Err(e),
            };
            match step {
                Ok(Some(event)) => return Some(Ok(event)),
                Ok(None) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(rec_err(e)));
                }
            }
        }
        None
    }
}

impl<R: BufRead> std::iter::FusedIterator for CastReader<R> {}

// ── Playback helpers ────────────────────────────────────────────────────

/// Delay before an event `gap` seconds after the previous one: silences
/// are capped at `idle_cap`, then divided by `speed` (clamped to
/// 0.5–8; a non-finite speed plays at 1).
pub fn playback_delay(gap: f64, speed: f64, idle_cap: Duration) -> Duration {
    let speed = clamp_speed(speed);
    let gap = if gap.is_finite() { gap.max(0.0) } else { 0.0 };
    let capped = gap.min(idle_cap.as_secs_f64());
    Duration::try_from_secs_f64(capped / speed).unwrap_or(Duration::ZERO)
}

fn clamp_speed(speed: f64) -> f64 {
    if speed.is_finite() {
        speed.clamp(MIN_PLAYBACK_SPEED, MAX_PLAYBACK_SPEED)
    } else {
        1.0
    }
}

/// Playback clock: turns event times into the delays to wait between them.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackTiming {
    speed: f64,
    idle_cap: Duration,
    last: f64,
}

impl PlaybackTiming {
    /// Starts at time 0 with [`PLAYBACK_IDLE_CAP`].
    pub fn new(speed: f64) -> Self {
        Self {
            speed: clamp_speed(speed),
            idle_cap: PLAYBACK_IDLE_CAP,
            last: 0.0,
        }
    }

    #[must_use]
    pub fn with_idle_cap(mut self, idle_cap: Duration) -> Self {
        self.idle_cap = idle_cap;
        self
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// Changes the speed (clamped) for the next delays.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = clamp_speed(speed);
    }

    /// How long to wait before showing an event at `time`.
    pub fn delay_until(&mut self, time: f64) -> Duration {
        let delay = playback_delay(time - self.last, self.speed, self.idle_cap);
        if time.is_finite() && time > self.last {
            self.last = time;
        }
        delay
    }
}

#[derive(Debug, Default)]
enum SanitizerState {
    #[default]
    Ground,
    /// After ESC (not emitted yet: it is emitted with the character that
    /// completes it, or dropped).
    Esc,
    /// Inside an OSC. `keep`: `None` until the number is read.
    Osc {
        body: String,
        keep: Option<bool>,
        esc: bool,
    },
    /// Inside DCS / SOS / PM / APC: dropped up to ST.
    StringDrop { esc: bool },
}

/// Neutralises, in **untrusted** playback output, the escape sequences that
/// act outside the terminal grid, keeping state across chunks. It is an
/// **allowlist**:
///
/// - every OSC is dropped (clipboard 52, titles 0/1/2, hyperlinks 8 (the
///   link text stays), notifications 9/777, cwd 7, iTerm 1337…), except the
///   colour *settings* 4, 10, 11, 12, 104, 110, 111, 112 (queries with `?`
///   dropped too, the player has nobody to answer); a kept OSC is re-emitted
///   from what was parsed (C0 inside it, ignored by emulators, removed);
/// - DCS, SOS, PM and APC strings are dropped (tmux passthrough, `DECRQSS`
///   queries, and sixel images too);
/// - C1 controls (U+0080–U+009F) are **never emitted**: in any state a C1
///   aborts the current sequence; U+009D opens an OSC, U+0090, U+0098,
///   U+009E, U+009F a dropped string, U+009C ends a string, any other one
///   is dropped;
/// - an OSC ends on BEL or ST (`ESC \` or U+009C), and is aborted by CAN,
///   SUB, another ESC or a C1;
/// - after ESC, like xterm.js and vte, C0 controls are executed (emitted)
///   while the escape stays pending, DEL is ignored; CAN and SUB cancel it;
///   a non-ASCII character cancels it (the character is kept as text).
///
/// **Invariant** of the output: every ESC is immediately followed by a
/// character in `0x20..=0x7E`, and the only ESC + `]` are the kept colour
/// OSCs (no ESC + `P`, `X`, `^`, `_`); no C1 is ever emitted. So the output
/// cannot leave the parser of xterm.js or vte in an escape, OSC or string
/// state that the sanitiser did not see, whatever the chunking.
///
/// Everything else (text, SGR colours, cursor moves, CSI, modes) passes
/// unchanged. The player must not feed emulator replies back anywhere.
#[derive(Debug, Default)]
pub struct PlaybackSanitizer {
    state: SanitizerState,
}

fn osc_allowed(number: &str) -> bool {
    matches!(
        number,
        "4" | "10" | "11" | "12" | "104" | "110" | "111" | "112"
    )
}

/// U+0080–U+009F.
fn is_c1(c: char) -> bool {
    matches!(c, '\u{80}'..='\u{9f}')
}

fn new_osc() -> SanitizerState {
    SanitizerState::Osc {
        body: String::new(),
        keep: None,
        esc: false,
    }
}

impl PlaybackSanitizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sanitises one chunk.
    pub fn sanitize(&mut self, chunk: &str) -> String {
        let mut out = String::with_capacity(chunk.len());
        for c in chunk.chars() {
            self.step(c, &mut out);
        }
        out
    }

    /// End of stream: an unfinished sequence is dropped.
    pub fn finish(&mut self) {
        self.state = SanitizerState::Ground;
    }

    /// What a C1 does, in any state: the current sequence is aborted and the
    /// C1 is never emitted.
    fn c1(c: char) -> SanitizerState {
        match c {
            '\u{9d}' => new_osc(),
            '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => SanitizerState::StringDrop { esc: false },
            _ => SanitizerState::Ground,
        }
    }

    fn step(&mut self, c: char, out: &mut String) {
        let mut next = Some(c);
        while let Some(c) = next.take() {
            let state = std::mem::take(&mut self.state);
            // U+009C (ST) ends a string normally; any other C1 aborts.
            let ends_string = c == '\u{9c}'
                && matches!(
                    state,
                    SanitizerState::Osc { esc: false, .. }
                        | SanitizerState::StringDrop { esc: false }
                );
            if is_c1(c) && !ends_string {
                self.state = Self::c1(c);
                continue;
            }
            self.state = match state {
                SanitizerState::Ground => {
                    if c == '\x1b' {
                        SanitizerState::Esc
                    } else {
                        out.push(c);
                        SanitizerState::Ground
                    }
                }
                SanitizerState::Esc => Self::esc(c, out, &mut next),
                SanitizerState::Osc { body, keep, esc } => {
                    Self::osc(c, body, keep, esc, out, &mut next)
                }
                SanitizerState::StringDrop { esc: true } => {
                    if c == '\\' {
                        SanitizerState::Ground
                    } else {
                        // Aborted by a new escape sequence.
                        next = Some(c);
                        SanitizerState::Esc
                    }
                }
                SanitizerState::StringDrop { esc: false } => match c {
                    '\x1b' => SanitizerState::StringDrop { esc: true },
                    '\u{9c}' | '\x18' | '\x1a' => SanitizerState::Ground,
                    _ => SanitizerState::StringDrop { esc: false },
                },
            };
        }
    }

    /// The character after a (pending, unemitted) ESC.
    fn esc(c: char, out: &mut String, next: &mut Option<char>) -> SanitizerState {
        match c {
            ']' => new_osc(),
            'P' | 'X' | '^' | '_' => SanitizerState::StringDrop { esc: false },
            // ESC ESC restarts the escape (VT parser), DEL is ignored there.
            '\x1b' | '\x7f' => SanitizerState::Esc,
            // A bare ST (no string to end) does nothing; CAN and SUB cancel.
            '\\' | '\x18' | '\x1a' => SanitizerState::Ground,
            // Other C0: executed by the emulator, the escape stays pending.
            '\0'..='\x1f' => {
                out.push(c);
                SanitizerState::Esc
            }
            ' '..='~' => {
                out.push('\x1b');
                out.push(c);
                SanitizerState::Ground
            }
            // Non-ASCII: xterm.js cancels the escape, vte ignores the bytes
            // and stays in it. The ESC is dropped, the character is text.
            _ => {
                *next = Some(c);
                SanitizerState::Ground
            }
        }
    }

    fn osc(
        c: char,
        mut body: String,
        mut keep: Option<bool>,
        esc: bool,
        out: &mut String,
        next: &mut Option<char>,
    ) -> SanitizerState {
        let terminate = |body: &str, keep: Option<bool>, terminator: &str, out: &mut String| {
            if keep.unwrap_or_else(|| osc_allowed(body)) {
                out.push_str("\x1b]");
                out.push_str(body);
                out.push_str(terminator);
            }
            SanitizerState::Ground
        };
        if esc {
            if c == '\\' {
                return terminate(&body, keep, "\x1b\\", out);
            }
            // ESC + anything else aborts the OSC (dropped) and starts anew.
            *next = Some(c);
            return SanitizerState::Esc;
        }
        match c {
            '\x07' => return terminate(&body, keep, "\x07", out),
            '\u{9c}' => return terminate(&body, keep, "\x1b\\", out),
            '\x1b' => {
                return SanitizerState::Osc {
                    body,
                    keep,
                    esc: true,
                };
            }
            '\x18' | '\x1a' => return SanitizerState::Ground,
            // Other C0 inside an OSC: ignored by xterm.js and vte.
            '\0'..='\x1f' => {
                return SanitizerState::Osc {
                    body,
                    keep,
                    esc: false,
                };
            }
            _ => {}
        }
        match keep {
            None if c.is_ascii_digit() && body.len() < MAX_OSC_DIGITS => body.push(c),
            None if c == ';' => {
                let allowed = osc_allowed(&body);
                keep = Some(allowed);
                if allowed {
                    body.push(';');
                } else {
                    body.clear();
                }
            }
            None => {
                keep = Some(false);
                body.clear();
            }
            Some(true) if c == '?' || body.len() >= MAX_KEPT_OSC => {
                keep = Some(false);
                body.clear();
            }
            Some(true) => body.push(c),
            Some(false) => {}
        }
        SanitizerState::Osc {
            body,
            keep,
            esc: false,
        }
    }
}

/// One-shot [`PlaybackSanitizer`] for a complete text.
pub fn sanitize_playback(text: &str) -> String {
    let mut sanitizer = PlaybackSanitizer::new();
    let out = sanitizer.sanitize(text);
    sanitizer.finish();
    out
}

// ── Listing, opening, deleting (blocking) ───────────────────────────────

/// Time of the last event, read from the end of the file.
fn tail_duration(file: &mut File, size: u64) -> Option<f64> {
    let start = size.saturating_sub(TAIL_PROBE_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(TAIL_PROBE_BYTES).read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    // Started mid-file: the first segment may be the end of a cut line.
    let whole = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let last = whole.split('\n').rev().find(|l| !l.trim().is_empty())?;
    let value: Value = serde_json::from_str(last.trim()).ok()?;
    value
        .as_array()
        .and_then(|items| items.first())
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t >= 0.0)
}

fn entry_for(dir: &Path, name: &str) -> Option<RecordingEntry> {
    let path = dir.join(name);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let size = meta.len();
    let stem = name
        .strip_suffix(&format!(".{CAST_EXTENSION}"))
        .unwrap_or(name)
        .to_owned();
    let mut entry = RecordingEntry {
        name: name.to_owned(),
        alias: stem,
        started: None,
        size,
        duration: None,
        width: None,
        height: None,
        valid: false,
    };
    let Ok(mut file) = open_nofollow(&path) else {
        return Some(entry);
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return None;
    }
    let header = CastReader::new(BufReader::new(&mut file), name).map(|r| r.header().clone());
    if let Ok(header) = header {
        entry.valid = true;
        entry.width = Some(header.width);
        entry.height = Some(header.height);
        entry.started = header
            .timestamp
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok());
        if let Some(title) = header.title.filter(|t| !t.is_empty()) {
            entry.alias = title;
        }
        entry.duration = tail_duration(&mut file, size).or(Some(0.0));
    }
    Some(entry)
}

/// The recordings in `dir`, newest first. Symlinks and other non-files are
/// skipped; an invalid file is listed with `valid: false` (to delete it).
pub(crate) fn list(dir: &Path) -> Result<Vec<RecordingEntry>> {
    if !check_dir(dir)? {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let Ok(item) = item else { continue };
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if validate_name(&name).is_err() {
            continue;
        }
        if let Some(entry) = entry_for(dir, &name) {
            entries.push(entry);
        }
        if entries.len() >= MAX_LISTED {
            break;
        }
    }
    entries.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| b.name.cmp(&a.name)));
    Ok(entries)
}

/// Opens `dir/name` for playback (a symlink, a non-file or a file over the
/// cap is refused).
pub(crate) fn open(dir: &Path, name: &str) -> Result<RecordingReader> {
    validate_name(name).map_err(rec_err)?;
    let not_found = || {
        rec_err(RecordingError::NotFound {
            name: name.to_owned(),
        })
    };
    if !check_dir(dir)? {
        return Err(not_found());
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Err(rec_err(RecordingError::UnsafePath { path })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(e) => return Err(Error::io(&path, e)),
    }
    let file = match open_nofollow(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        // ELOOP: swapped for a symlink after the check.
        Err(_) => return Err(rec_err(RecordingError::UnsafePath { path })),
    };
    let meta = file.metadata().map_err(|e| Error::io(&path, e))?;
    if !meta.is_file() {
        return Err(rec_err(RecordingError::UnsafePath { path }));
    }
    if meta.len() > MAX_RECORDING_BYTES {
        return Err(rec_err(RecordingError::TooLarge {
            name: name.to_owned(),
            max: MAX_RECORDING_BYTES,
        }));
    }
    CastReader::new(BufReader::new(file), name)
}

/// Deletes `dir/name`: only a regular file directly inside `dir`.
pub(crate) fn delete(dir: &Path, name: &str) -> Result<()> {
    validate_name(name).map_err(rec_err)?;
    let not_found = || {
        rec_err(RecordingError::NotFound {
            name: name.to_owned(),
        })
    };
    if !check_dir(dir)? {
        return Err(not_found());
    }
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {}
        Ok(_) => return Err(rec_err(RecordingError::UnsafePath { path })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
        Err(e) => return Err(Error::io(&path, e)),
    }
    std::fs::remove_file(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            not_found()
        } else {
            rec_err(RecordingError::Delete {
                name: name.to_owned(),
                source,
            })
        }
    })
}

#[cfg(test)]
mod tests;
