//! Win32 implementation (see the crate doc for the `unsafe` policy).

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAceEx, CONTAINER_INHERIT_ACE, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
    GetAce, GetAclInformation, GetLengthSid, GetTokenInformation, INHERIT_ONLY_ACE, IsValidSid,
    OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser,
    WELL_KNOWN_SID_TYPE, WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_READ_DATA, FILE_WRITE_DATA,
    WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `ACCESS_ALLOWED_ACE_TYPE` (`winnt.h`): the only allow ACE whose SID sits
/// at a fixed offset. Deny ACEs are ignored by the audit (they only remove
/// access); any other allow-type ACE (object, callback) is counted as
/// foreign access, since its trustee is not checked.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;
const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 10;
const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 12;

/// Rights that let a principal read the content.
const READ_RIGHTS: u32 = FILE_READ_DATA | GENERIC_READ | GENERIC_ALL;
/// Rights that let a principal change, replace or delete the content, or
/// grant itself more (`WRITE_DAC`, `WRITE_OWNER`: counted as read too).
const WRITE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;
const ESCALATION_RIGHTS: u32 = WRITE_DAC | WRITE_OWNER | GENERIC_ALL;

/// A security identifier, copied into owned, `DWORD`-aligned memory (SIDs
/// are `8 + 4 × n` bytes long, so the words hold it exactly). Equality is
/// byte equality, which is what `EqualSid` does.
#[derive(Clone, PartialEq, Eq)]
struct Sid {
    words: Vec<u32>,
}

impl std::fmt::Debug for Sid {
    /// `S-1-5-21-…`, computed from the bytes (no allocation by Windows).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes: Vec<u8> = self.words.iter().flat_map(|w| w.to_ne_bytes()).collect();
        let revision = bytes.first().copied().unwrap_or(0);
        let authority = bytes.get(2..8).map_or(0u64, |a| {
            a.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
        });
        write!(f, "S-{revision}-{authority}")?;
        for sub in bytes.get(8..).unwrap_or_default().as_chunks::<4>().0 {
            write!(f, "-{}", u32::from_le_bytes(*sub))?;
        }
        Ok(())
    }
}

impl Sid {
    fn as_psid(&self) -> PSID {
        // The Win32 functions taking a `PSID` here only read it.
        self.words.as_ptr().cast_mut().cast()
    }

    fn byte_len(&self) -> usize {
        self.words.len().saturating_mul(4)
    }

    /// Copies the SID at `psid`, refusing one longer than `max_len` bytes.
    ///
    /// # Safety
    ///
    /// `psid` must be non-null and point to readable memory holding at least
    /// `max_len` bytes (or a complete, valid SID shorter than that) for the
    /// duration of the call.
    #[allow(unsafe_code)]
    unsafe fn copy_from(psid: PSID, max_len: usize) -> io::Result<Self> {
        if psid.is_null() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "null SID"));
        }
        // SAFETY: `psid` points to readable memory (caller contract);
        // `IsValidSid` only reads the revision, count and authorities.
        if unsafe { IsValidSid(psid) } == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid SID"));
        }
        // SAFETY: the SID was just validated, so its length is readable.
        let len = usize::try_from(unsafe { GetLengthSid(psid) }).unwrap_or(usize::MAX);
        if len > max_len || len % 4 != 0 || len < 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SID out of bounds",
            ));
        }
        let mut words = vec![0u32; len / 4];
        // SAFETY: `len` bytes are readable at `psid` (validated SID, within
        // `max_len`), `words` holds exactly `len` writable bytes, and the
        // two do not overlap (`words` was just allocated).
        unsafe {
            std::ptr::copy_nonoverlapping(psid.cast::<u8>(), words.as_mut_ptr().cast::<u8>(), len);
        }
        Ok(Self { words })
    }
}

/// Who owns the object, among the principals OpenSSH for Windows trusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    CurrentUser,
    /// `NT AUTHORITY\SYSTEM`.
    System,
    /// `BUILTIN\Administrators` (owner of what an elevated process creates).
    Administrators,
    Other,
}

