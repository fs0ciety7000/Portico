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
//! - [`pipe_server_user`]: who runs the server end of a named pipe (the
//!   user of the server process's token), and [`pipe_audit`] (its owner and
//!   DACL), to verify an `ssh-agent` pipe before a key is sent to it
//!   ([`agent_server_trusted`]).
//! - [`program_data_dir`]: `%PROGRAMDATA%` from the known folder (never
//!   from the environment variable, which any process can set).
//! - [`disable_wer_heap_dumps`]: Windows Error Reporting leaves the heap
//!   out of this process's crash reports.
//!
//! The policies themselves ([`Audit`], [`Principal`],
//! [`agent_server_trusted`]) are plain data, compiled and tested on every
//! platform.
//!
//! The ACL functions take a handle (never a path): the checks and changes apply to the
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
pub use imp::{
    audit, disable_wer_heap_dumps, pipe_audit, pipe_server_user, program_data_dir,
    restrict_to_owner,
};

/// Who a security identifier is, among the principals OpenSSH for Windows
/// trusts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    CurrentUser,
    /// `NT AUTHORITY\SYSTEM`.
    System,
    /// `BUILTIN\Administrators` (owner of what an elevated process creates).
    Administrators,
    /// `NT SERVICE\TrustedInstaller` (Windows servicing: owns what ships
    /// with the system).
    TrustedInstaller,
    Other,
}

impl Principal {
    /// The machine itself (not the current user): `SYSTEM`,
    /// `Administrators` or `TrustedInstaller`. An unprivileged account can
    /// neither be nor create objects owned by one of these.
    pub fn is_privileged(self) -> bool {
        matches!(
            self,
            Self::System | Self::Administrators | Self::TrustedInstaller
        )
    }
}

/// What [`audit`] found.
// Four independent facts read from one security descriptor (not states of
// one thing): an enum or bitflags would only hide them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Audit {
    pub owner: Principal,
    /// A principal other than the current user, `SYSTEM`,
    /// `Administrators` or `TrustedInstaller` can read the content (or
    /// there is no DACL to stop it).
    pub others_can_read: bool,
    /// … can write, delete or re-permission it.
    pub others_can_write: bool,
    /// A named pipe: such a principal may create its own instance of it
    /// (`FILE_CREATE_PIPE_INSTANCE`, or `WRITE_DAC`, `WRITE_OWNER`,
    /// `GENERIC_WRITE`, `GENERIC_ALL`), and so receive the next client.
    /// Not implied by `others_can_write`: the OpenSSH agent's pipe grants
    /// `FILE_WRITE_DATA` to every authenticated user, not this right.
    pub others_can_add_instance: bool,
    /// The DACL is protected (does not inherit from the parent).
    pub protected: bool,
}

impl Audit {
    /// Owned by a trusted principal (the current user or the machine).
    pub fn owner_trusted(&self) -> bool {
        self.owner != Principal::Other
    }

    /// Trusted owner, and nobody else can read or write: what OpenSSH for
    /// Windows requires of a private key.
    pub fn is_private(&self) -> bool {
        self.owner_trusted() && !self.others_can_read && !self.others_can_write
    }

    /// A machine-wide file (or its folder) that can be trusted for every
    /// user, such as `%PROGRAMDATA%\ssh\ssh_known_hosts`: owned by the
    /// machine (not by an account, not even the current user's — anyone
    /// can create files in `%PROGRAMDATA%`), and no other account can
    /// change it.
    pub fn machine_owned_and_locked(&self) -> bool {
        self.owner.is_privileged() && !self.others_can_write
    }
}

/// Whether the server end of an `ssh-agent` named pipe may receive keys.
///
/// - `server_user`: the user of the server process's token
///   ([`pipe_server_user`]), or `None` when that token cannot be opened (a
///   process of another account, or a protected service).
/// - `pipe`: the pipe's owner and DACL ([`pipe_audit`]), or `None` when
///   they cannot be read. The owner is set from the token of the process
///   that created the pipe: another unprivileged account cannot make it
///   anything but itself.
///
/// The server's token decides when it is readable: the agent service
/// (`SYSTEM`) and an agent run by the current user (1Password,
/// `KeePassXC`…) are accepted, whether the pipe can be read or not. A pipe
/// that *is* read and owned by another account is still refused (a server
/// handle passed on to one of our processes). When the token is
/// unreadable, only a pipe that is read, owned by the machine (`SYSTEM`,
/// `Administrators`) and on which no other account may add an instance
/// passes; an unreadable pipe is then refused.
pub fn agent_server_trusted(server_user: Option<Principal>, pipe: Option<&Audit>) -> bool {
    match server_user {
        Some(user) => {
            matches!(user, Principal::CurrentUser | Principal::System)
                && pipe.is_none_or(|pipe| {
                    matches!(
                        pipe.owner,
                        Principal::CurrentUser | Principal::System | Principal::Administrators
                    )
                })
        }
        None => pipe.is_some_and(|pipe| {
            matches!(pipe.owner, Principal::System | Principal::Administrators)
                && !pipe.others_can_add_instance
        }),
    }
}

