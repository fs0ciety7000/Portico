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

/// Writes `bytes` to a `0600` temp file next to `path`, synced to disk.
fn write_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    use std::io::Write;

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(|e| Error::io(&tmp, e))?;
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
}
