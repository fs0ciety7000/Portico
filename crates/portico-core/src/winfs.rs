//! Windows counterpart of the Unix `0600` / `0700` modes (Phase 8).
//!
//! | Unix                                   | Windows (here)                                        |
//! |----------------------------------------|-------------------------------------------------------|
//! | file created `0600`                    | [`private_new_file`] then [`restrict_file`]: protected DACL, current user only, set on the handle **before any byte is written**; owner set to the user (an elevated process would otherwise leave `Administrators`) |
//! | directory created `0700`               | [`restrict_dir`]: same DACL, inheritable (`OI\|CI`)   |
//! | `O_NOFOLLOW`                           | `FILE_FLAG_OPEN_REPARSE_POINT` (a symlink or junction is opened as itself, then refused as "not a regular file") |
//! | owner = euid, no `g/o` write (`0o022`) | [`audit_dir`]: owner is the user, `SYSTEM` or `Administrators`, nobody else can write (the OpenSSH for Windows rule) |
//! | `fchmod` of a loose folder             | [`restrict_dir_handle`] on the handle that was checked |
//!
//! The ACL calls are in `portico-winsec`, the only crate allowed `unsafe`
//! (ADR "Phase 8 — ACL Windows"). A filesystem without ACLs (FAT32, exFAT:
//! USB sticks) answers "not supported": like Unix modes on such a mount, the
//! restriction is then skipped (logged), never fatal.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use portico_winsec::access;

/// `FILE_ATTRIBUTE_REPARSE_POINT`: symlink, junction, mount point…
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// `ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED`: no ACL on this volume.
const NO_ACL_ERRORS: [i32; 2] = [1, 50];

/// Options creating a new file (`CREATE_NEW`: never an existing name, nor a
/// symlink planted there) whose handle may rewrite its DACL and owner (a
/// creator is granted what it asks for).
pub(crate) fn private_new_file() -> OpenOptions {
    let mut opts = OpenOptions::new();
    opts.write(true)
        .create_new(true)
        .access_mode(
            access::GENERIC_WRITE | access::READ_CONTROL | access::WRITE_DAC | access::WRITE_OWNER,
        )
        .custom_flags(access::FILE_FLAG_OPEN_REPARSE_POINT);
    opts
}

/// Adds the rights [`restrict_file`] needs to options that open (or create)
/// a file for reading and writing. `MAXIMUM_ALLOWED` picks up
/// `WRITE_OWNER` when the DACL grants it (to make the user the owner), and
/// does not fail the open when it does not.
pub(crate) fn with_dacl_rights(opts: &mut OpenOptions) {
    opts.access_mode(
        access::GENERIC_READ
            | access::GENERIC_WRITE
            | access::READ_CONTROL
            | access::WRITE_DAC
            | access::MAXIMUM_ALLOWED,
    );
}

fn tolerate_no_acl(result: io::Result<()>, what: &Path) -> io::Result<()> {
    match result {
        Err(e) if e.raw_os_error().is_some_and(|c| NO_ACL_ERRORS.contains(&c)) => {
            tracing::warn!(path = %what.display(), "no ACL support on this volume: left as is");
            Ok(())
        }
        other => other,
    }
}

/// Owner-only, protected DACL on an open file (see the module doc).
pub(crate) fn restrict_file(file: &File, path: &Path) -> io::Result<()> {
    tolerate_no_acl(portico_winsec::restrict_to_owner(file, false), path)
}

/// Opens a directory itself (not what a reparse point leads to), for its
/// security descriptor. `pin`: no `FILE_SHARE_DELETE`, so the directory
/// cannot be renamed or replaced while the handle is open.
pub(crate) fn open_dir(path: &Path, write_dac: bool, pin: bool) -> io::Result<File> {
    let mut rights = access::READ_CONTROL | access::FILE_READ_ATTRIBUTES | access::SYNCHRONIZE;
    if write_dac {
        // `MAXIMUM_ALLOWED`: also `WRITE_OWNER` when granted (see
        // `with_dacl_rights`).
        rights |= access::WRITE_DAC | access::MAXIMUM_ALLOWED;
    }
    let mut share = access::FILE_SHARE_READ | access::FILE_SHARE_WRITE;
    if !pin {
        share |= access::FILE_SHARE_DELETE;
    }
    let dir = OpenOptions::new()
        .access_mode(rights)
        .share_mode(share)
        .custom_flags(access::FILE_FLAG_BACKUP_SEMANTICS | access::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = dir.metadata()?;
    if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "not a real directory (reparse point)",
        ));
    }
    Ok(dir)
}

/// Owner-only, protected, inheritable DACL on an open directory.
pub(crate) fn restrict_dir_handle(dir: &File, path: &Path) -> io::Result<()> {
    tolerate_no_acl(portico_winsec::restrict_to_owner(dir, true), path)
}

/// [`restrict_dir_handle`] on the directory at `path` (just created by us).
pub(crate) fn restrict_dir(path: &Path) -> io::Result<()> {
    let dir = open_dir(path, true, false)?;
    restrict_dir_handle(&dir, path)
}

