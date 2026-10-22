//! Filesystem locations and atomic persistence of Portico's own data.
//!
//! Portico data is plain, human-readable TOML (git-friendly, zero lock-in):
//!
//! | file              | content                                   |
//! |-------------------|-------------------------------------------|
//! | `hosts.toml`      | tags, groups, favorites, notes per alias  |
//! | `snippets.toml`   | command snippets                          |
//! | `settings.toml`   | theme, keymap overrides, preferences      |
//! | `state/recent.toml` | recent sessions (data dir)              |
//! | `vault.toml`      | encrypted secrets fallback (data dir)     |
//!
//! Secrets are **never** written here in clear text (see [`crate::secrets`]
//! and [`crate::vault`]).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Resolved directories used by the engine.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.ssh`
    pub ssh_dir: PathBuf,
    /// User home (for `~` and `%d` expansion).
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME/portico` or `~/Library/Application Support/dev.Portico.portico`.
    pub config_dir: PathBuf,
    /// Recent sessions, logs, vault.
    pub data_dir: PathBuf,
    /// System-wide `known_hosts` files (read-only).
    pub global_known_hosts: Vec<PathBuf>,
}

impl Paths {
    /// Standard platform locations.
    pub fn discover() -> Result<Self> {
        let base = directories::BaseDirs::new().ok_or(Error::NoHomeDir)?;
        let project =
            directories::ProjectDirs::from("dev", "Portico", "portico").ok_or(Error::NoHomeDir)?;
        Ok(Self {
            ssh_dir: base.home_dir().join(".ssh"),
            home: base.home_dir().to_path_buf(),
            config_dir: project.config_dir().to_path_buf(),
            data_dir: project.data_dir().to_path_buf(),
            global_known_hosts: vec![
                PathBuf::from("/etc/ssh/ssh_known_hosts"),
                PathBuf::from("/etc/ssh/ssh_known_hosts2"),
            ],
        })
    }

    /// Everything under one root: for tests and `--portable` mode.
    pub fn rooted(root: &Path) -> Self {
        Self {
            ssh_dir: root.join(".ssh"),
            home: root.to_path_buf(),
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            global_known_hosts: vec![root.join("etc/ssh/ssh_known_hosts")],
        }
    }

    pub fn ssh_config(&self) -> PathBuf {
        self.ssh_dir.join("config")
    }

    pub fn known_hosts(&self) -> PathBuf {
        self.ssh_dir.join("known_hosts")
    }

    /// `snippets.toml` (config dir).
    pub fn snippets(&self) -> PathBuf {
        self.config_dir.join("snippets.toml")
    }

    /// Encrypted fallback vault (data dir), see [`crate::vault`].
    pub fn vault(&self) -> PathBuf {
        self.data_dir.join("vault.toml")
    }

    /// `hosts.toml` (config dir): tags, group, favorite, notes per alias.
    pub fn hosts_meta(&self) -> PathBuf {
        self.config_dir.join("hosts.toml")
    }

    /// `settings.toml` (config dir), see [`crate::settings`].
    pub fn settings(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// `state/recent.toml` (data dir), see [`crate::recent`].
    pub fn recent(&self) -> PathBuf {
        self.data_dir.join("state").join("recent.toml")
    }

    /// OpenSSH default identities, in OpenSSH's order.
    pub fn default_identities(&self) -> Vec<PathBuf> {
        ["id_rsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .map(|n| self.ssh_dir.join(n))
            .collect()
    }
}

/// Reads a TOML file; a missing file yields `T::default()`.
pub fn load_toml<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| Error::Serde {
            what: "toml",
            message: format!("{}: {e}", path.display()),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// Reads a UTF-8 file of at most `max` bytes; `None` when it is missing.
/// The size is checked before reading and again on what was read (the
/// file may grow in between).
///
/// Only a regular file is read: a symlink to a regular file is followed,
/// but a FIFO, socket, device or directory is refused with
/// [`Error::NotRegularFile`] *before* it is opened (opening a FIFO for
/// reading blocks until a writer shows up). On Unix the open is also
/// non-blocking, which closes the window where the path is swapped for a
/// FIFO between the check and the open; the opened file is checked again.
pub(crate) fn read_capped(path: &Path, max: u64) -> Result<Option<String>> {
    match read_capped_bytes(path, max)? {
        None => Ok(None),
        Some(bytes) => String::from_utf8(bytes).map(Some).map_err(|e| {
            Error::io(
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e.utf8_error()),
            )
        }),
    }
}

/// [`read_capped`] without the UTF-8 decoding: the raw bytes (callers that
/// may hold secrets wrap them in `Zeroizing`).
pub(crate) fn read_capped_bytes(path: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    use std::io::Read;

    let too_large = || Error::FileTooLarge {
        path: path.to_path_buf(),
        max,
    };
    let not_regular = || Error::NotRegularFile {
        path: path.to_path_buf(),
    };
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => return Err(not_regular()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NONBLOCK` has no effect on a regular file.
        let nonblock = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or(0);
        opts.custom_flags(nonblock);
    }
    let file = match opts.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };
    let meta = file.metadata().map_err(|e| Error::io(path, e))?;
    if !meta.is_file() {
        return Err(not_regular());
    }
    if meta.len() > max {
        return Err(too_large());
    }
    // Sized up front: no reallocation leaves stray copies of the content.
    let capacity = usize::try_from(meta.len()).unwrap_or(0).saturating_add(1);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(too_large());
    }
    Ok(Some(bytes))
}

