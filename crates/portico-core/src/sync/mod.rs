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
//! - **S1a**: the pure foundations (crypto, bundle, containers, clocks and
//!   merge, guards, passphrase): no disk, network, keyring or engine.
//! - **S1b**: the engine wiring: the folder ([`folder`], safe root F9),
//!   the projection of the local files ([`project`]), the validated
//!   application ([`apply`], F1/F7/F8), the local state ([`state`]), the
//!   pass and the user operations ([`runner`]), the public types ([`api`]);
//!   the `Engine::sync_*` methods and the background task live in
//!   `engine::sync_api`.
//! - S1c: the UIs.
//! - **S2a**: bundle v2 (epoch commitment, chained lineage, roster with
//!   forgotten devices and the password policy), key rotation and
//!   adoption going forward only, forgetting a device, the local epoch
//!   floor, `IndexedSecretStore` (`docs/SYNC.md` §13). S2b/S3: passwords,
//!   device slots.
//!
//! No network call, ever: the folder is only read and written locally.
//!
//! ## Layout
//!
//! | Module | Role |
//! |---|---|
//! | [`crypto`] | Argon2id with sync-specific bounds, HKDF subkeys, XChaCha20-Poly1305 sealing, canonical encoding, HMAC with constant-time compare, padding buckets |
//! | [`bundle`] | `bundle.toml`: KDF parameters, passphrase key slot, sealed roster, header MAC; create / open / change passphrase |
//! | [`container`] | Sealed device snapshots (`devices/<id>.snap`) and the sealed local replica |
//! | [`clock`] | Device ids, dots, version vectors, multi-value registers, the local dot issuer |
//! | [`model`] | Record keys and values, the synced state (generic keyed registers of canonical JSON) and its transactional merge, typed projection, conflicts, tombstone GC |
//! | [`guard`] | Rollback, fork, downgrade and restored-state detection (high-water marks), exact bounds of forgotten devices |
//! | [`lineage`] | Judging a key lineage: rotation elsewhere, fork, forgery (S2a) |
//! | [`passphrase`] | Diceware generator (EFF large wordlist) and the enforced strength minimum |
//! | [`folder`] | `FolderIo`: listing, capped reads, atomic writes in `portico-sync/`, safe root (F9), limits (F15) |
//! | [`state`] | `data_dir/sync/`: `sync.toml`, sealed replica, snippet confirmations, `sync.lock` |
//! | [`project`] | Local files → records, local changes against the last applied projection (F7) |
//! | [`apply`] | Validated application: quarantine, shadowing, confirmations (F1, F8) |
//! | [`runner`] | One pass, create / join / unlock / resolve / confirm / disable |
//! | [`api`] | Statuses, devices, conflicts, issues, pending confirmations (UI types) |
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

pub mod api;
pub(crate) mod apply;
pub(crate) mod bundle;
pub(crate) mod clock;
pub(crate) mod container;
pub(crate) mod crypto;
pub(crate) mod folder;
pub(crate) mod guard;
pub(crate) mod lineage;
pub(crate) mod model;
pub mod passphrase;
pub(crate) mod project;
pub(crate) mod runner;
pub(crate) mod state;

