//! Encrypted folder sync (phase 11, `docs/SYNC.md`).
//!
//! Portico synchronizes hosts, host metadata, snippets and a whitelist of
//! settings between the user's machines through a folder that a service
//! the user already runs (Dropbox, iCloud Drive, Syncthing, a git
//! checkout…) replicates. Portico itself only does local file I/O there;
//! the folder only ever holds ciphertext.
//!
//! ## Delivery
//!
//! **S1a (this code): the pure foundations.** Nothing here touches the
//! disk, the network, the keyring or the engine: every function takes and
//! returns bytes or values, so it is deterministic to test. The engine
//! wiring (folder I/O, projection of local files, apply, background task,
//! events) is S1b; the UIs are S1c; passwords, key rotation and device
//! revocation are S2/S3.
//!
//! ## Layout
//!
//! | Module | Role |
//! |---|---|
//! | [`crypto`] | Argon2id with sync-specific bounds, HKDF subkeys, XChaCha20-Poly1305 sealing, canonical encoding, HMAC with constant-time compare, padding buckets |
//! | [`bundle`] | `bundle.toml`: KDF parameters, passphrase key slot, sealed roster, header MAC; create / open / change passphrase |
//! | [`container`] | Sealed device snapshots (`devices/<id>.snap`) and the sealed local replica |
//! | [`clock`] | Device ids, dots, version vectors, multi-value registers, the local dot issuer |
//! | [`model`] | Record keys and values, the synced state and its merge, conflicts, tombstone GC, opaque passthrough |
//! | [`guard`] | Rollback, fork, downgrade and restored-state detection (high-water marks) |
//! | [`passphrase`] | Diceware generator (EFF large wordlist) and the enforced strength minimum |
//!
//! ## Invariants (all modules)
//!
//! - Key material lives in [`zeroize::Zeroizing`] buffers and types whose
//!   `Debug` is redacted. No key, passphrase or decrypted value reaches an
//!   error message, a log or an event.
//! - Every parser is bounded (file size, counts, string lengths) **before**
//!   any expensive work, and the KDF parameters are checked against
//!   [`crypto::KdfBounds`] before any memory is allocated.
//! - Authentication comes first: nothing read from the folder is used, and
//!   no decision that could lead to a write is taken, before its AEAD tag
//!   or MAC has been verified.
//! - The wall clock is display only; ordering and deletion use dots and
//!   version vectors exclusively.

// S1a ships the foundations without their consumer: the engine wiring
// (S1b) is the only caller of these crate-internal modules. Remove these
// `allow`s with S1b; the tests exercise every item meanwhile.
#[allow(dead_code)]
pub(crate) mod clock;
#[allow(dead_code)]
pub(crate) mod crypto;

pub use clock::DeviceId;

