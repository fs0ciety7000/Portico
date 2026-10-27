//! The sync folder (S1b, `docs/SYNC.md` §3.2, security review F9, F15).
//!
//! ```text
//! <folder chosen by the user>/          canonicalized once, at set-up
//! └── portico-sync/                     a real directory (no link, no junction)
//!     ├── README.txt
//!     ├── bundle.toml                   (+ provider conflict copies `bundle*.toml`)
//!     └── devices/                      a real directory
//!         ├── <32 hex>.snap             one writer per file
//!         ├── <32 hex> (1).snap         a provider conflict copy (any suffix)
//!         └── .<32 hex>.snap.icloud     an iCloud placeholder (not downloaded)
//! ```
//!
//! [`FolderIo`] is everything the engine does there; [`StdFolder`] is the
//! real implementation, the tests also use an in-memory provider. Rules:
//!
//! - **Safe root** (F9): the chosen folder is canonicalized at set-up and
//!   refused inside `~/.ssh`, the config or the data directory
//!   ([`check_root`], re-checked on every pass). Before every operation,
//!   `portico-sync/` and `devices/` must be real directories: a symbolic
//!   link, a Windows junction or mount point (a "name surrogate" reparse
//!   point; cloud placeholders are not) is refused, and their canonical
//!   path must be the expected child of the root. Files are read without
//!   following a link (`O_NOFOLLOW` on Unix) and only when they are
//!   regular files; writes go through a fresh temp file (`create_new`,
//!   `0600` / owner-only DACL) renamed over the target, never through a
//!   link (`store::write_atomic_capped`).
//! - **Limits** (F15): `devices/` may list at most [`MAX_DEVICE_ENTRIES`]
//!   entries (beyond, the sync pauses, nothing is deleted); at most
//!   [`MAX_CANDIDATES_PER_DEVICE`] files per device id; snapshot and
//!   bundle reads are capped by the caller, and a pass reads at most
//!   [`PASS_BYTE_BUDGET`] bytes.
//! - Only names Portico understands are considered: `<id>.snap`, provider
//!   copies `<id><suffix>.snap`, placeholders `.<id>.snap.icloud`. Temp
//!   files (leading `.`) and anything else are ignored, never deleted.
//! - Nothing here is created outside `portico-sync/`: the chosen folder is
//!   never modified (no `chmod`, no DACL change).
//!
//! Every function is blocking: the engine runs them on the dedicated sync
//! thread (`docs/SYNC.md` §5.4).
//!
//! Residual (documented): between the directory check and the rename, a
//! process that can write to the folder could swap `devices/` for a link;
//! the write then lands where the link points. That process can already
//! delete or replace every file of the folder, and what is written is
//! ciphertext.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::SyncError;
use super::clock::DeviceId;
use crate::store::Paths;

/// The directory Portico creates in the chosen folder.
pub(crate) const SYNC_DIR_NAME: &str = "portico-sync";
const DEVICES_DIR: &str = "devices";
const BUNDLE_FILE: &str = "bundle.toml";
const README_FILE: &str = "README.txt";
const SNAP_EXT: &str = ".snap";
const ICLOUD_EXT: &str = ".icloud";
/// Most entries `devices/` may hold (F15): beyond, the sync pauses.
pub(crate) const MAX_DEVICE_ENTRIES: usize = 256;
/// Most files considered per device id (the primary and provider copies).
pub(crate) const MAX_CANDIDATES_PER_DEVICE: usize = 8;
/// Most `bundle*.toml` provider copies considered.
pub(crate) const MAX_BUNDLE_COPIES: usize = 8;
/// Bytes one pass may read from the folder (F15).
pub(crate) const PASS_BYTE_BUDGET: u64 = 64 * 1024 * 1024;
/// Longest provider suffix between the id and `.snap`.
const MAX_COPY_SUFFIX_BYTES: usize = 160;
/// Largest `README.txt` written.
const README_MAX: u64 = 4096;

