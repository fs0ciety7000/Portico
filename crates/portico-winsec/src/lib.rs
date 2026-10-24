//! # portico-winsec
//!
//! Windows file security for Portico (Phase 8): the Windows counterpart of
//! the Unix `0600` / `0700` modes, and of the "group/other can read or
//! write" checks.
//!
//! - [`restrict_to_owner`]: replaces the DACL of an open file or directory
//!   with a **protected** (not inherited) DACL holding a single ACE that
//!   grants the current user full control. For a directory the ACE is
//!   inheritable (`OI|CI`), so what is created inside is private too. This is
//!   exactly what Microsoft documents to fix OpenSSH for Windows'
//!   "UNPROTECTED PRIVATE KEY FILE" (`icacls <f> /inheritance:r /grant:r
//!   %USERNAME%:F`).
//! - [`audit`]: reads the owner and DACL of an open file and says whether a
//!   principal *other than* the current user, `SYSTEM` or `Administrators`
//!   can read or write it — the rule OpenSSH for Windows applies to private
//!   keys and configuration files (`check_secure_file_permission`).
//!
//! Both take a handle (never a path): the checks and changes apply to the
//! object that was opened, which cannot be swapped meanwhile. The handle
//! must have been opened with `READ_CONTROL` (audit) or `WRITE_DAC`
//! (restrict); [`access`] holds the flags.
//!
//! ## Why `unsafe` lives here
//!
//! `#![forbid(unsafe_code)]` holds everywhere else in the workspace. The
//! security descriptor API has no maintained safe wrapper (ADR "Phase 8 —
//! ACL Windows" in `docs/STACK.md`), so this crate is the one exception:
//! `unsafe_code` is `deny` at crate level, each function that calls Win32
//! carries its own `#[allow(unsafe_code)]`, every `unsafe` block holds one
//! operation (`clippy::multiple_unsafe_ops_per_block`) and states its
//! invariants (`SAFETY:`, `clippy::undocumented_unsafe_blocks`). Owned
//! memory only: SIDs and ACLs are built in Rust `Vec`s, the one
//! system-allocated block (`GetSecurityInfo`) is freed by a guard.
//!
//! On other platforms the crate is empty (and forbids `unsafe`).

#![cfg_attr(not(windows), forbid(unsafe_code))]

#[cfg(windows)]
mod imp;

#[cfg(windows)]
pub use imp::{Audit, Principal, audit, restrict_to_owner};

/// Access, share and flag bits for opening a handle that [`audit`] and
/// [`restrict_to_owner`] accept (`OpenOptionsExt::access_mode`,
/// `share_mode`, `custom_flags`).
#[cfg(windows)]
pub mod access {
    pub use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    pub use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
    };
}