/// The ACL audit of an open handle; `None` on a volume without ACLs.
pub(crate) fn audit(object: &File) -> io::Result<Option<portico_winsec::Audit>> {
    match portico_winsec::audit(object) {
        Ok(audit) => Ok(Some(audit)),
        Err(e) if e.raw_os_error().is_some_and(|c| NO_ACL_ERRORS.contains(&c)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether an existing directory is safe to create secrets in: a trusted
/// owner, and nobody else can write (`None`: no ACLs, nothing to check).
pub(crate) fn dir_is_safe(path: &Path) -> io::Result<bool> {
    let dir = open_dir(path, false, false)?;
    Ok(audit(&dir)?.is_none_or(|a| a.owner_trusted() && !a.others_can_write))
}

/// Whether the machine-wide file at `path` can be trusted for this user:
/// the file (opened as itself: a reparse point is refused) and its folder
/// are owned by the machine and no other account can change them
/// (`Audit::machine_owned_and_locked`). `None`: a volume without ACLs.
pub(crate) fn machine_file_trusted(path: &Path) -> io::Result<Option<bool>> {
    let file = OpenOptions::new()
        .access_mode(access::READ_CONTROL | access::FILE_READ_ATTRIBUTES)
        .custom_flags(access::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(Some(false));
    }
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(Some(false));
    };
    let dir = match open_dir(parent, false, false) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => return Ok(Some(false)),
        Err(e) => return Err(e),
    };
    let (Some(file_audit), Some(dir_audit)) = (audit(&file)?, audit(&dir)?) else {
        return Ok(None);
    };
    Ok(Some(
        file_audit.machine_owned_and_locked() && dir_audit.machine_owned_and_locked(),
    ))
}

/// Whether other accounts can read the file at `path` (`false` when it
/// cannot be told). Used to warn about a private key, like OpenSSH does.
pub(crate) fn readable_by_others(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .access_mode(access::READ_CONTROL)
        .open(path)
    else {
        return false;
    };
    matches!(audit(&file), Ok(Some(a)) if a.others_can_read || !a.owner_trusted())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Wine derives a DACL from the Unix mode: ACL assertions only hold on
    /// Windows itself (CI `windows-latest`).
    pub(crate) fn under_wine() -> bool {
        std::env::var_os("WINELOADER").is_some()
            || Path::new(r"C:\windows\system32\winecfg.exe").exists()
    }

    #[test]
    fn new_private_file_refuses_an_existing_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        let file = private_new_file().open(&path).expect("create");
        restrict_file(&file, &path).expect("restrict");
        drop(file);
        assert_eq!(
            private_new_file().open(&path).expect_err("exists").kind(),
            io::ErrorKind::AlreadyExists
        );
        if !under_wine() {
            assert!(!readable_by_others(&path));
            // The user's, even from an elevated process (CI).
            let opened = OpenOptions::new()
                .access_mode(access::READ_CONTROL)
                .open(&path)
                .expect("open");
            let audit = audit(&opened).expect("audit").expect("ACLs");
            assert_eq!(audit.owner, portico_winsec::Principal::CurrentUser);
        }
    }

    #[test]
    fn restricted_dir_is_safe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("d");
        std::fs::create_dir(&sub).expect("mkdir");
        restrict_dir(&sub).expect("restrict");
        if !under_wine() {
            assert!(dir_is_safe(&sub).expect("audit"));
            let opened = open_dir(&sub, false, false).expect("open");
            let audit = audit(&opened).expect("audit").expect("ACLs");
            assert_eq!(audit.owner, portico_winsec::Principal::CurrentUser);
        }
        assert!(open_dir(&dir.path().join("missing"), false, false).is_err());
    }

    /// What a user creates (even restricted to them) is not a machine file:
    /// a `known_hosts` planted in `%PROGRAMDATA%` by an account is skipped.
    #[test]
    fn a_user_file_is_not_a_machine_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("ssh");
        std::fs::create_dir(&sub).expect("mkdir");
        restrict_dir(&sub).expect("restrict");
        let path = sub.join("ssh_known_hosts");
        let file = private_new_file().open(&path).expect("create");
        restrict_file(&file, &path).expect("restrict");
        drop(file);
        let trusted = machine_file_trusted(&path).expect("audit");
        if !under_wine() {
            assert_eq!(trusted, Some(false));
        }
        assert_eq!(
            machine_file_trusted(&sub.join("missing"))
                .expect_err("missing")
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    /// A file of the system itself passes (`notepad.exe`: `TrustedInstaller`,
    /// in a folder no user can write).
    #[test]
    fn a_system_file_is_a_machine_file() {
        if under_wine() {
            return;
        }
        let windir = std::env::var_os("SystemRoot").map_or_else(
            || std::path::PathBuf::from(r"C:\Windows"),
            std::path::PathBuf::from,
        );
        let path = windir.join("System32").join("notepad.exe");
        if !path.exists() {
            return;
        }
        assert_eq!(machine_file_trusted(&path).expect("audit"), Some(true));
    }

    #[test]
    fn a_directory_symlink_is_not_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("mkdir");
        let link = dir.path().join("link");
        // Needs Developer Mode or an elevated token (CI): skipped otherwise.
        if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
            return;
        }
        let err = open_dir(&link, false, false).expect_err("reparse point");
        // Wine opens the link differently (NotFound): refused either way.
        if !under_wine() {
            assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
        }
    }
}