pub use api::{
    AlertKind, ConflictSide, DisableOptions, FolderProbe, FolderReason, JoinBulkChoice,
    JoinPreview, PendingHostChange, SyncConflict, SyncCreate, SyncDevice, SyncHostValue, SyncIssue,
    SyncJoin, SyncOptions, SyncOverview, SyncResolution, SyncSnippetValue, SyncStatus, SyncSubject,
    SyncValue, UnconfirmedSnippet, WaitReason,
};
pub use clock::DeviceId;
pub use passphrase::{
    GeneratedPassphrase, NewPassphrase, PassphraseStrength, assess_passphrase, generate_passphrase,
};

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

    /// Another device's snapshot claims to know dots of this device that
    /// this device never issued (S1a review finding 3). Only evidence in
    /// this device's own files can mean "restored"; a third-party claim is
    /// an alert naming the claimant, never an automatic re-join.
    #[error("sync device {claimant} claims changes of this device ({device}) that it never made")]
    ThirdPartyClaim { device: String, claimant: String },

    /// Merging a device's snapshot would break the limits of the state
    /// (S1a review finding 2): nothing was merged.
    #[error("sync snapshot of device {device} was not merged: {reason}")]
    MergeRefused {
        device: String,
        reason: &'static str,
    },

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

    /// A newer Portico wrote this record: this version keeps and relays
    /// it but cannot change it (S1a review finding 4).
    #[error("this sync {kind} entry was written by a newer Portico")]
    RecordHeld { kind: &'static str },

    #[error("cannot resolve this sync conflict: {reason}")]
    Resolution { reason: &'static str },

    #[error("sync cryptography failed: {0}")]
    Crypto(&'static str),

    // ── S1b: folder, engine wiring, application ─────────────────────────
    /// The chosen folder (or `portico-sync/` in it) is gone: unmounted
    /// drive, provider not running. Never recreated automatically.
    #[error("the sync folder is missing")]
    FolderMissing,

    /// `portico-sync/` exists but holds no `bundle.toml`.
    #[error("the sync folder holds no Portico sync bundle")]
    FolderNotPortico,

    /// "Create" on a folder that already holds a bundle.
    #[error("the sync folder already holds a Portico sync bundle")]
    BundleExists,

    #[error("the sync folder cannot be written")]
    FolderUnwritable,

    /// A link, a reparse point, or a folder inside `~/.ssh`, the config
    /// or the data directory (F9).
    #[error("the sync folder is not usable: {reason}")]
    FolderUnsafe { reason: &'static str },

    /// More device files than the listing limit: the sync pauses without
    /// deleting anything (F15).
    #[error("the sync folder holds more than {max} device files")]
    FolderTooManyEntries { max: usize },

    #[error("the sync folder did not answer within {secs} seconds")]
    FolderTimeout { secs: u64 },

    #[error("sync folder {op} failed ({kind})")]
    FolderIo { op: &'static str, kind: String },

    /// The data key is not in memory nor remembered: the passphrase is
    /// needed on this device.
    #[error("sync is locked on this device")]
    Locked,

    #[error("sync is not enabled on this device")]
    Disabled,

    #[error("sync is already enabled on this device")]
    AlreadyEnabled,

    /// A join token that expired, was already used, or never existed (F14).
    #[error("this sync join expired or was already completed")]
    JoinExpired,

    /// Another sync operation is still running (a stalled folder).
    #[error("sync is busy")]
    Busy,

    /// `hosts.conf` holds something Portico would not rewrite: the hosts
    /// are neither projected nor applied until it is fixed.
    #[error("hosts.conf was edited by hand: synced hosts are paused")]
    HostsFileUnmanaged,

    /// A received host would shadow a name of the user's own config
    /// (F1): kept aside, never written.
    #[error("synced host `{alias}` would shadow a host name of your ssh config")]
    AliasShadowed { alias: String },

    #[error("synced host `{alias}` jumps through `{jump}`, which is not known on this device")]
    JumpMissing { alias: String, jump: String },

    /// A warning: the host is written, its key is missing here.
    #[error("the key of synced host `{alias}` is not on this device")]
    KeyMissing { alias: String },

    #[error("synced host `{alias}` was not removed: it is the jump host of `{by}`")]
    HostInUse { alias: String, by: String },

    /// Nothing awaits confirmation under that name, or what awaits is no
    /// longer what the user was shown (the hash differs): the UI reloads
    /// the list and shows the new content.
    #[error("no synced change of `{alias}` awaits confirmation")]
    NotPending { alias: String },

    /// A received host whose alias this device holds in a form that cannot
    /// be synced (an identity file outside the home directory…): the local
    /// host is kept, never overwritten nor deleted by sync.
    #[error("synced host `{alias}` was not applied: this device's own `{alias}` cannot be synced")]
    LocalHostUnsynced { alias: String },

    /// The same for host metadata, a snippet or a setting held here in a
    /// form that cannot be synced.
    #[error("a synced {kind} entry was not applied: this device's own value cannot be synced")]
    LocalRecordUnsynced { kind: &'static str },

    /// F8: a snippet changed by sync runs only once confirmed here.
    #[error("snippet `{id}` was changed by sync: review and confirm it before running it")]
    SnippetUnconfirmed { id: String },

    /// The key slots changed on another device (passphrase changed or
    /// reset): an alert to acknowledge.
    #[error("the sync passphrase was changed or reset on another device")]
    PassphraseChanged,

    /// A newer epoch (key rotation, S2).
    #[error("the sync keys were rotated on another device")]
    KeysRotated,

    /// A device holding the key sent another value for a change already
    /// received (a misbehaving device): the first value was kept.
    #[error("sync device {device} sent {count} conflicting copies of changes already received")]
    DivergentDots { device: String, count: usize },

    /// Values removed by a device's context without a replacement (S1a
    /// review finding 3).
    #[error("sync device {device} removed {count} values without replacing them")]
    DroppedWithoutSuccessor { device: String, count: usize },

    /// A snapshot whose context claims changes not yet in the folder: not
    /// merged until they arrive (S1a review finding 3).
    #[error("the sync data of device {device} waits for other devices' files")]
    Deferred { device: String },

    /// A device file not readable yet (uploading, cloud placeholder).
    #[error("the sync file of device {device} is not available yet")]
    PendingFile { device: String },

    // ── S2a: rotation, forgetting a device (`docs/SYNC.md` §13) ─────────
    /// The new passphrase opens a key slot this device already verified
    /// (the current one or one of its history, RR1): a rotation must
    /// change it, and an adoption under it did not come from the user.
    #[error("this sync passphrase opens older sync keys")]
    PassphraseReused,

    /// Two devices rotated the keys from the same epoch (§13.3.7).
    #[error("two devices changed the sync keys at the same time")]
    RotationConflict,

    /// The other lineage of a rotation conflict still lists a device this
    /// device forgot (E2): adopting it would bring that device back.
    #[error("the other device's sync keys still include forgotten device {device}")]
    ConflictKeepsForgotten { device: String },

    /// This device is in the roster's forgotten list (§13.4.4).
    #[error("this device was removed from the sync by {by}")]
    DeviceForgotten { by: String, at: i64 },

    #[error("this device cannot forget itself")]
    ForgetSelf,

    #[error("sync device {device} is not in the device list")]
    DeviceUnknown { device: String },

    /// The roster's forgotten list is full (it is never pruned, R4).
    #[error("the sync bundle already lists {max} forgotten devices")]
    RosterFull { max: usize },

    /// A snapshot carries changes of a forgotten device beyond its exact
    /// bound (§13.4.2): not merged.
    #[error("sync device {device} relays changes of forgotten device {forgotten}")]
    RevokedWrites { device: String, forgotten: String },

    /// A local value written by a forgotten device (or of unknown origin
    /// after a re-join, RR4) waits for the user's decision.
    #[error("a synced {kind} entry here was written by {device}: keep it or take the synced value")]
    RevokedValue { kind: &'static str, device: String },

    /// Devices whose last verified snapshot announces Portico < 0.7: they
    /// stop syncing at the first S2 operation (Q5).
    #[error("{count} sync devices run a Portico older than 0.7")]
    PeersOutdated { count: usize },

    #[error("this sync key entry is the key in use")]
    KeyEntryCurrent,

    #[error("this keychain entry is not an old sync key")]
    KeyEntryInvalid,

    /// The new keys cannot be checked against this device's lineage: its
    /// local record of the current epoch is lost.
    #[error("this device cannot check the new sync keys")]
    LineageUnknown,

    /// A key rotation (or adoption) is written locally and waits for the
    /// folder (§13.3.4): it only ever goes forward.
    #[error("a change of the sync keys waits for the sync folder")]
    RotationPending,

    /// A device file announces keys newer than this device's (§13.3.5).
    #[error("the sync file of device {device} announces newer keys")]
    NewerEpochFile { device: String },

    /// An active device still publishes under older keys: its file is
    /// never merged until it types the new passphrase (§13.3.6).
    #[error("sync device {device} still uses the previous sync keys")]
    NeedsNewKeys { device: String },
}

impl SyncError {
    // One arm per variant, each with its hint: splitting it would only
    // scatter a flat table.
    #[allow(clippy::too_many_lines)]
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
                "sync_rollback",
                Some(
                    "A sync file was put back to an earlier version: open Portico on an up-to-date device.",
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
                    "A sync file was put back to an earlier version (restore, provider history, or an attacker). It was ignored: open Portico on an up-to-date device.",
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
            Self::ThirdPartyClaim { .. } => (
                "sync_third_party_claim",
                Some(
                    "Another device's sync file claims changes this device never made. Nothing was merged from it; check that device.",
                ),
            ),
            Self::MergeRefused { .. } => (
                "sync_merge_refused",
                Some(
                    "That device's data would exceed the sync limits: nothing was merged from it. Check that device.",
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
            Self::RecordHeld { .. } => (
                "sync_record_needs_newer",
                Some(
                    "Another device runs a newer Portico and wrote this entry: update Portico on this device to change it.",
                ),
            ),
            Self::Resolution { .. } => ("sync_resolution_invalid", None),
            Self::Crypto(_) => ("sync_crypto", None),
            Self::FolderMissing => (
                "sync_folder_missing",
                Some(
                    "Check that the drive is mounted and your sync service runs. Portico waits and retries; nothing is recreated.",
                ),
            ),
            Self::FolderNotPortico => (
                "sync_folder_not_portico",
                Some(
                    "Wait for your sync service to download bundle.toml, or pick the folder chosen on your first device.",
                ),
            ),
            Self::BundleExists => (
                "sync_bundle_exists",
                Some("This folder already holds a sync: join it instead of creating a new one."),
            ),
            Self::FolderUnwritable => (
                "sync_folder_unwritable",
                Some(
                    "Portico can read the folder but not write to it: changes from other devices still arrive; check the folder's permissions.",
                ),
            ),
            Self::FolderUnsafe { .. } => (
                "sync_folder_unsafe",
                Some(
                    "Pick a plain folder of your sync service, outside ~/.ssh and Portico's own folders, without links inside portico-sync.",
                ),
            ),
            Self::FolderTooManyEntries { .. } => (
                "sync_folder_too_many",
                Some(
                    "The devices folder holds unexpected files: remove what is not a Portico device file. Sync is paused, nothing was deleted.",
                ),
            ),
            Self::FolderTimeout { .. } => (
                "sync_folder_timeout",
                Some(
                    "The folder is slow or stalled (network drive, files being downloaded). Portico retries later.",
                ),
            ),
            Self::FolderIo { .. } => (
                "sync_folder_io",
                Some("Check the sync folder and its permissions."),
            ),
            Self::Locked => (
                "sync_locked",
                Some("Enter the sync passphrase to unlock sync on this device."),
            ),
            Self::Disabled => (
                "sync_disabled",
                Some("Set up sync first: create a sync folder or join an existing one."),
            ),
            Self::AlreadyEnabled => (
                "sync_already_enabled",
                Some("Disable sync on this device before creating or joining another one."),
            ),
            Self::JoinExpired => (
                "sync_join_expired",
                Some("Start joining again: enter the passphrase and review the changes."),
            ),
            Self::Busy => (
                "sync_busy",
                Some("A sync operation is still running: try again in a moment."),
            ),
            Self::HostsFileUnmanaged => (
                "sync_hosts_file_unmanaged",
                Some(
                    "hosts.conf holds lines Portico does not manage: undo the hand edit (or move it to ~/.ssh/config); synced hosts resume then. Other data keeps syncing.",
                ),
            ),
            Self::AliasShadowed { .. } => (
                "sync_alias_shadowed",
                Some(
                    "Skip it on this device, or rename the host on another device: Portico never shadows a host of your own config.",
                ),
            ),
            Self::JumpMissing { .. } => (
                "sync_jump_missing",
                Some("It is applied once its jump host exists on this device."),
            ),
            Self::KeyMissing { .. } => (
                "sync_key_missing",
                Some("Copy the private key to this device, or edit the host to use a local key."),
            ),
            Self::HostInUse { .. } => (
                "sync_host_in_use",
                Some("Change the jump host of those hosts first."),
            ),
            Self::NotPending { .. } => (
                "sync_not_pending",
                Some("Reload the list of synced changes awaiting confirmation."),
            ),
            Self::LocalHostUnsynced { .. } => (
                "sync_local_host_unsynced",
                Some(
                    "This device's own version of the host cannot be synced (for example its key is outside your home folder): it is kept. Edit or rename it here to receive the synced one, or skip it on this device.",
                ),
            ),
            Self::LocalRecordUnsynced { .. } => (
                "sync_local_unsynced",
                Some(
                    "This device's own value cannot be synced and is kept: edit it here to receive the synced one, or skip it on this device.",
                ),
            ),
            Self::SnippetUnconfirmed { .. } => (
                "sync_snippet_unconfirmed",
                Some(
                    "Another device changed this snippet: read the whole command, then confirm it to run it here.",
                ),
            ),
            Self::PassphraseChanged => (
                "sync_passphrase_changed",
                Some(
                    "If you did not change it yourself, someone holding your sync passphrase or key did: check your devices.",
                ),
            ),
            Self::KeysRotated => (
                "sync_keys_rotated",
                Some(
                    "Type the new passphrase chosen on the other device. If you did not change the keys, type nothing.",
                ),
            ),
            Self::DivergentDots { .. } => (
                "sync_divergent",
                Some(
                    "That device sent two versions of one change: the first one was kept. Check that device.",
                ),
            ),
            Self::DroppedWithoutSuccessor { .. } => (
                "sync_dropped_values",
                Some(
                    "Another device's sync data removed values without replacing them: check that device.",
                ),
            ),
            Self::Deferred { .. } => (
                "sync_deferred",
                Some(
                    "Its changes are merged once the other devices' files arrive. If it lasts, check that device.",
                ),
            ),
            Self::PendingFile { .. } => (
                "sync_pending",
                Some(
                    "The file is still uploading or only a placeholder (iCloud: “Keep Downloaded”, OneDrive: “Always keep on this device”).",
                ),
            ),
            Self::PassphraseReused => (
                "sync_passphrase_reused",
                Some(
                    "This passphrase opens older keys: choose another one (rotation), or type nothing if you did not change the keys (adoption).",
                ),
            ),
            Self::RotationConflict => (
                "sync_rotation_conflict",
                Some(
                    "Two devices changed the keys at the same time: use the other device's keys or keep this device's.",
                ),
            ),
            Self::ConflictKeepsForgotten { .. } => (
                "sync_rotation_conflict",
                Some(
                    "The other device's keys still include a device you removed: keep this device's keys instead.",
                ),
            ),
            Self::DeviceForgotten { .. } => (
                "sync_device_forgotten",
                Some("This device was removed: turn sync off, or join again as a new device."),
            ),
            Self::ForgetSelf => (
                "sync_forget_self",
                Some("To remove this device, use Turn sync off."),
            ),
            Self::DeviceUnknown { .. } => {
                ("sync_device_unknown", Some("Reload the list of devices."))
            }
            Self::RosterFull { .. } => (
                "sync_roster_full",
                Some("Too many removed devices: create a new sync bundle."),
            ),
            Self::RevokedWrites { .. } => (
                "sync_revoked_writes",
                Some(
                    "That device relays changes of a removed device: type the new passphrase on that device.",
                ),
            ),
            Self::RevokedValue { .. } => (
                "sync_revoked_value",
                Some(
                    "Value written by a removed device (or of unknown origin): keep it, or take the synced value.",
                ),
            ),
            Self::PeersOutdated { .. } => (
                "sync_peers_outdated",
                Some("Update Portico on these devices (0.7 or later)."),
            ),
            Self::KeyEntryCurrent => (
                "sync_key_entry_current",
                Some("This is the key in use: use Turn sync off."),
            ),
            Self::KeyEntryInvalid => (
                "sync_key_entry_invalid",
                Some("Only old sync keys can be removed here."),
            ),
            Self::LineageUnknown => (
                "sync_lineage_unknown",
                Some(
                    "This device lost its record of the current keys: turn sync off, then join again as a new device.",
                ),
            ),
            Self::RotationPending => (
                "sync_rotation_pending",
                Some(
                    "The new keys are saved on this device: Portico writes them to the folder as soon as it answers.",
                ),
            ),
            Self::NeedsNewKeys { .. } => (
                "sync_needs_new_keys",
                Some("Type the new sync passphrase on that device."),
            ),
            Self::NewerEpochFile { .. } => (
                "sync_keys_rotated",
                Some(
                    "A device file announces newer keys. Type the new passphrase if someone changed the keys; otherwise remove that file.",
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One sample per variant: a flat list.
    #[allow(clippy::too_many_lines)]
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
            SyncError::ThirdPartyClaim {
                device: "ab".into(),
                claimant: "cd".into(),
            },
            SyncError::MergeRefused {
                device: "ab".into(),
                reason: "x",
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
            SyncError::RecordHeld { kind: "host" },
            SyncError::Resolution { reason: "x" },
            SyncError::Crypto("x"),
            SyncError::FolderMissing,
            SyncError::FolderNotPortico,
            SyncError::BundleExists,
            SyncError::FolderUnwritable,
            SyncError::FolderUnsafe { reason: "x" },
            SyncError::FolderTooManyEntries { max: 1 },
            SyncError::FolderTimeout { secs: 1 },
            SyncError::FolderIo {
                op: "read",
                kind: "x".into(),
            },
            SyncError::Locked,
            SyncError::Disabled,
            SyncError::AlreadyEnabled,
            SyncError::JoinExpired,
            SyncError::Busy,
            SyncError::HostsFileUnmanaged,
            SyncError::AliasShadowed { alias: "a".into() },
            SyncError::JumpMissing {
                alias: "a".into(),
                jump: "b".into(),
            },
            SyncError::KeyMissing { alias: "a".into() },
            SyncError::HostInUse {
                alias: "a".into(),
                by: "b".into(),
            },
            SyncError::NotPending { alias: "a".into() },
            SyncError::LocalHostUnsynced { alias: "a".into() },
            SyncError::LocalRecordUnsynced { kind: "host_meta" },
            SyncError::SnippetUnconfirmed { id: "a".into() },
            SyncError::PassphraseChanged,
            SyncError::KeysRotated,
            SyncError::DivergentDots {
                device: "ab".into(),
                count: 1,
            },
            SyncError::DroppedWithoutSuccessor {
                device: "ab".into(),
                count: 1,
            },
            SyncError::Deferred {
                device: "ab".into(),
            },
            SyncError::PendingFile {
                device: "ab".into(),
            },
            SyncError::PassphraseReused,
            SyncError::RotationConflict,
            SyncError::ConflictKeepsForgotten {
                device: "ab".into(),
            },
            SyncError::DeviceForgotten {
                by: "ab".into(),
                at: 0,
            },
            SyncError::ForgetSelf,
            SyncError::DeviceUnknown {
                device: "ab".into(),
            },
            SyncError::RosterFull { max: 1024 },
            SyncError::RevokedWrites {
                device: "ab".into(),
                forgotten: "cd".into(),
            },
            SyncError::RevokedValue {
                kind: "host",
                device: "ab".into(),
            },
            SyncError::PeersOutdated { count: 1 },
            SyncError::KeyEntryCurrent,
            SyncError::KeyEntryInvalid,
            SyncError::LineageUnknown,
            SyncError::RotationPending,
            SyncError::NewerEpochFile {
                device: "ab".into(),
            },
            SyncError::NeedsNewKeys {
                device: "ab".into(),
            },
        ];
        for error in samples {
            let summary = crate::Error::from(error).summary();
            assert!(summary.code.starts_with("sync_"), "{}", summary.code);
            assert!(!summary.message.is_empty());
            assert!(
                summary.hint.is_some()
                    || summary.code == "sync_resolution_invalid"
                    || summary.code == "sync_crypto",
                "{}",
                summary.code
            );
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

    /// B2 (S1c review): identifiers are quoted, so a UI that capitalises a
    /// sentence never changes an alias (`legacy` stays `legacy`).
    #[test]
    fn aliases_are_quoted_in_messages() {
        let message = |error: SyncError| crate::Error::from(error).summary().message;
        assert_eq!(
            message(SyncError::AliasShadowed {
                alias: "legacy".into()
            }),
            "synced host `legacy` would shadow a host name of your ssh config"
        );
        assert_eq!(
            message(SyncError::JumpMissing {
                alias: "web".into(),
                jump: "bastion".into()
            }),
            "synced host `web` jumps through `bastion`, which is not known on this device"
        );
        assert_eq!(
            message(SyncError::SnippetUnconfirmed {
                id: "deploy".into()
            }),
            "snippet `deploy` was changed by sync: review and confirm it before running it"
        );
        let taken = crate::managed_hosts::ManagedHostError::AliasTaken {
            alias: "legacy".into(),
            file: std::path::PathBuf::from("hosts.conf"),
            line: 3,
        };
        assert_eq!(
            taken.to_string(),
            "`legacy` is already defined in hosts.conf:3"
        );
    }
}