/// Errors of the sync foundations. Messages name the file kind, the field
/// or the device (by id), never a value, a key or a passphrase.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// The passphrase does not open the key slot. A tampered slot, salt or
    /// KDF parameter gives the same error on purpose: there is no oracle.
    #[error("wrong sync passphrase")]
    WrongPassphrase,

    #[error(
        "the sync passphrase is too weak (about {estimated_bits} bits, at least {required_bits} needed, or {required_words} generated words)"
    )]
    PassphraseWeak {
        estimated_bits: u32,
        required_bits: u32,
        required_words: usize,
    },

    #[error("sync {what} is corrupt: {reason}")]
    Corrupt {
        what: &'static str,
        reason: &'static str,
    },

    #[error(
        "sync {what} format version {found} is not supported (this Portico reads version {supported})"
    )]
    UnsupportedVersion {
        what: &'static str,
        found: u32,
        supported: u32,
    },

    /// A file of an older format than one already seen from this bundle.
    #[error("sync {what} format version {found} is older than version {seen} already seen")]
    FormatDowngrade {
        what: &'static str,
        found: u32,
        seen: u32,
    },

    #[error("sync key derivation {param} = {value} is outside the accepted range {min}..={max}")]
    KdfOutOfBounds {
        param: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },

    #[error("not enough memory for the sync key derivation ({memory_kib} KiB)")]
    KdfMemory { memory_kib: u32 },

    /// The key slot opened but the header MAC or the sealed roster does
    /// not verify: someone without the key edited `bundle.toml`.
    #[error("sync bundle.toml was modified without the key")]
    BundleTampered,

    /// A snapshot or replica failed authentication (tampered, torn, or
    /// sealed for another device, sequence or bundle).
    #[error("sync {what} of device {device} failed authentication")]
    Unauthenticated { what: &'static str, device: String },

    /// An older copy of a file than one already verified.
    #[error("sync {what} was replaced by an older copy")]
    Rollback { what: String },

    /// Same sequence, different content: one device identity writes from
    /// two machines (or a replay with altered content).
    #[error("sync device {device} published two different snapshots with the same sequence")]
    Fork { device: String },

    /// This device's own published state is ahead of its local state: the
    /// data dir was restored from a backup or cloned. It must re-join under
    /// a new device id before writing anything.
    #[error("the local sync state of device {device} is older than what it already published")]
    RestoredState { device: String },

    #[error("sync {what} is larger than {max} bytes")]
    TooLarge { what: &'static str, max: usize },

    #[error("invalid sync device name: {reason}")]
    InvalidDeviceName { reason: &'static str },

    #[error("invalid sync record {kind}.{field}: {reason}")]
    InvalidRecord {
        kind: &'static str,
        field: &'static str,
        reason: &'static str,
    },

    #[error("cannot resolve this sync conflict: {reason}")]
    Resolution { reason: &'static str },

    #[error("sync cryptography failed: {0}")]
    Crypto(&'static str),
}

impl SyncError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::WrongPassphrase => (
                "sync_wrong_passphrase",
                Some(
                    "Check the sync passphrase (keyboard layout, Caps Lock). It is the one chosen on the first device.",
                ),
            ),
            Self::PassphraseWeak { .. } => (
                "sync_passphrase_weak",
                Some(
                    "The sync folder can be attacked offline: use “Generate a passphrase” (6 words) or a longer phrase.",
                ),
            ),
            Self::Corrupt { .. } => (
                "sync_corrupt",
                Some(
                    "A sync file is damaged. Wait for your sync service to finish, or remove this device's file from the folder and sync again.",
                ),
            ),
            Self::UnsupportedVersion { .. } => (
                "sync_version",
                Some("Another device runs a newer Portico: update Portico on this device."),
            ),
            Self::FormatDowngrade { .. } => (
                "sync_version",
                Some(
                    "An older Portico file replaced a newer one in the sync folder: check the folder's history and your devices.",
                ),
            ),
            Self::KdfOutOfBounds { .. } => (
                "sync_kdf_out_of_bounds",
                Some(
                    "bundle.toml asks for an unsafe key derivation: it was not written by Portico. Do not type your passphrase for this folder.",
                ),
            ),
            Self::KdfMemory { .. } => (
                "sync_kdf_memory",
                Some("Close memory-hungry applications and try again."),
            ),
            Self::BundleTampered => (
                "sync_bundle_tampered",
                Some(
                    "bundle.toml was modified outside Portico. Nothing was applied. Check who can write to the sync folder.",
                ),
            ),
            Self::Unauthenticated { .. } => (
                "sync_snapshot_invalid",
                Some(
                    "The file may still be uploading. If it persists, the sync folder was modified outside Portico.",
                ),
            ),
            Self::Rollback { .. } => (
                "sync_rollback",
                Some(
                    "An older copy of a sync file was put back (restore, provider history, or an attacker). It was ignored.",
                ),
            ),
            Self::Fork { .. } => (
                "sync_fork",
                Some(
                    "The same Portico profile runs on two machines (cloned profile or restored backup). That device must re-join the sync.",
                ),
            ),
            Self::RestoredState { .. } => (
                "sync_restored",
                Some(
                    "This device's sync state was restored from a backup: Portico re-joins the sync under a new device identity.",
                ),
            ),
            Self::TooLarge { .. } => (
                "sync_too_large",
                Some("Too much data to sync: remove unused hosts or snippets."),
            ),
            Self::InvalidDeviceName { .. } => (
                "sync_device_name_invalid",
                Some("Use a short device name (1 to 64 characters, no control characters)."),
            ),
            Self::InvalidRecord { .. } => (
                "sync_record_invalid",
                Some(
                    "A synced entry was refused on this device and kept aside; edit it on the device that created it.",
                ),
            ),
            Self::Resolution { .. } => ("sync_resolution_invalid", None),
            Self::Crypto(_) => ("sync_crypto", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_a_sync_code() {
        let samples = [
            SyncError::WrongPassphrase,
            SyncError::PassphraseWeak {
                estimated_bits: 20,
                required_bits: 60,
                required_words: 5,
            },
            SyncError::Corrupt {
                what: "bundle.toml",
                reason: "x",
            },
            SyncError::UnsupportedVersion {
                what: "bundle.toml",
                found: 2,
                supported: 1,
            },
            SyncError::FormatDowngrade {
                what: "snapshot",
                found: 1,
                seen: 2,
            },
            SyncError::KdfOutOfBounds {
                param: "memory_kib",
                value: 1,
                min: 2,
                max: 3,
            },
            SyncError::KdfMemory { memory_kib: 1 },
            SyncError::BundleTampered,
            SyncError::Unauthenticated {
                what: "snapshot",
                device: "ab".into(),
            },
            SyncError::Rollback { what: "x".into() },
            SyncError::Fork {
                device: "ab".into(),
            },
            SyncError::RestoredState {
                device: "ab".into(),
            },
            SyncError::TooLarge {
                what: "snapshot",
                max: 1,
            },
            SyncError::InvalidDeviceName { reason: "x" },
            SyncError::InvalidRecord {
                kind: "host",
                field: "alias",
                reason: "x",
            },
            SyncError::Resolution { reason: "x" },
            SyncError::Crypto("x"),
        ];
        for error in samples {
            let summary = crate::Error::from(error).summary();
            assert!(summary.code.starts_with("sync_"), "{}", summary.code);
            assert!(!summary.message.is_empty());
            // Messages start lowercase (project convention).
            assert!(
                summary
                    .message
                    .chars()
                    .next()
                    .is_some_and(|c| !c.is_uppercase()),
                "{}",
                summary.message
            );
        }
    }
}