/// Writes a file that [`read_capped`] with the same `max` reads back:
/// refuses, before touching the disk, content larger than `max`
/// ([`Error::WriteTooLarge`]) and a destination that is not a regular file
/// ([`Error::NotRegularFile`]: a symlink would be replaced by the rename,
/// silently breaking the user's link). Then [`write_atomic`].
pub(crate) fn write_atomic_capped(path: &Path, bytes: &[u8], max: u64) -> Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(Error::WriteTooLarge {
            path: path.to_path_buf(),
            max,
        });
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(Error::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(path, e)),
    }
    write_atomic(path, bytes)
}

/// Parses TOML text read from `path` (errors name the file).
pub(crate) fn parse_toml<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    toml::from_str(text).map_err(|e| Error::Serde {
        what: "toml",
        message: format!("{}: {e}", path.display()),
    })
}

/// [`read_capped`] + [`parse_toml`]; a missing file yields `T::default()`.
pub(crate) fn load_toml_capped<T: DeserializeOwned + Default>(path: &Path, max: u64) -> Result<T> {
    match read_capped(path, max)? {
        Some(text) => parse_toml(path, &text),
        None => Ok(T::default()),
    }
}

/// Parses TOML text as an editable document (comments and layout kept).
pub(crate) fn parse_document(path: &Path, text: &str) -> Result<toml_edit::DocumentMut> {
    text.parse()
        .map_err(|e: toml_edit::TomlError| Error::Serde {
            what: "toml",
            message: format!("{}: {}", path.display(), e.message()),
        })
}

/// The table under `key` in `parent`, created (implicit, so no empty
/// header is written) when missing; an inline table is converted. Any
/// other value there is an error: it is never silently replaced.
pub(crate) fn child_table<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
    path: &Path,
) -> Result<&'a mut toml_edit::Table> {
    let item = parent.entry(key).or_insert_with(|| {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        toml_edit::Item::Table(table)
    });
    if !item.is_table() {
        match std::mem::take(item).into_table() {
            Ok(table) => *item = toml_edit::Item::Table(table),
            Err(other) => *item = other,
        }
    }
    item.as_table_mut().ok_or_else(|| Error::Serde {
        what: "toml",
        message: format!("{}: `{key}` is not a table", path.display()),
    })
}

/// The lock file guarding read-modify-write cycles of `path`
/// (`<name>.lock`, next to it, left in place).
pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Writes a TOML file atomically (temp file in the same dir + rename), with
/// `0600` permissions on Unix.
pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).map_err(|e| Error::Serde {
        what: "toml",
        message: e.to_string(),
    })?;
    write_atomic(path, text.as_bytes())
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = write_temp(path, bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(path, e)
    })
}

/// Like [`write_atomic`] but never replaces an existing file: the complete
/// temp file is hard-linked to `path`, which fails with
/// [`std::io::ErrorKind::AlreadyExists`] (no check-then-write race).
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> std::result::Result<(), WriteNewError> {
    let tmp = write_temp(path, bytes).map_err(WriteNewError::Other)?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(WriteNewError::Exists),
        Err(e) => Err(WriteNewError::Other(Error::io(path, e))),
    }
}