/// What [`audit`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Audit {
    pub owner: Principal,
    /// A principal other than the current user, `SYSTEM` or
    /// `Administrators` can read the content (or has no DACL to stop it).
    pub others_can_read: bool,
    /// … can write, delete or re-permission it.
    pub others_can_write: bool,
    /// The DACL is protected (does not inherit from the parent).
    pub protected: bool,
}

impl Audit {
    /// Owned by a trusted principal.
    pub fn owner_trusted(&self) -> bool {
        self.owner != Principal::Other
    }

    /// Trusted owner, and nobody else can read or write: what OpenSSH for
    /// Windows requires of a private key.
    pub fn is_private(&self) -> bool {
        self.owner_trusted() && !self.others_can_read && !self.others_can_write
    }
}

/// The trusted principals, resolved once per process (they do not change).
#[derive(Debug)]
struct Trusted {
    user: Sid,
    system: Sid,
    administrators: Sid,
}

impl Trusted {
    fn get() -> io::Result<&'static Self> {
        static TRUSTED: OnceLock<Trusted> = OnceLock::new();
        if let Some(trusted) = TRUSTED.get() {
            return Ok(trusted);
        }
        let trusted = Self {
            user: current_user()?,
            system: well_known(WinLocalSystemSid)?,
            administrators: well_known(WinBuiltinAdministratorsSid)?,
        };
        Ok(TRUSTED.get_or_init(|| trusted))
    }

    fn classify(&self, sid: &Sid) -> Principal {
        if *sid == self.user {
            Principal::CurrentUser
        } else if *sid == self.system {
            Principal::System
        } else if *sid == self.administrators {
            Principal::Administrators
        } else {
            Principal::Other
        }
    }
}

/// The user of this process's token.
#[allow(unsafe_code)]
fn current_user() -> io::Result<Sid> {
    let mut raw: HANDLE = null_mut();
    // SAFETY: no precondition; returns a pseudo-handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: `process` is valid for this process; `raw` is a valid out
    // pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `raw` is a new token handle that nothing else owns;
    // `OwnedHandle` closes it exactly once.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0u32;
    // SAFETY: a null buffer of length 0 is the documented size query; it
    // fails with ERROR_INSUFFICIENT_BUFFER and sets `needed`.
    let _ = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            null_mut(),
            0,
            &raw mut needed,
        )
    };
    let size = usize::try_from(needed).unwrap_or(0);
    if size < std::mem::size_of::<TOKEN_USER>() {
        return Err(io::Error::last_os_error());
    }
    // u64 words: aligned for `TOKEN_USER` (pointer-sized fields).
    let mut buf = vec![0u64; size.div_ceil(8)];
    let len = u32::try_from(buf.len().saturating_mul(8)).unwrap_or(u32::MAX);
    // SAFETY: `buf` is writable for `len` bytes and 8-aligned.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buf.as_mut_ptr().cast(),
            len,
            &raw mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `buf` starts with an initialized, aligned
    // `TOKEN_USER` (plain old data).
    let user = unsafe { buf.as_ptr().cast::<TOKEN_USER>().read() };
    // SAFETY: the SID it points to lies inside `buf`, alive and unchanged
    // here; `len` bounds it.
    unsafe { Sid::copy_from(user.User.Sid, usize::try_from(len).unwrap_or(0)) }
}

