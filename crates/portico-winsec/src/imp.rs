//! Win32 implementation (see the crate doc for the `unsafe` policy).

use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NOT_SUPPORTED, ERROR_SUCCESS, GENERIC_ALL, GENERIC_READ,
    GENERIC_WRITE, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    GetSecurityInfo, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SE_OBJECT_TYPE, SetSecurityInfo,
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
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::ErrorReporting::{WER_FAULT_REPORTING_FLAG_NOHEAP, WerSetFlags};
use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath};

use crate::{Audit, Principal};

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
/// `FILE_CREATE_PIPE_INSTANCE` (`winnt.h`, the bit of `FILE_APPEND_DATA`
/// on a pipe).
const FILE_CREATE_PIPE_INSTANCE: u32 = 0x0004;
/// Rights that let a principal create its own instance of a named pipe
/// (and so receive a client's connection), directly or by re-permissioning
/// it. `FILE_WRITE_DATA` is not one: the OpenSSH agent's pipe grants it to
/// every authenticated user (`(A;;0x12019b;;;AU)`).
const INSTANCE_RIGHTS: u32 =
    FILE_CREATE_PIPE_INSTANCE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;

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

/// The trusted principals, resolved once per process (they do not change).
#[derive(Debug)]
struct Trusted {
    user: Sid,
    system: Sid,
    administrators: Sid,
    installer: Sid,
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
            installer: trusted_installer(),
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
        } else if *sid == self.installer {
            Principal::TrustedInstaller
        } else {
            Principal::Other
        }
    }
}

/// The user of this process's token.
#[allow(unsafe_code)]
fn current_user() -> io::Result<Sid> {
    // SAFETY: no precondition; returns a pseudo-handle that needs no closing.
    let process = unsafe { GetCurrentProcess() };
    token_user(process)
}

/// The user of the token of `process` (a handle with at least
/// `PROCESS_QUERY_LIMITED_INFORMATION`, valid for the call).
#[allow(unsafe_code)]
fn token_user(process: HANDLE) -> io::Result<Sid> {
    let mut raw: HANDLE = null_mut();
    // SAFETY: `process` is a valid process handle (caller contract); `raw`
    // is a valid out pointer.
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
    // The SID must lie inside `buf` (Windows places it right after the
    // `TOKEN_USER`): its offset from the start, checked, bounds the copy
    // to the bytes that remain.
    let filled = usize::try_from(len).unwrap_or(0);
    let offset = user
        .User
        .Sid
        .addr()
        .checked_sub(buf.as_ptr().addr())
        .filter(|offset| *offset < filled)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "token SID outside its buffer")
        })?;
    // SAFETY: the SID pointer lies inside `buf` (checked above), alive and
    // unchanged here, and `filled - offset` bytes of `buf` follow it, which
    // bounds the copy.
    unsafe { Sid::copy_from(user.User.Sid, filled.saturating_sub(offset)) }
}

/// `NT SERVICE\TrustedInstaller`, `S-1-5-80-956008885-3418522649-1831038044-
/// 1853292631-2271478464`: a service SID (SHA-1 of the service name), the
/// same on every machine, with no `WELL_KNOWN_SID_TYPE`. Built from its
/// bytes: revision 1, 6 sub-authorities, authority 5 (NT), then the
/// sub-authorities as native `DWORD`s, the layout of a SID.
fn trusted_installer() -> Sid {
    Sid {
        words: vec![
            u32::from_ne_bytes([1, 6, 0, 0]),
            u32::from_ne_bytes([0, 0, 0, 5]),
            80,
            956_008_885,
            3_418_522_649,
            1_831_038_044,
            1_853_292_631,
            2_271_478_464,
        ],
    }
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
/// (inheritable to children for a directory), and makes the user its owner
/// when the handle allows it. The handle needs `WRITE_DAC`, and
/// `WRITE_OWNER` for the owner (see [`crate::access`]).
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
    // Owner first choice: the user too, so that what an elevated process
    // creates (owned by `Administrators` by default) is the user's, like
    // what a standard one creates. Needs `WRITE_OWNER` on the handle; one
    // opened without it gets the DACL alone (the owner stays a trusted one:
    // the creator).
    // SAFETY: `handle` is a live handle borrowed for the call; the SID and
    // the ACL are valid and outlive the call; group and SACL may be null
    // since the flags only name the owner and the DACL.
    let status = unsafe {
        SetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            user.as_psid(),
            null_mut(),
            acl.as_ptr().cast::<ACL>(),
            null(),
        )
    };
    if status != ERROR_ACCESS_DENIED {
        return win32(status);
    }
    // SAFETY: as above, with the DACL alone (owner left as is, may be null).
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