const README_TEXT: &str = "Portico encrypted sync data.\n\n\
This folder holds only ciphertext: hosts, snippets and settings of your Portico devices,\n\
sealed with a key derived from your sync passphrase. Do not modify these files.\n\
Private keys, known_hosts and ~/.ssh/config are never synchronized.\n";

/// A file of `portico-sync/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FolderFile {
    /// `bundle.toml`.
    Bundle,
    /// A provider copy of `bundle.toml` (read only).
    BundleCopy(String),
    /// A file of `devices/` (a name [`parse_device_name`] accepts).
    Device(String),
}

impl FolderFile {
    /// The primary file of `device`.
    pub(crate) fn primary(device: &DeviceId) -> Self {
        Self::Device(format!("{device}{SNAP_EXT}"))
    }
}

/// What a `devices/` file name designates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NameKind {
    /// `<id>.snap`.
    Primary,
    /// `<id><suffix>.snap`: a copy made by the provider (Dropbox
    /// "conflicted copy", Syncthing `.sync-conflict-…`, `OneDrive`
    /// `-MACHINE`, Google Drive / iCloud ` (1)`, Nextcloud).
    Copy,
    /// `.<id>.snap.icloud`: not downloaded yet.
    Placeholder,
}

/// Parses a `devices/` entry name.
pub(crate) fn parse_device_name(name: &str) -> Option<(DeviceId, NameKind)> {
    if let Some(rest) = name.strip_prefix('.') {
        let inner = rest.strip_suffix(ICLOUD_EXT)?;
        let id = inner.strip_suffix(SNAP_EXT)?;
        return DeviceId::from_hex(id).map(|id| (id, NameKind::Placeholder));
    }
    let stem = name.strip_suffix(SNAP_EXT)?;
    let id = DeviceId::from_hex(stem.get(..32)?)?;
    let suffix = stem.get(32..)?;
    if suffix.is_empty() {
        return Some((id, NameKind::Primary));
    }
    let plausible = suffix.len() <= MAX_COPY_SUFFIX_BYTES
        && !suffix.contains(['/', '\\'])
        && !suffix.chars().any(char::is_control);
    plausible.then_some((id, NameKind::Copy))
}

/// One candidate file of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) file: FolderFile,
    pub(crate) size: u64,
    pub(crate) primary: bool,
}

/// What `devices/` (and the bundle copies) hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Listing {
    /// Candidates per device, primary first, at most
    /// [`MAX_CANDIDATES_PER_DEVICE`].
    pub(crate) devices: BTreeMap<DeviceId, Vec<Candidate>>,
    /// Devices with only a cloud placeholder.
    pub(crate) placeholders: BTreeSet<DeviceId>,
    /// Provider copies of `bundle.toml`.
    pub(crate) bundle_copies: Vec<FolderFile>,
}

