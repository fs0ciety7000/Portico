//! Process hardening applied at startup by both apps (Phase 4).
//!
//! Portico holds decrypted secrets in memory for short periods (passwords,
//! key passphrases, the vault key). [`harden_process`] reduces the ways
//! another process of the same user, or a crash, could read them:
//!
//! - **`RLIMIT_CORE = 0`** (Unix, soft *and hard*): no core dump is written
//!   on a crash. Resource limits are inherited across `fork` and `execve`,
//!   and an unprivileged process cannot raise a hard limit, so this one
//!   covers **every** child process too (including the `WebKitGTK` helper
//!   processes of the GUI on Linux);
//! - **`PR_SET_DUMPABLE = 0`** (Linux): the *Rust process itself* cannot be
//!   attached with `ptrace` by other processes of the same user (root and
//!   `CAP_SYS_PTRACE` aside), and `/proc/<pid>/mem`, `environ`… become
//!   root-owned. Also disables core dumps independently of the rlimit.
//!
//! What this does **not** cover: the dumpable flag is reset by the kernel on
//! `execve` of an ordinary (non set-uid) binary. On Linux the GUI's
//! `WebKitWebProcess` and `WebKitNetworkProcess`, which `WebKitGTK` starts with
//! `exec`, are therefore traceable again by the same user. The web process
//! renders the UI, so it briefly sees what the user types in a prompt field
//! (password, passphrase) before it crosses the IPC; keys, the vault key and
//! the SSH transport stay in the Rust process. Core dumps stay blocked in
//! those helpers through the inherited hard `RLIMIT_CORE`. Linux distributions
//! that set `kernel.yama.ptrace_scope >= 1` already restrict same-user
//! `ptrace` to ancestors, which covers that gap for most desktops.
//!
//! macOS: there is no safe wrapper for `ptrace(PT_DENY_ATTACH)` in the crates
//! already in the tree (rustix, nix), and `unsafe` is forbidden in the
//! workspace. The signed and notarized app uses the hardened runtime
//! (without the `get-task-allow` entitlement), which already refuses
//! debugger attachment; the TUI binary only gets `RLIMIT_CORE = 0`. This is
//! reported as [`StepStatus::Unsupported`].
//!
//! Windows (Phase 8): neither measure is applied, both are reported as
//! [`StepStatus::Unsupported`]. There is no `RLIMIT_CORE`: Windows Error
//! Reporting writes a local crash dump only when an administrator enabled
//! `LocalDumps` in the registry, and opting one program out
//! (`WerAddExcludedApplication`) is a per-user registry write we do not make
//! behind the user's back. Other processes of the same user can open this
//! one (`PROCESS_VM_READ`), as on macOS without the hardened runtime: a
//! process DACL would not stop them (the owner can always rewrite it). No
//! measure needs administrator rights, so none is attempted.
//!
//! Hardening is best effort: a failure is reported, never fatal.

use serde::Serialize;

/// What happened to one hardening measure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepStatus {
    Applied,
    /// The system refused (message without secret: an errno description).
    Failed {
        reason: String,
    },
    /// Not available on this platform.
    Unsupported {
        reason: &'static str,
    },
}

/// One hardening measure and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HardeningStep {
    /// Stable name: `core_dumps`, `ptrace`.
    pub name: &'static str,
    pub status: StepStatus,
}

/// Outcome of [`harden_process`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HardeningReport {
    pub steps: Vec<HardeningStep>,
}

impl HardeningReport {
    /// Whether every measure was applied.
    pub fn fully_applied(&self) -> bool {
        self.steps.iter().all(|s| s.status == StepStatus::Applied)
    }

    /// Logs the report (`info` when complete, `warn` per missing measure).
    pub fn log(&self) {
        for step in &self.steps {
            match &step.status {
                StepStatus::Applied => tracing::debug!(step = step.name, "hardening applied"),
                StepStatus::Failed { reason } => {
                    tracing::warn!(step = step.name, reason = %reason, "hardening failed");
                }
                StepStatus::Unsupported { reason } => {
                    tracing::info!(step = step.name, reason, "hardening unavailable");
                }
            }
        }
    }
}

/// Applies every measure described in the module doc. Call it first thing
/// in `main`, before any secret is read. Never fails: see the report.
pub fn harden_process() -> HardeningReport {
    HardeningReport {
        steps: vec![
            HardeningStep {
                name: "core_dumps",
                status: disable_core_dumps(),
            },
            HardeningStep {
                name: "ptrace",
                status: deny_ptrace(),
            },
        ],
    }
}

#[cfg(unix)]
fn disable_core_dumps() -> StepStatus {
    use rustix::process::{Resource, Rlimit, setrlimit};
    let zero = Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    match setrlimit(Resource::Core, zero) {
        Ok(()) => StepStatus::Applied,
        Err(e) => StepStatus::Failed {
            reason: format!("setrlimit(RLIMIT_CORE): {e}"),
        },
    }
}

#[cfg(windows)]
fn disable_core_dumps() -> StepStatus {
    StepStatus::Unsupported {
        reason: "no RLIMIT_CORE on Windows; WER dumps only if an administrator enabled LocalDumps",
    }
}

#[cfg(not(any(unix, windows)))]
fn disable_core_dumps() -> StepStatus {
    StepStatus::Unsupported {
        reason: "no RLIMIT_CORE on this platform",
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn deny_ptrace() -> StepStatus {
    use rustix::process::{DumpableBehavior, set_dumpable_behavior};
    match set_dumpable_behavior(DumpableBehavior::NotDumpable) {
        Ok(()) => StepStatus::Applied,
        Err(e) => StepStatus::Failed {
            reason: format!("prctl(PR_SET_DUMPABLE): {e}"),
        },
    }
}

#[cfg(target_os = "macos")]
fn deny_ptrace() -> StepStatus {
    StepStatus::Unsupported {
        reason: "PT_DENY_ATTACH has no safe wrapper; rely on the hardened runtime",
    }
}

#[cfg(windows)]
fn deny_ptrace() -> StepStatus {
    StepStatus::Unsupported {
        reason: "same-user processes can open this one; no per-process opt-out on Windows",
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn deny_ptrace() -> StepStatus {
    StepStatus::Unsupported {
        reason: "no ptrace protection on this platform",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardening_disables_core_dumps_and_ptrace() {
        let report = harden_process();
        assert_eq!(report.steps.len(), 2);
        #[cfg(unix)]
        {
            let limit = rustix::process::getrlimit(rustix::process::Resource::Core);
            assert_eq!(limit.current, Some(0), "{report:?}");
            assert_eq!(limit.maximum, Some(0), "{report:?}");
        }
        #[cfg(target_os = "linux")]
        {
            assert!(report.fully_applied(), "{report:?}");
            assert_eq!(
                rustix::process::dumpable_behavior().expect("prctl"),
                rustix::process::DumpableBehavior::NotDumpable
            );
            // /proc/self stays readable by the process itself.
            assert!(std::fs::read_to_string("/proc/self/status").is_ok());
        }
        #[cfg(windows)]
        assert!(
            report
                .steps
                .iter()
                .all(|s| matches!(s.status, StepStatus::Unsupported { .. })),
            "{report:?}"
        );
        // Idempotent.
        let again = harden_process();
        #[cfg(target_os = "linux")]
        assert!(again.fully_applied(), "{again:?}");
        let json = serde_json::to_string(&again).expect("json");
        assert!(json.contains("core_dumps"), "{json}");
        again.log();
    }
}