/// Access, share and flag bits for opening a handle that [`audit`] and
/// [`restrict_to_owner`] accept (`OpenOptionsExt::access_mode`,
/// `share_mode`, `custom_flags`).
#[cfg(windows)]
pub mod access {
    pub use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
    pub use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
        WRITE_OWNER,
    };

    /// `MAXIMUM_ALLOWED` (`winnt.h`): with the rights a handle needs, every
    /// other right the DACL grants. Lets an open of an existing object pick
    /// up `WRITE_OWNER` when it is granted without failing when it is not.
    pub const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe(owner: Principal, others_can_add_instance: bool) -> Audit {
        Audit {
            owner,
            others_can_read: true,
            others_can_write: true,
            others_can_add_instance,
            protected: false,
        }
    }

    #[test]
    fn agent_servers() {
        use Principal::{Administrators, CurrentUser, Other, System, TrustedInstaller};
        let trusted = |owner: Option<Principal>, server| {
            agent_server_trusted(server, owner.map(|o| pipe(o, false)).as_ref())
        };
        // The Windows service, and an agent of this user (elevated: the
        // pipe is owned by `Administrators`).
        assert!(trusted(Some(System), Some(System)));
        assert!(trusted(Some(Administrators), Some(System)));
        assert!(trusted(Some(CurrentUser), Some(CurrentUser)));
        assert!(trusted(Some(Administrators), Some(CurrentUser)));
        // A readable token decides, even when the pipe cannot be read.
        assert!(trusted(None, Some(CurrentUser)));
        assert!(trusted(None, Some(System)));
        assert!(!trusted(None, Some(Other)));
        assert!(!trusted(None, Some(Administrators)));
        // Unreadable token: a machine-owned pipe only.
        assert!(trusted(Some(Administrators), None));
        assert!(trusted(Some(System), None));
        assert!(!trusted(Some(CurrentUser), None));
        assert!(!trusted(Some(Other), None));
        assert!(!trusted(Some(TrustedInstaller), None));
        assert!(!trusted(None, None));
        // Another account's pipe, whatever the server says.
        assert!(!trusted(Some(Other), Some(CurrentUser)));
        assert!(!trusted(Some(Other), Some(System)));
        assert!(!trusted(Some(TrustedInstaller), Some(System)));
        // A pipe of ours served by another account (handle passed on).
        assert!(!trusted(Some(CurrentUser), Some(Other)));
        assert!(!trusted(Some(System), Some(Other)));
    }

    #[test]
    fn agent_servers_refuse_a_machine_pipe_open_to_instances() {
        use Principal::{CurrentUser, System};
        assert!(!agent_server_trusted(None, Some(&pipe(System, true))));
        assert!(agent_server_trusted(None, Some(&pipe(System, false))));
        // The token decides when it is readable.
        assert!(agent_server_trusted(
            Some(CurrentUser),
            Some(&pipe(CurrentUser, true))
        ));
    }

    fn audit(owner: Principal, others_can_write: bool) -> Audit {
        Audit {
            owner,
            others_can_read: true,
            others_can_write,
            others_can_add_instance: others_can_write,
            protected: false,
        }
    }

    #[test]
    fn machine_files() {
        for owner in [
            Principal::System,
            Principal::Administrators,
            Principal::TrustedInstaller,
        ] {
            assert!(audit(owner, false).machine_owned_and_locked(), "{owner:?}");
            assert!(!audit(owner, true).machine_owned_and_locked(), "{owner:?}");
        }
        // Readable by everyone is expected of a system known_hosts.
        assert!(audit(Principal::System, false).machine_owned_and_locked());
        for owner in [Principal::CurrentUser, Principal::Other] {
            assert!(!audit(owner, false).machine_owned_and_locked(), "{owner:?}");
        }
    }

    #[test]
    fn private_files() {
        let private = Audit {
            owner: Principal::CurrentUser,
            others_can_read: false,
            others_can_write: false,
            others_can_add_instance: false,
            protected: true,
        };
        assert!(private.is_private());
        assert!(
            !Audit {
                owner: Principal::Other,
                ..private
            }
            .is_private()
        );
        assert!(
            !Audit {
                others_can_read: true,
                ..private
            }
            .is_private()
        );
    }
}