/// Everything the engine does in the sync folder (blocking).
pub(crate) trait FolderIo: Send + Sync + fmt::Debug {
    /// `portico-sync/` (and `devices/` when present) are real directories
    /// of the root: [`SyncError::FolderMissing`] (root or `portico-sync/`
    /// gone), [`SyncError::FolderUnsafe`].
    fn check(&self) -> Result<(), SyncError>;
    /// Creates `portico-sync/`, `devices/` and `README.txt` (create only).
    fn init(&self) -> Result<(), SyncError>;
    fn list(&self) -> Result<Listing, SyncError>;
    /// Reads a file of at most `max` bytes; `None` when it is missing.
    fn read(&self, file: &FolderFile, max: usize) -> Result<Option<Vec<u8>>, SyncError>;
    /// Replaces a file atomically.
    fn write(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError>;
    /// Creates a file that must not exist ([`SyncError::BundleExists`]).
    fn create(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError>;
    /// Removes a device file (this device's own copies only: the caller
    /// checks).
    fn remove(&self, file: &FolderFile) -> Result<(), SyncError>;
}

/// Opens the folder a `sync.toml` names (the tests swap it for an
/// in-memory provider).
pub(crate) trait FolderOpener: Send + Sync + fmt::Debug {
    fn open(&self, root: &Path) -> std::sync::Arc<dyn FolderIo>;
}

/// The real file system.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct StdOpener;

impl FolderOpener for StdOpener {
    fn open(&self, root: &Path) -> std::sync::Arc<dyn FolderIo> {
        std::sync::Arc::new(StdFolder::new(root.to_path_buf()))
    }
}

/// Canonicalizes the folder the user chose and refuses one inside
/// `~/.ssh`, the config or the data directory (F9: no loop, no secrets
/// next to keys). Blocking.
pub(crate) fn check_root(chosen: &Path, paths: &Paths) -> Result<PathBuf, SyncError> {
    let root = match std::fs::canonicalize(chosen) {
        Ok(root) => root,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SyncError::FolderMissing);
        }
        Err(e) => return Err(io_error("open", &e)),
    };
    let meta = std::fs::metadata(&root).map_err(|e| io_error("open", &e))?;
    if !meta.is_dir() {
        return Err(SyncError::FolderUnsafe {
            reason: "it is not a folder",
        });
    }
    for private in [&paths.ssh_dir, &paths.config_dir, &paths.data_dir] {
        let canonical = std::fs::canonicalize(private).unwrap_or_else(|_| private.clone());
        if root.starts_with(&canonical) {
            return Err(SyncError::FolderUnsafe {
                reason: "it is inside ~/.ssh or a Portico folder",
            });
        }
    }
    Ok(root)
}

fn io_error(op: &'static str, e: &std::io::Error) -> SyncError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => SyncError::FolderMissing,
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem
            if matches!(op, "write" | "create" | "remove") =>
        {
            SyncError::FolderUnwritable
        }
        kind => SyncError::FolderIo {
            op,
            kind: kind.to_string(),
        },
    }
}

fn from_store(op: &'static str, e: crate::Error) -> SyncError {
    match e {
        crate::Error::Io { source, .. } => io_error(op, &source),
        crate::Error::NotRegularFile { .. } => SyncError::FolderUnsafe {
            reason: "a sync file is a link or not a regular file",
        },
        crate::Error::FileTooLarge { .. } | crate::Error::WriteTooLarge { .. } => {
            SyncError::TooLarge {
                what: "sync file",
                max: crate::sync::container::MAX_CONTAINER_FILE_BYTES,
            }
        }
        _ => SyncError::FolderIo {
            op,
            kind: "unexpected error".to_owned(),
        },
    }
}

/// The real folder: `root` is canonical (from [`check_root`]).
#[derive(Debug, Clone)]
pub(crate) struct StdFolder {
    root: PathBuf,
}

/// Whether a directory exists and is a real one.
enum DirState {
    Real,
    Missing,
}

impl StdFolder {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn sync_dir(&self) -> PathBuf {
        self.root.join(SYNC_DIR_NAME)
    }

    fn devices_dir(&self) -> PathBuf {
        self.sync_dir().join(DEVICES_DIR)
    }