/// Reads the owner and DACL of `object`, a file or a directory (handle
/// with `READ_CONTROL`).
pub fn audit(object: &impl AsHandle) -> io::Result<Audit> {
    audit_object(object, SE_FILE_OBJECT)
}

/// [`audit`] of an object of the given kind.
#[allow(unsafe_code)]
fn audit_object(object: &impl AsHandle, kind: SE_OBJECT_TYPE) -> io::Result<Audit> {
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
            kind,
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
    if owner.is_null() {
        // No owner at all: a volume without security (FAT32, exFAT, some
        // network shares). Said as "not supported", like the other ACL
        // calls there, so callers treat it as "no ACL to check".
        drop(guard);
        return Err(io::Error::from_raw_os_error(
            i32::try_from(ERROR_NOT_SUPPORTED).unwrap_or(50),
        ));
    }
    // SAFETY: `owner` points to the owner SID inside the live descriptor;
    // `IsValidSid` + the SID length bound the copy (a SID is at most
    // `SECURITY_MAX_SID_SIZE` bytes).
    let owner_sid =
        unsafe { Sid::copy_from(owner, usize::try_from(SECURITY_MAX_SID_SIZE).unwrap_or(68)) }?;
    let mut audit = Audit {
        owner: trusted.classify(&owner_sid),
        others_can_read: false,
        others_can_write: false,
        others_can_add_instance: false,
        // SAFETY: `descriptor` was returned by `GetSecurityInfo` and is
        // kept alive by `guard`.
        protected: unsafe { protected(descriptor) },
    };
    if dacl.is_null() {
        // A NULL DACL grants everyone full access.
        audit.others_can_read = true;
        audit.others_can_write = true;
        audit.others_can_add_instance = true;
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
            audit.others_can_add_instance |= mask & INSTANCE_RIGHTS != 0;
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

/// [`audit`] of the named pipe `pipe` (a client handle, opened with
/// `GENERIC_READ`, which includes `READ_CONTROL`): its owner, and whether
/// another account may add an instance of it
/// ([`Audit::others_can_add_instance`]).
///
/// Read as a kernel object (`SE_KERNEL_OBJECT`, as tokio documents for
/// pipes): the file-object path of [`audit`] (`SE_FILE_OBJECT`) is meant
/// for files and directories. Combine with [`pipe_server_user`] through
/// [`crate::agent_server_trusted`].
pub fn pipe_audit(pipe: &impl AsHandle) -> io::Result<Audit> {
    audit_object(pipe, SE_KERNEL_OBJECT)
}

/// Who runs the server end of the named pipe `pipe` (a client handle):
/// the user of the server process's token, `None` when that token cannot
/// be opened (another account's process, or a service: a standard user may
/// not open a `SYSTEM` token). Combine with the pipe's owner
/// ([`pipe_owner`]) through [`crate::agent_server_trusted`].
#[allow(unsafe_code)]
pub fn pipe_server_user(pipe: &impl AsHandle) -> io::Result<Option<Principal>> {
    let trusted = Trusted::get()?;
    let mut pid = 0u32;
    // SAFETY: the handle is live for the call (borrowed); `pid` is a valid
    // out pointer. Fails (no pid) on anything but a pipe client handle.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_handle().as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: plain call; the returned handle, when not null, is new and
    // owned by nobody else.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == i32::try_from(ERROR_ACCESS_DENIED).ok() {
            Ok(None)
        } else {
            Err(e)
        };
    }
    // SAFETY: `raw` is a new process handle (checked non-null) that nothing
    // else owns; `OwnedHandle` closes it exactly once.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    match token_user(process.as_raw_handle()) {
        Ok(sid) => Ok(Some(trusted.classify(&sid))),
        Err(e) if e.raw_os_error() == i32::try_from(ERROR_ACCESS_DENIED).ok() => Ok(None),
        Err(e) => Err(e),
    }
}