/// Failure of [`write_new`].
#[derive(Debug)]
pub(crate) enum WriteNewError {
    /// The destination already exists (it was left untouched).
    Exists,
    Other(Error),
}

/// Attempts at finding a free temp file name before giving up.
const TEMP_NAME_ATTEMPTS: u32 = 8;

/// Creates `dir` and its missing parents, private (`0700` on Unix: only
/// directories created here get that mode, existing ones are left alone).
pub(crate) fn create_private_dir_all(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| Error::io(dir, e))
}

/// Opens (creating it `0600` if needed) the lock file `path` and takes an
/// exclusive advisory lock on it (`flock`), blocking until it is free. The
/// lock is released when the returned file is dropped. Each open is its
/// own lock holder, so two handles in one process exclude each other too.
pub(crate) fn lock_exclusive(path: &Path) -> Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path).map_err(|e| Error::io(path, e))?;
    file.lock().map_err(|e| Error::io(path, e))?;
    Ok(file)
}

/// Random suffix of temp file names (unpredictable: no pre-planted name).
fn random_suffix() -> std::io::Result<u64> {
    use rand::TryRng;
    rand::rngs::SysRng
        .try_next_u64()
        .map_err(|e| std::io::Error::other(format!("no system randomness: {e}")))
}

/// Writes `bytes` to a new `0600` temp file next to `path`, synced to disk.
///
/// The name is random and the file is created with `create_new` (`O_EXCL`):
/// a symlink or file planted under that name is never followed nor reused.
/// Missing parent directories are created `0700`.
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_private_dir_all(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut attempt = 0u32;
    let (tmp, mut file) = loop {
        let suffix = random_suffix().map_err(|e| Error::io(dir, e))?;
        let tmp = dir.join(format!(".{file_name}.tmp-{suffix:016x}"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    && attempt.saturating_add(1) < TEMP_NAME_ATTEMPTS =>
            {
                attempt = attempt.saturating_add(1);
            }
            Err(e) => return Err(Error::io(&tmp, e)),
        }
    };
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| Error::io(&tmp, e));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Doc {
        name: String,
    }

    #[test]
    fn roundtrip_and_missing_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/doc.toml");
        assert_eq!(load_toml::<Doc>(&path).expect("load"), Doc::default());
        let doc = Doc {
            name: "portico".into(),
        };
        save_toml(&path, &doc).expect("save");
        assert_eq!(load_toml::<Doc>(&path).expect("load"), doc);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn write_new_refuses_to_replace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("new.txt");
        write_new(&path, b"first").expect("create");
        assert!(matches!(
            write_new(&path, b"second"),
            Err(WriteNewError::Exists)
        ));
        assert_eq!(std::fs::read(&path).expect("read"), b"first");
        // No temp file left behind.
        assert_eq!(std::fs::read_dir(dir.path()).expect("ls").count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn write_temp_does_not_follow_a_planted_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let mode = |p: &Path| {
            std::fs::symlink_metadata(p)
                .expect("meta")
                .permissions()
                .mode()
                & 0o777
        };

        // A symlink planted at the final name is replaced, its target kept.
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"original").expect("write");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let path = dir.path().join("doc.toml");
        std::os::unix::fs::symlink(&victim, &path).expect("symlink");
        write_atomic(&path, b"new").expect("write");
        assert_eq!(std::fs::read(&victim).expect("read"), b"original");
        assert_eq!(mode(&victim), 0o644);
        let meta = std::fs::symlink_metadata(&path).expect("meta");
        assert!(meta.file_type().is_file(), "{meta:?}");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
        assert_eq!(mode(&path), 0o600);
        // No temp file left behind.
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("ls")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");

        // Missing parents are created private.
        let nested = dir.path().join("a/b/doc.toml");
        write_atomic(&nested, b"x").expect("write");
        assert_eq!(mode(&dir.path().join("a")), 0o700);
        assert_eq!(mode(&dir.path().join("a/b")), 0o700);
        assert_eq!(mode(&nested), 0o600);
    }

    #[test]
    fn exclusive_lock_is_released_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sub/x.lock");
        let first = lock_exclusive(&path).expect("lock");
        let probe = std::fs::File::open(&path).expect("open");
        assert!(matches!(
            probe.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(first);
        probe.try_lock().expect("free after drop");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