    /// `path` must be a real directory whose canonical form is `path`
    /// itself (`self.root` is canonical): a link, a junction or a mount
    /// point anywhere is refused.
    fn real_dir(path: &Path) -> Result<DirState, SyncError> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DirState::Missing),
            Err(e) => return Err(io_error("open", &e)),
        };
        if meta.file_type().is_symlink() {
            return Err(SyncError::FolderUnsafe {
                reason: "a link inside the sync folder",
            });
        }
        if !meta.is_dir() {
            return Err(SyncError::FolderUnsafe {
                reason: "a sync directory is not a folder",
            });
        }
        let canonical = std::fs::canonicalize(path).map_err(|e| io_error("open", &e))?;
        if canonical != path {
            return Err(SyncError::FolderUnsafe {
                reason: "a link or junction inside the sync folder",
            });
        }
        Ok(DirState::Real)
    }

    fn check_root_dir(&self) -> Result<(), SyncError> {
        match std::fs::symlink_metadata(&self.root) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(SyncError::FolderUnsafe {
                    reason: "the sync folder was replaced by a link",
                });
            }
            Err(e) => return Err(io_error("open", &e)),
        }
        let canonical = std::fs::canonicalize(&self.root).map_err(|e| io_error("open", &e))?;
        if canonical != self.root {
            return Err(SyncError::FolderUnsafe {
                reason: "the sync folder was replaced by a link",
            });
        }
        Ok(())
    }

    /// The path of `file`, after checking its directories.
    fn path_of(&self, file: &FolderFile) -> Result<PathBuf, SyncError> {
        self.check()?;
        Ok(match file {
            FolderFile::Bundle => self.sync_dir().join(BUNDLE_FILE),
            FolderFile::BundleCopy(name) => {
                if !is_bundle_copy(name) {
                    return Err(SyncError::FolderUnsafe {
                        reason: "not a sync file name",
                    });
                }
                self.sync_dir().join(name)
            }
            FolderFile::Device(name) => {
                if parse_device_name(name).is_none_or(|(_, kind)| kind == NameKind::Placeholder) {
                    return Err(SyncError::FolderUnsafe {
                        reason: "not a sync file name",
                    });
                }
                self.devices_dir().join(name)
            }
        })
    }

    fn ensure_devices_dir(&self) -> Result<(), SyncError> {
        let devices = self.devices_dir();
        if matches!(Self::real_dir(&devices)?, DirState::Missing) {
            crate::store::create_private_dir_all(&devices).map_err(|e| from_store("create", e))?;
            Self::real_dir(&devices)?;
        }
        Ok(())
    }
}