/// A string allocated by the shell (`CoTaskMemAlloc`).
struct CoTaskString(*mut u16);

impl Drop for CoTaskString {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the block was allocated by `SHGetKnownFolderPath` (or is
        // null, which `CoTaskMemFree` accepts), and is freed only here.
        unsafe { CoTaskMemFree(self.0.cast_const().cast()) };
    }
}

/// Longest path the known-folder API returns (`UNICODE_STRING` limit, in
/// UTF-16 units): bounds the scan for the terminating NUL.
const MAX_KNOWN_FOLDER_LEN: usize = 32_767;

/// `%PROGRAMDATA%` (usually `C:\ProgramData`), from the known folder
/// `FOLDERID_ProgramData`: the environment variable can be set by whoever
/// starts the process, the known folder cannot.
#[allow(unsafe_code)]
pub fn program_data_dir() -> io::Result<PathBuf> {
    let mut raw: *mut u16 = null_mut();
    // SAFETY: the GUID is a static; flags 0 (`KF_FLAG_DEFAULT`); a null
    // token is the current user; `raw` is a valid out pointer. The string
    // it receives is freed by the guard (also on failure, as documented).
    let status =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, null_mut(), &raw mut raw) };
    let guard = CoTaskString(raw);
    if status != 0 || guard.0.is_null() {
        return Err(io::Error::other(format!(
            "no ProgramData known folder (HRESULT {status:#010x})"
        )));
    }
    let mut units = Vec::new();
    for index in 0..MAX_KNOWN_FOLDER_LEN {
        // SAFETY: the string is NUL-terminated and alive (guard); `index`
        // never passes its NUL, since the loop stops there.
        let unit = unsafe { guard.0.wrapping_add(index).read() };
        if unit == 0 {
            return Ok(PathBuf::from(OsString::from_wide(&units)));
        }
        units.push(unit);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "ProgramData path too long",
    ))
}

/// Asks Windows Error Reporting to leave the heap out of this process's
/// crash reports (`WER_FAULT_REPORTING_FLAG_NOHEAP`): a report sent or kept
/// after a crash does not carry the memory that may hold secrets. Per
/// process, no registry write, no rights needed.
#[allow(unsafe_code)]
pub fn disable_wer_heap_dumps() -> io::Result<()> {
    // SAFETY: plain call with a documented flag; no pointer.
    let status = unsafe { WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP) };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "WerSetFlags: HRESULT {status:#010x}"
        )))
    }
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
            .access_mode(
                crate::access::READ_CONTROL
                    | crate::access::WRITE_DAC
                    | crate::access::MAXIMUM_ALLOWED,
            )
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
    fn program_data_is_absolute() {
        let dir = program_data_dir().unwrap();
        assert!(dir.is_absolute(), "{}", dir.display());
    }

    #[test]
    fn wer_flags_are_accepted() {
        if under_wine() {
            return; // wine may not implement WER
        }
        disable_wer_heap_dumps().unwrap();
    }

    #[test]
    fn trusted_installer_sid() {
        assert_eq!(
            format!("{:?}", trusted_installer()),
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        );
    }

    #[test]
    fn a_file_is_not_a_pipe() {
        let file = tempfile::tempfile().unwrap();
        assert!(pipe_server_user(&file).is_err());
    }

    #[tokio::test]
    async fn a_pipe_of_this_user_is_a_trusted_agent_server() {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let name = format!(r"\\.\pipe\portico-winsec-test-{}", std::process::id());
        let _server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        if under_wine() {
            return; // no pipe security descriptor under wine
        }
        let user = pipe_server_user(&client).unwrap();
        assert_eq!(user, Some(Principal::CurrentUser));
        let pipe = pipe_audit(&client);
        // `Administrators` when the test runs elevated (CI).
        assert!(
            pipe.as_ref().is_ok_and(|a| matches!(
                a.owner,
                Principal::CurrentUser | Principal::Administrators
            )),
            "{pipe:?}"
        );
        assert!(crate::agent_server_trusted(user, pipe.ok().as_ref()));
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
                    | crate::access::WRITE_DAC
                    | crate::access::WRITE_OWNER,
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
        // The user's even when this test runs elevated (CI runners), where
        // a new file is owned by `Administrators`.
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
        assert_eq!(dir_audit.owner, Principal::CurrentUser);
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
