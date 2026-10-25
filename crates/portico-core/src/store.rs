//! Filesystem locations and atomic persistence of Portico's own data.
//!
//! Portico data is plain, human-readable TOML (git-friendly, zero lock-in):
//!
//! | file              | content                                   |
//! |-------------------|-------------------------------------------|
//! | `hosts.toml`      | tags, groups, favorites, notes per alias  |
//! | `hosts.conf`      | `ssh_config` fragment of the hosts created in Portico |
//! | `snippets.toml`   | command snippets                          |
//! | `settings.toml`   | theme, keymap overrides, preferences      |
//! | `state/recent.toml` | recent sessions (data dir)              |
//! | `state/update.toml` | last opt-in update check (data dir)     |
//! | `vault.toml`      | encrypted secrets fallback (data dir)     |
//! | `recordings/*.cast` | opt-in terminal recordings (data dir)   |
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
    /// `~/.ssh` (`%USERPROFILE%\.ssh` on Windows, like OpenSSH for Windows)
    pub ssh_dir: PathBuf,
    /// User home (for `~` and `%d` expansion; `%USERPROFILE%` on Windows).
    pub home: PathBuf,
    /// `$XDG_CONFIG_HOME/portico`, `~/Library/Application Support/dev.Portico.portico`
    /// or `%APPDATA%\Portico\portico\config`.
    pub config_dir: PathBuf,
    /// Recent sessions, logs, vault (`%APPDATA%\Portico\portico\data` on
    /// Windows: roaming, like the config).
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
            global_known_hosts: global_known_hosts(),
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

    /// `hosts.conf` (config dir): the `ssh_config` fragment of the hosts
    /// created in Portico, see [`crate::managed_hosts`].
    pub fn managed_hosts(&self) -> PathBuf {
        self.config_dir.join("hosts.conf")
    }

    /// `settings.toml` (config dir), see [`crate::settings`].
    pub fn settings(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    /// `state/recent.toml` (data dir), see [`crate::recent`].
    pub fn recent(&self) -> PathBuf {
        self.data_dir.join("state").join("recent.toml")
    }

    /// `state/update.toml` (data dir): the last update check, see
    /// [`crate::update`].
    pub fn update_cache(&self) -> PathBuf {
        self.data_dir.join("state").join("update.toml")
    }

    /// `recordings/` (data dir): asciicast files, see [`crate::recording`].
    pub fn recordings(&self) -> PathBuf {
        self.data_dir.join("recordings")
    }

    /// OpenSSH default identities, in OpenSSH's order.
    pub fn default_identities(&self) -> Vec<PathBuf> {
        ["id_rsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .map(|n| self.ssh_dir.join(n))
            .collect()
    }
}

/// The system-wide `known_hosts` files OpenSSH reads: `/etc/ssh/…` on
/// Unix, `%PROGRAMDATA%\ssh\…` with OpenSSH for Windows. On Windows the
/// folder comes from the known folder `FOLDERID_ProgramData`, never from
/// the `PROGRAMDATA` variable (set by whoever starts the process), and the
/// files are only used when the machine owns them
/// (`known_hosts::trusted_global_files`).
fn global_known_hosts() -> Vec<PathBuf> {
    #[cfg(windows)]
    let dir = portico_winsec::program_data_dir()
        .unwrap_or_else(|_| PathBuf::from(r"C:\ProgramData"))
        .join("ssh");
    #[cfg(not(windows))]
    let dir = PathBuf::from("/etc/ssh");
    vec![dir.join("ssh_known_hosts"), dir.join("ssh_known_hosts2")]
}

/// A relative path with `/` separators on every platform (what Portico
/// writes in an `ssh_config` after `~/`: OpenSSH for Windows reads `/`, and
/// a `\` would be an escape for OpenSSH's argument splitting).
pub(crate) fn slash_relative(rest: &Path) -> String {
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// Whether `name` (one path component) is unusable or dangerous as a
/// Windows file name, whatever the platform it is checked on:
///
/// - `<>:"|?*` or a control character: `:` alone is a security issue,
///   `C:x` makes `dir.join` *replace* the folder (drive-relative path) and
///   `a:b` writes an NTFS alternate data stream of `a`;
/// - a trailing dot or space (silently stripped by Win32: `a.` is `a`);
/// - a reserved device name, with or without extension, any case, trailing
///   spaces before the dot ignored: `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
///   `LPT1`–`LPT9`, the superscript `COM¹²³`/`LPT¹²³`, `CONIN$`, `CONOUT$`.
pub(crate) fn windows_name_problem(name: &str) -> bool {
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return true;
    }
    if name.ends_with(['.', ' ']) {
        return true;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|n| {
            let mut chars = n.chars();
            matches!(
                (chars.next(), chars.next()),
                (Some('1'..='9' | '¹' | '²' | '³'), None)
            )
        })
    };
    numbered("COM") || numbered("LPT")
}