fn is_bundle_copy(name: &str) -> bool {
    name != BUNDLE_FILE
        && name.starts_with("bundle")
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        && name.len() <= MAX_COPY_SUFFIX_BYTES
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

/// Reads a regular file without following a link (`O_NOFOLLOW` and
/// `O_NONBLOCK` on Unix), at most `max` bytes.
fn read_no_follow(path: &Path, max: usize) -> Result<Option<Vec<u8>>, SyncError> {
    use std::io::Read;

    let max_u64 = u64::try_from(max).unwrap_or(u64::MAX);
    let too_large = || SyncError::TooLarge {
        what: "sync file",
        max,
    };
    let unsafe_file = || SyncError::FolderUnsafe {
        reason: "a sync file is a link or not a regular file",
    };
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => return Err(unsafe_file()),
        Ok(meta) if meta.len() > max_u64 => return Err(too_large()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error("read", &e)),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        opts.custom_flags(i32::try_from(flags.bits()).unwrap_or(0));
    }
    let file = match opts.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error("read", &e)),
    };
    let meta = file.metadata().map_err(|e| io_error("read", &e))?;
    if !meta.is_file() {
        return Err(unsafe_file());
    }
    if meta.len() > max_u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(max_u64.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read", &e))?;
    if bytes.len() > max {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

impl FolderIo for StdFolder {
    fn check(&self) -> Result<(), SyncError> {
        self.check_root_dir()?;
        if matches!(Self::real_dir(&self.sync_dir())?, DirState::Missing) {
            return Err(SyncError::FolderNotPortico);
        }
        Self::real_dir(&self.devices_dir())?;
        Ok(())
    }

    fn init(&self) -> Result<(), SyncError> {
        self.check_root_dir()?;
        let sync_dir = self.sync_dir();
        if matches!(Self::real_dir(&sync_dir)?, DirState::Missing) {
            crate::store::create_private_dir_all(&sync_dir).map_err(|e| from_store("create", e))?;
            Self::real_dir(&sync_dir)?;
        }
        self.ensure_devices_dir()?;
        let readme = sync_dir.join(README_FILE);
        if matches!(std::fs::symlink_metadata(&readme), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        {
            crate::store::write_atomic_capped(&readme, README_TEXT.as_bytes(), README_MAX)
                .map_err(|e| from_store("create", e))?;
        }
        Ok(())
    }

    fn list(&self) -> Result<Listing, SyncError> {
        self.check()?;
        let mut listing = Listing::default();
        if let Ok(entries) = std::fs::read_dir(self.sync_dir()) {
            for entry in entries.flatten().take(MAX_DEVICE_ENTRIES) {
                if let Some(name) = entry.file_name().to_str()
                    && is_bundle_copy(name)
                    && listing.bundle_copies.len() < MAX_BUNDLE_COPIES
                {
                    listing
                        .bundle_copies
                        .push(FolderFile::BundleCopy(name.to_owned()));
                }
            }
        }
        listing.bundle_copies.sort();
        let entries = match std::fs::read_dir(self.devices_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
            Err(e) => return Err(io_error("list", &e)),
        };
        let mut count = 0usize;
        for entry in entries {
            let entry = entry.map_err(|e| io_error("list", &e))?;
            count = count.saturating_add(1);
            if count > MAX_DEVICE_ENTRIES {
                return Err(SyncError::FolderTooManyEntries {
                    max: MAX_DEVICE_ENTRIES,
                });
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some((device, kind)) = parse_device_name(&name) else {
                continue;
            };
            // `DirEntry::metadata` does not follow a link.
            let Ok(meta) = entry.metadata() else { continue };
            match kind {
                NameKind::Placeholder => {
                    listing.placeholders.insert(device);
                }
                NameKind::Primary | NameKind::Copy => {
                    if meta.file_type().is_symlink() || !meta.is_file() {
                        continue;
                    }
                    listing.devices.entry(device).or_default().push(Candidate {
                        file: FolderFile::Device(name),
                        size: meta.len(),
                        primary: kind == NameKind::Primary,
                    });
                }
            }
        }
        for candidates in listing.devices.values_mut() {
            candidates.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.file.cmp(&b.file)));
            candidates.truncate(MAX_CANDIDATES_PER_DEVICE);
        }
        listing
            .placeholders
            .retain(|device| !listing.devices.contains_key(device));
        Ok(listing)
    }

    fn read(&self, file: &FolderFile, max: usize) -> Result<Option<Vec<u8>>, SyncError> {
        let path = self.path_of(file)?;
        read_no_follow(&path, max)
    }

    fn write(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError> {
        let path = self.path_of(file)?;
        if matches!(file, FolderFile::Device(_)) {
            self.ensure_devices_dir()?;
        }
        let max = u64::try_from(crate::sync::container::MAX_CONTAINER_FILE_BYTES).unwrap_or(0);
        crate::store::write_atomic_capped(&path, bytes, max).map_err(|e| from_store("write", e))
    }

    fn create(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError> {
        let path = self.path_of(file)?;
        match crate::store::write_new(&path, bytes) {
            Ok(()) => Ok(()),
            Err(crate::store::WriteNewError::Exists) => Err(SyncError::BundleExists),
            Err(crate::store::WriteNewError::Other(e)) => Err(from_store("create", e)),
        }
    }

    fn remove(&self, file: &FolderFile) -> Result<(), SyncError> {
        let FolderFile::Device(_) = file else {
            return Err(SyncError::FolderUnsafe {
                reason: "only device files are ever removed",
            });
        };
        let path = self.path_of(file)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                std::fs::remove_file(&path).map_err(|e| io_error("remove", &e))
            }
            Ok(_) => Err(SyncError::FolderUnsafe {
                reason: "a sync file is a link or not a regular file",
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("remove", &e)),
        }
    }
}

/// An in-memory provider for the tests (`docs/SYNC.md` §7, the
/// `SimulatedProvider`): every engine opening it sees the same files.
/// Scenarios are driven by editing [`sim::SimFolder::files`] directly
/// (rollback, truncation, conflict copies, deletion, placeholders) and by
/// its switches (missing, read-only).
#[cfg(test)]
pub(crate) mod sim {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct SimState {
        /// `bundle.toml`, `bundle (1).toml`, `devices/<name>`.
        pub(crate) files: BTreeMap<String, Vec<u8>>,
        pub(crate) initialized: bool,
        pub(crate) missing: bool,
        pub(crate) read_only: bool,
        /// Device names shown as iCloud placeholders.
        pub(crate) placeholders: Vec<String>,
        pub(crate) writes: usize,
    }

    #[derive(Debug, Default, Clone)]
    pub(crate) struct SimFolder {
        pub(crate) state: Arc<Mutex<SimState>>,
    }

    impl SimFolder {
        pub(crate) fn with<T>(&self, f: impl FnOnce(&mut SimState) -> T) -> T {
            f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
        }

        fn key(file: &FolderFile) -> String {
            match file {
                FolderFile::Bundle => BUNDLE_FILE.to_owned(),
                FolderFile::BundleCopy(name) => name.clone(),
                FolderFile::Device(name) => format!("devices/{name}"),
            }
        }
    }

    impl FolderOpener for SimFolder {
        fn open(&self, _root: &Path) -> Arc<dyn FolderIo> {
            Arc::new(self.clone())
        }
    }

    impl FolderIo for SimFolder {
        fn check(&self) -> Result<(), SyncError> {
            self.with(|s| {
                if s.missing {
                    Err(SyncError::FolderMissing)
                } else if !s.initialized {
                    Err(SyncError::FolderNotPortico)
                } else {
                    Ok(())
                }
            })
        }

        fn init(&self) -> Result<(), SyncError> {
            self.with(|s| {
                if s.read_only {
                    return Err(SyncError::FolderUnwritable);
                }
                s.initialized = true;
                Ok(())
            })
        }

        fn list(&self) -> Result<Listing, SyncError> {
            self.check()?;
            self.with(|s| {
                let mut listing = Listing::default();
                for (key, bytes) in &s.files {
                    if let Some(name) = key.strip_prefix("devices/") {
                        if let Some((device, kind)) = parse_device_name(name)
                            && kind != NameKind::Placeholder
                        {
                            listing.devices.entry(device).or_default().push(Candidate {
                                file: FolderFile::Device(name.to_owned()),
                                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                                primary: kind == NameKind::Primary,
                            });
                        }
                    } else if is_bundle_copy(key) {
                        listing
                            .bundle_copies
                            .push(FolderFile::BundleCopy(key.clone()));
                    }
                }
                for name in &s.placeholders {
                    if let Some((device, NameKind::Placeholder)) = parse_device_name(name) {
                        listing.placeholders.insert(device);
                    }
                }
                for candidates in listing.devices.values_mut() {
                    candidates.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.file.cmp(&b.file)));
                    candidates.truncate(MAX_CANDIDATES_PER_DEVICE);
                }
                listing
                    .placeholders
                    .retain(|device| !listing.devices.contains_key(device));
                Ok(listing)
            })
        }

        fn read(&self, file: &FolderFile, max: usize) -> Result<Option<Vec<u8>>, SyncError> {
            self.check()?;
            self.with(|s| match s.files.get(&Self::key(file)) {
                Some(bytes) if bytes.len() > max => Err(SyncError::TooLarge {
                    what: "sync file",
                    max,
                }),
                other => Ok(other.cloned()),
            })
        }

        fn write(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError> {
            self.check()?;
            self.with(|s| {
                if s.read_only {
                    return Err(SyncError::FolderUnwritable);
                }
                s.writes = s.writes.saturating_add(1);
                s.files.insert(Self::key(file), bytes.to_vec());
                Ok(())
            })
        }

        fn create(&self, file: &FolderFile, bytes: &[u8]) -> Result<(), SyncError> {
            self.check()?;
            self.with(|s| {
                if s.read_only {
                    return Err(SyncError::FolderUnwritable);
                }
                let key = Self::key(file);
                if s.files.contains_key(&key) {
                    return Err(SyncError::BundleExists);
                }
                s.files.insert(key, bytes.to_vec());
                Ok(())
            })
        }

        fn remove(&self, file: &FolderFile) -> Result<(), SyncError> {
            self.check()?;
            self.with(|s| {
                if s.read_only {
                    return Err(SyncError::FolderUnwritable);
                }
                s.files.remove(&Self::key(file));
                Ok(())
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn device_names_are_parsed_strictly() {
        let id = DeviceId::from_hex(ID).expect("id");
        assert_eq!(
            parse_device_name(&format!("{ID}.snap")),
            Some((id, NameKind::Primary))
        );
        for copy in [
            format!("{ID} (conflicted copy 2026-10-27).snap"),
            format!("{ID}.sync-conflict-20261027-101010-ABCDEFG.snap"),
            format!("{ID}-LAPTOP.snap"),
            format!("{ID} (1).snap"),
        ] {
            assert_eq!(
                parse_device_name(&copy),
                Some((id, NameKind::Copy)),
                "{copy}"
            );
        }
        assert_eq!(
            parse_device_name(&format!(".{ID}.snap.icloud")),
            Some((id, NameKind::Placeholder))
        );
        for ignored in [
            "README.txt".to_owned(),
            format!(".{ID}.snap.tmp-0011223344556677"),
            format!("{ID}.snap.tmp"),
            format!("{}.snap", &ID[..31]),
            format!("{}.snap", ID.to_uppercase()),
            format!("{ID}\n.snap"),
            "bundle.toml".to_owned(),
        ] {
            assert_eq!(parse_device_name(&ignored), None, "{ignored:?}");
        }
    }

    fn rooted() -> (tempfile::TempDir, Paths, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(&dir.path().join("home"));
        let folder = dir.path().join("Dropbox");
        std::fs::create_dir_all(&folder).expect("mkdir");
        std::fs::create_dir_all(&paths.ssh_dir).expect("mkdir");
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        std::fs::create_dir_all(&paths.data_dir).expect("mkdir");
        let root = check_root(&folder, &paths).expect("root");
        (dir, paths, root)
    }

    #[test]
    fn roots_inside_private_folders_are_refused() {
        let (_dir, paths, _) = rooted();
        for private in [&paths.ssh_dir, &paths.config_dir, &paths.data_dir] {
            let inside = private.join("sync");
            std::fs::create_dir_all(&inside).expect("mkdir");
            assert!(matches!(
                check_root(&inside, &paths),
                Err(SyncError::FolderUnsafe { .. })
            ));
            assert!(matches!(
                check_root(private, &paths),
                Err(SyncError::FolderUnsafe { .. })
            ));
        }
        assert_eq!(
            check_root(&paths.home.join("nowhere"), &paths),
            Err(SyncError::FolderMissing)
        );
    }

    #[test]
    fn init_write_list_read_round_trip() {
        let (_dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        assert_eq!(folder.check(), Err(SyncError::FolderNotPortico));
        folder.init().expect("init");
        folder.check().expect("check");
        assert!(root.join("portico-sync/README.txt").is_file());
        let id = DeviceId::from_hex(ID).expect("id");
        let file = FolderFile::primary(&id);
        folder.write(&file, b"snapshot").expect("write");
        folder
            .write(&FolderFile::Device(format!("{ID} (1).snap")), b"copy")
            .expect("write copy");
        std::fs::write(root.join("portico-sync/devices/notes.txt"), b"x").expect("w");
        std::fs::write(root.join("portico-sync/bundle (1).toml"), b"x").expect("w");
        let listing = folder.list().expect("list");
        let candidates = listing.devices.get(&id).expect("device");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.first().is_some_and(|c| c.primary));
        assert_eq!(
            listing.bundle_copies,
            vec![FolderFile::BundleCopy("bundle (1).toml".into())]
        );
        assert_eq!(
            folder.read(&file, 1024).expect("read").as_deref(),
            Some(&b"snapshot"[..])
        );
        assert!(matches!(
            folder.read(&file, 3),
            Err(SyncError::TooLarge { .. })
        ));
        folder
            .create(&FolderFile::Bundle, b"bundle")
            .expect("create");
        assert_eq!(
            folder.create(&FolderFile::Bundle, b"other"),
            Err(SyncError::BundleExists)
        );
        folder.remove(&file).expect("remove");
        assert_eq!(folder.read(&file, 1024).expect("read"), None);
        assert!(
            folder.remove(&FolderFile::Bundle).is_err(),
            "never the bundle"
        );
    }

    #[test]
    fn too_many_entries_pause_without_deleting() {
        let (_dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        folder.init().expect("init");
        let devices = root.join("portico-sync/devices");
        for n in 0..=MAX_DEVICE_ENTRIES {
            std::fs::write(devices.join(format!("junk-{n}")), b"x").expect("w");
        }
        assert_eq!(
            folder.list(),
            Err(SyncError::FolderTooManyEntries {
                max: MAX_DEVICE_ENTRIES
            })
        );
        assert_eq!(
            std::fs::read_dir(&devices).expect("dir").count(),
            MAX_DEVICE_ENTRIES + 1
        );
    }

    #[cfg(unix)]
    #[test]
    fn devices_dir_symlink_is_refused_and_nothing_written_outside() {
        let (dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        folder.init().expect("init");
        let devices = root.join("portico-sync/devices");
        std::fs::remove_dir(&devices).expect("rmdir");
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir");
        std::os::unix::fs::symlink(&elsewhere, &devices).expect("symlink");
        let id = DeviceId::from_hex(ID).expect("id");
        assert!(matches!(
            folder.check(),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(matches!(
            folder.write(&FolderFile::primary(&id), b"x"),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(matches!(folder.list(), Err(SyncError::FolderUnsafe { .. })));
        assert_eq!(
            std::fs::read_dir(&elsewhere).expect("dir").count(),
            0,
            "nothing written through the link"
        );
        // The same for `portico-sync/` itself.
        std::fs::remove_file(&devices).expect("rm link");
        let sync_dir = root.join("portico-sync");
        std::fs::rename(&sync_dir, dir.path().join("moved")).expect("mv");
        std::os::unix::fs::symlink(dir.path().join("moved"), &sync_dir).expect("symlink");
        assert!(matches!(
            folder.check(),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(matches!(
            folder.write(&FolderFile::Bundle, b"x"),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(!dir.path().join("moved/bundle.toml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn linked_device_files_are_neither_read_nor_replaced() {
        let (dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        folder.init().expect("init");
        let target = dir.path().join("secret.txt");
        std::fs::write(&target, b"outside").expect("w");
        let id = DeviceId::from_hex(ID).expect("id");
        let link = root.join(format!("portico-sync/devices/{ID}.snap"));
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(matches!(
            folder.read(&FolderFile::primary(&id), 1024),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(matches!(
            folder.write(&FolderFile::primary(&id), b"new"),
            Err(SyncError::FolderUnsafe { .. })
        ));
        assert!(
            folder.list().expect("list").devices.is_empty(),
            "links are not candidates"
        );
        assert_eq!(std::fs::read(&target).expect("r"), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn a_root_replaced_by_a_link_is_refused() {
        let (dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        folder.init().expect("init");
        std::fs::rename(&root, dir.path().join("real")).expect("mv");
        std::os::unix::fs::symlink(dir.path().join("real"), &root).expect("symlink");
        assert!(matches!(
            folder.check(),
            Err(SyncError::FolderUnsafe { .. })
        ));
    }

    #[test]
    fn a_missing_folder_is_reported_not_recreated() {
        let (_dir, _paths, root) = rooted();
        let folder = StdFolder::new(root.clone());
        folder.init().expect("init");
        std::fs::remove_dir_all(&root).expect("rm");
        assert_eq!(folder.check(), Err(SyncError::FolderMissing));
        assert_eq!(folder.list(), Err(SyncError::FolderMissing));
        assert!(!root.exists(), "never recreated");
    }
}