/// A well-known SID of the local machine (`SYSTEM`, `Administrators`).
#[allow(unsafe_code)]
fn well_known(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Sid> {
    let max = SECURITY_MAX_SID_SIZE;
    let mut words = vec![0u32; usize::try_from(max).unwrap_or(68).div_ceil(4)];
    let mut size = max;
    // SAFETY: `words` holds `max` writable, DWORD-aligned bytes and `size`
    // says so; a null domain SID is allowed for these builtin SID types.
    if unsafe { CreateWellKnownSid(kind, null_mut(), words.as_mut_ptr().cast(), &raw mut size) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    words.truncate(usize::try_from(size).unwrap_or(0).div_ceil(4));
    Ok(Sid { words })
}

/// Replaces the DACL of `object` (a file, or a directory when `directory`)
/// with a protected DACL granting only the current user full control
/// (inheritable to children for a directory). The handle needs `WRITE_DAC`.
#[allow(unsafe_code)]
pub fn restrict_to_owner(object: &impl AsHandle, directory: bool) -> io::Result<()> {
    let user = &Trusted::get()?.user;
    // One ACE: its fixed part, minus the `SidStart` placeholder, plus the SID.
    let entry_size = std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        .saturating_sub(std::mem::size_of::<u32>())
        .saturating_add(user.byte_len());
    let total_size = std::mem::size_of::<ACL>().saturating_add(entry_size);
    let mut acl = vec![0u32; total_size.div_ceil(4)];
    let acl_bytes = u32::try_from(acl.len().saturating_mul(4))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACL too large"))?;
    // SAFETY: `acl` is a DWORD-aligned writable buffer of `acl_bytes` bytes.
    if unsafe {
        windows_sys::Win32::Security::InitializeAcl(
            acl.as_mut_ptr().cast(),
            acl_bytes,
            ACL_REVISION,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let flags = if directory {
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
    } else {
        0
    };
    // SAFETY: `acl` was initialized above with room for exactly one ACE
    // carrying this SID; the SID is valid and only read (the ACE copies it).
    if unsafe {
        AddAccessAllowedAceEx(
            acl.as_mut_ptr().cast(),
            ACL_REVISION,
            flags,
            FILE_ALL_ACCESS,
            user.as_psid(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let handle = object.as_handle();
    // SAFETY: `handle` is a live handle borrowed for the call; the ACL is
    // valid and outlives the call; owner, group and SACL may be null since
    // the flags only name the DACL.
    let status = unsafe {
        SetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl.as_ptr().cast::<ACL>(),
            null(),
        )
    };
    win32(status)
}

/// A security descriptor allocated by the system (`LocalAlloc`).
struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for LocalDescriptor {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the block was allocated by `GetSecurityInfo` with
            // `LocalAlloc`, and is freed only here, once.
            let _ = unsafe { LocalFree(self.0) };
        }
    }
}

/// Reads the owner and DACL of `object` (handle with `READ_CONTROL`).
#[allow(unsafe_code)]
pub fn audit(object: &impl AsHandle) -> io::Result<Audit> {
    let trusted = Trusted::get()?;
    let handle = object.as_handle();
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `handle` is live for the call; every out pointer is valid. On
    // success `descriptor` is a `LocalAlloc` block (freed by the guard
    // below) and `owner` / `dacl` point inside it.
    let status = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    win32(status)?;
    let guard = LocalDescriptor(descriptor);
    // SAFETY: `owner` points to the owner SID inside the live descriptor;
    // `IsValidSid` + the SID length bound the copy (a SID is at most
    // `SECURITY_MAX_SID_SIZE` bytes).
    let owner_sid =
        unsafe { Sid::copy_from(owner, usize::try_from(SECURITY_MAX_SID_SIZE).unwrap_or(68)) }?;
    let mut audit = Audit {
        owner: trusted.classify(&owner_sid),
        others_can_read: false,
        others_can_write: false,
        // SAFETY: `descriptor` was returned by `GetSecurityInfo` and is
        // kept alive by `guard`.
        protected: unsafe { protected(descriptor) },
    };
    if dacl.is_null() {
        // A NULL DACL grants everyone full access.
        audit.others_can_read = true;
        audit.others_can_write = true;
        drop(guard);
        return Ok(audit);
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    let info_len = u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(0);
    // SAFETY: `dacl` is a valid ACL inside the live descriptor; `info` is
    // writable for its own size.
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast::<c_void>(),
            info_len,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    for index in 0..info.AceCount {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: `index < AceCount` of this valid ACL; `ace` is a valid out
        // pointer.
        if unsafe { GetAce(dacl, index, &raw mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `GetAce` returned a pointer to an ACE inside the ACL, which
        // starts with an `ACE_HEADER` (read unaligned: no layout assumption).
        let header = unsafe { ace.cast::<ACE_HEADER>().read_unaligned() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            // Applies to children only, not to this object.
            continue;
        }
        let (mask, foreign) = match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE => {
                // SAFETY: an ACCESS_ALLOWED_ACE (type checked) of `AceSize`
                // bytes: header, mask, then the SID at `SidStart`.
                let fixed = unsafe { ace.cast::<ACCESS_ALLOWED_ACE>().read_unaligned() };
                let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
                let room = usize::from(header.AceSize).saturating_sub(sid_offset);
                // SAFETY: the SID starts `sid_offset` bytes into the ACE and
                // the ACE holds `AceSize` bytes, so `room` bytes are readable.
                let sid_ptr = unsafe { ace.cast::<u8>().add(sid_offset) };
                // SAFETY: see above; `copy_from` refuses a SID longer than
                // `room`.
                let sid = unsafe { Sid::copy_from(sid_ptr.cast(), room) }?;
                (fixed.Mask, trusted.classify(&sid) == Principal::Other)
            }
            ACCESS_DENIED_ACE_TYPE
            | ACCESS_DENIED_OBJECT_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE => continue,
            // Object or callback allow ACE: trustee not checked, assume the worst.
            _ => (GENERIC_ALL, true),
        };
        if foreign {
            audit.others_can_read |= mask & (READ_RIGHTS | ESCALATION_RIGHTS) != 0;
            audit.others_can_write |= mask & WRITE_RIGHTS != 0;
        }
    }
    drop(guard);
    Ok(audit)
}

/// Whether the descriptor's DACL is protected from inheritance.
///
/// # Safety
///
/// `descriptor` must point to a valid security descriptor, alive for the
/// duration of the call.
#[allow(unsafe_code)]
unsafe fn protected(descriptor: PSECURITY_DESCRIPTOR) -> bool {
    use windows_sys::Win32::Security::{GetSecurityDescriptorControl, SE_DACL_PROTECTED};
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is valid and alive (caller contract); both out
    // pointers are valid.
    let ok =
        unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) };
    ok != 0 && control & SE_DACL_PROTECTED != 0
}

fn win32(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Wine keeps no real security descriptors on most filesystems (it
    /// derives a DACL from the Unix mode): the ACL tests only mean something
    /// on Windows itself (CI `windows-latest`).
    fn under_wine() -> bool {
        std::env::var_os("WINELOADER").is_some()
            || std::path::Path::new(r"C:\windows\system32\winecfg.exe").exists()
    }

    fn open_dir(path: &std::path::Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .access_mode(crate::access::READ_CONTROL | crate::access::WRITE_DAC)
            .custom_flags(crate::access::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }

    #[test]
    fn well_known_sids_format() {
        assert_eq!(
            format!("{:?}", well_known(WinLocalSystemSid).unwrap()),
            "S-1-5-18"
        );
        assert_eq!(
            format!("{:?}", well_known(WinBuiltinAdministratorsSid).unwrap()),
            "S-1-5-32-544"
        );
        let user = format!("{:?}", current_user().unwrap());
        assert!(user.starts_with("S-1-5-"), "{user}");
    }

    #[test]
    fn restricted_file_is_private() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(
                crate::access::GENERIC_WRITE
                    | crate::access::READ_CONTROL
                    | crate::access::WRITE_DAC,
            )
            .open(&path)
            .unwrap();
        restrict_to_owner(&file, false).unwrap();
        let audit = audit(&file).unwrap();
        if under_wine() {
            return; // the calls succeed, the result is not Windows'
        }
        assert!(audit.is_private(), "{audit:?}");
        assert!(audit.protected, "{audit:?}");
        assert_eq!(audit.owner, Principal::CurrentUser);
    }

    #[test]
    fn restricted_directory_makes_children_private() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("private");
        std::fs::create_dir(&sub).unwrap();
        restrict_to_owner(&open_dir(&sub), true).unwrap();
        if under_wine() {
            return; // the call succeeds, the result is not Windows'
        }
        let dir_audit = audit(&open_dir(&sub)).unwrap();
        assert!(dir_audit.is_private(), "{dir_audit:?}");
        let child = sub.join("child");
        std::fs::write(&child, b"x").unwrap();
        let opened = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .access_mode(crate::access::READ_CONTROL)
                .open(&child)
                .unwrap()
        };
        let audit = audit(&opened).unwrap();
        assert!(audit.is_private(), "{audit:?}");
        // Inherited, so not protected itself.
        assert!(!audit.protected, "{audit:?}");
    }
}