/// `true` when `a` and `b` name the same file: equal paths, or equal once
/// canonicalized (both must exist for that). Blocking.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
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
/// `0600` permissions on Unix and an owner-only DACL on Windows (the
/// rename, `MoveFileExW(REPLACE_EXISTING)`, keeps the temp file's DACL).
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

/// Windows: the steps behind Portico's private files, for the apps' own
/// private files (the TUI log): open options able to rewrite the DACL, and
/// the owner-only DACL itself (see `winfs`, ADR "Phase 8 — ACL Windows").
#[cfg(windows)]
pub mod windows {
    use std::fs::{File, OpenOptions};
    use std::path::Path;

    /// Adds `READ_CONTROL | WRITE_DAC` (with read and write) to `opts`.
    pub fn with_dacl_rights(opts: &mut OpenOptions) {
        crate::winfs::with_dacl_rights(opts);
    }

    /// Owner-only protected DACL on `file` (opened with
    /// [`with_dacl_rights`]); skipped on a volume without ACLs.
    pub fn restrict_file(file: &File, path: &Path) -> std::io::Result<()> {
        crate::winfs::restrict_file(file, path)
    }
}

/// Attempts at finding a free temp file name before giving up.
const TEMP_NAME_ATTEMPTS: u32 = 8;

/// Creates `dir` and its missing parents, private (`0700` on Unix, an
/// owner-only DACL on Windows: only directories created here get it,
/// existing ones are left alone).
pub fn create_private_dir_all(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    create_missing_private_dirs(dir)?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| Error::io(dir, e))
}

/// Windows: creates the missing ancestors of `dir` (and `dir`) one by one,
/// each restricted to the user right after its creation, so exactly the
/// directories created here are made private. One created concurrently by
/// someone else (`AlreadyExists`) is left alone, like on Unix.
#[cfg(windows)]
fn create_missing_private_dirs(dir: &Path) -> Result<()> {
    let missing: Vec<&Path> = dir
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && std::fs::symlink_metadata(p).is_err())
        .collect();
    for path in missing.into_iter().rev() {
        match std::fs::create_dir(path) {
            Ok(()) => crate::winfs::restrict_dir(path).map_err(|e| Error::io(path, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::io(path, e)),
        }
    }
    Ok(())
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
    #[cfg(windows)]
    crate::winfs::with_dacl_rights(&mut opts);
    #[cfg(windows)]
    let created = std::fs::symlink_metadata(path).is_err();
    let file = opts.open(path).map_err(|e| Error::io(path, e))?;
    // Windows: a lock file created here is made private (the `0600` above).
    #[cfg(windows)]
    if created {
        crate::winfs::restrict_file(&file, path).map_err(|e| Error::io(path, e))?;
    }
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
        #[cfg(not(windows))]
        let mut opts = std::fs::OpenOptions::new();
        #[cfg(not(windows))]
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        #[cfg(windows)]
        let opts = crate::winfs::private_new_file();
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
    // Windows: private before the first byte is written (the `0600` above).
    #[cfg(windows)]
    if let Err(e) = crate::winfs::restrict_file(&file, &tmp) {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(&tmp, e));
    }
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
    fn windows_file_names() {
        for bad in [
            "C:x",
            "a:b",
            "file::$DATA",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "tab\tname",
            "trailing.",
            "trailing ",
            "CON",
            "con",
            "Con.txt",
            "nul.tar.gz",
            "NUL .txt",
            "aux",
            "prn.log",
            "COM1",
            "com9.txt",
            "LPT3",
            "lpt1.cfg",
            "COM\u{b9}",
            "lpt\u{b2}.txt",
            "COM\u{b3}",
            "CONIN$",
            "conout$.x",
        ] {
            assert!(windows_name_problem(bad), "{bad:?}");
        }
        for ok in [
            "report.pdf",
            ".bashrc",
            "console",
            "CON-2026.cast",
            "nul_device",
            "COM10",
            "COM0",
            "LPT",
            "com1x",
            "été 2026.txt",
            "a.b.c",
            "x.con",
        ] {
            assert!(!windows_name_problem(ok), "{ok:?}");
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
