//! What the UIs see of the folder sync (S1b, `docs/SYNC.md` §5.2–5.3):
//! statuses, devices, conflicts, issues and pending confirmations. Plain
//! data, serializable for the GUI's IPC; none of it ever holds a secret,
//! a key or a passphrase. Device names are always paired with their id
//! prefix (F16).

use std::path::PathBuf;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use super::passphrase::NewPassphrase;
use crate::error::ErrorSummary;

/// The sync state, for the status bar ([`crate::EngineEvent::Sync`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SyncStatus {
    /// Not set up on this device.
    Disabled,
    /// Set up, but the data key is neither in memory nor remembered: the
    /// passphrase is needed ([`crate::Engine::sync_unlock`]).
    Locked,
    /// The last pass succeeded.
    Idle {
        /// Unix seconds of the last pass (display).
        last_pass: i64,
        devices: usize,
        conflicts: usize,
        issues: usize,
        /// Received hosts waiting for a confirmation on this device (F1).
        pending_confirmations: usize,
    },
    Syncing,
    /// Some device's data is not available yet.
    Waiting {
        reason: WaitReason,
        /// `"name (3f9c0ae1)"`.
        device: Option<String>,
    },
    FolderUnavailable {
        reason: FolderReason,
    },
    /// Something the user must look at (also listed by
    /// [`crate::Engine::sync_issues`]).
    Alert {
        kind: AlertKind,
        device: Option<String>,
    },
    /// The last pass failed for another reason (corrupt local state…).
    Error {
        summary: ErrorSummary,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WaitReason {
    /// A file is uploading or torn (it fails authentication for now).
    PartialSync,
    /// A cloud placeholder (iCloud, `OneDrive` on demand).
    Placeholder,
    /// A device's data refers to changes not in the folder yet.
    Deferred,
    /// The other Portico process is syncing, or a pass is still running.
    Busy,
    /// An active device still publishes under older keys: it must type
    /// the new passphrase (S2a, §13.3.6). Its file is never merged.
    NeedsNewKeys,
    /// The new keys are written here and wait for the folder (§13.3.4).
    RotationPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FolderReason {
    /// The folder or `portico-sync/` is gone (never recreated).
    Missing,
    /// It did not answer in time (stalled network drive, hydration).
    NotMounted,
    PermissionDenied,
    /// `portico-sync/` exists without `bundle.toml`.
    BundleMissing,
    /// A link, a junction, or a folder inside a private one (F9).
    Unsafe,
    /// More device files than the listing limit (F15).
    TooManyEntries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AlertKind {
    /// An older copy of a sync file was put back.
    Rollback,
    /// One device identity published two different snapshots.
    Fork,
    /// This device's sync state was restored: it re-joined under a new id.
    Restored,
    /// `bundle.toml` was modified without the key.
    BundleTampered,
    /// A newer key epoch (S2 rotation).
    KeysRotatedElsewhere,
    /// The passphrase was changed or reset on another device (to
    /// acknowledge, [`crate::Engine::sync_acknowledge_alerts`]).
    PassphraseChanged,
    /// Another device claims changes this device never made.
    ThirdPartyClaim,
    /// A device's data would exceed the sync limits: not merged.
    MergeRefused,
    /// A device sent two values for one change.
    Divergent,
    /// A device's data removed values without replacing them.
    DroppedValues,
    /// Two devices changed the keys at the same time (S2a, §13.3.7):
    /// [`crate::Engine::sync_resolve_rotation_conflict`].
    RotationConflict,
    /// This device was removed from the sync (§13.4.4).
    DeviceForgotten,
    /// A device relays changes of a removed device (§13.4.2).
    RevokedWrites,
}

/// One device of the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDevice {
    /// 32 hex characters.
    pub id: String,
    pub name: String,
    /// `"name (3f9c0ae1)"`: what the UIs show (F16).
    pub label: String,
    pub this_device: bool,
    /// Highest publish sequence verified (0: none yet).
    pub seq: u64,
    /// What the device wrote as its publish time (display only, never
    /// trusted for ordering).
    pub published_at: Option<i64>,
    /// Local time this device last saw a new snapshot of it.
    pub last_seen: Option<i64>,
    /// No new snapshot for longer than the 180-day horizon.
    pub stale: bool,
    pub revoked: bool,
}

/// A synced host's connection values (no secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncHostValue {
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// `~/…`.
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSnippetValue {
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub run: bool,
}

/// What a conflict or issue is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SyncSubject {
    Host {
        alias: String,
    },
    HostMeta {
        alias: String,
        field: String,
    },
    Snippet {
        id: String,
    },
    Setting {
        key: String,
    },
    /// A record of a kind this version does not know.
    Other {
        kind: String,
    },
}

/// One side of a conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SyncValue {
    Host(SyncHostValue),
    Text(String),
    Bool(bool),
    Tags(Vec<String>),
    Snippet(SyncSnippetValue),
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictSide {
    /// Pass it back in [`SyncResolution::Pick`].
    pub index: usize,
    /// Labels of the devices that wrote this value.
    pub devices: Vec<String>,
    /// This device wrote (or holds) this value.
    pub local: bool,
    /// Display stamp of the most recent write.
    pub at: i64,
    pub value: SyncValue,
}

/// Concurrent edits of one record, never resolved silently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConflict {
    /// Opaque, stable while the conflict lasts.
    pub id: String,
    pub subject: SyncSubject,
    pub sides: Vec<ConflictSide>,
    /// Identifies the sides shown: pass it back to
    /// [`crate::Engine::sync_resolve`]. Sides changed since (another
    /// device wrote again and a pass merged it) are refused with
    /// `sync_not_pending`: the UI reloads and shows the new ones.
    pub sides_hash: String,
}

/// A user decision on a conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SyncResolution {
    Pick {
        index: usize,
    },
    /// Tags only: the union of every side (preselected by the UIs).
    Union,
    /// Snippets only: the side at `keep` stays under its id, every other
    /// side becomes a new snippet (`<id>-2`…).
    KeepBoth {
        keep: usize,
    },
}

/// Something the user may want to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncIssue {
    /// Pass it to [`crate::Engine::sync_skip_on_this_device`] when
    /// `skippable`.
    pub key: String,
    pub subject: Option<SyncSubject>,
    /// Code, message and hint (`sync_alias_shadowed`, …).
    pub summary: ErrorSummary,
    /// The issue only warns (the record was applied).
    pub warning: bool,
    pub skippable: bool,
}

/// A received host change that waits for this device's confirmation (F1):
/// nothing is written to `hosts.conf` before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingHostChange {
    pub alias: String,
    /// The host is new on this device.
    pub new: bool,
    /// What `hosts.conf` holds now.
    pub current: Option<SyncHostValue>,
    pub incoming: SyncHostValue,
    pub renamed_from: Option<String>,
    /// Labels of the devices that wrote it.
    pub devices: Vec<String>,
    /// Identifies the `incoming` destination shown to the user: pass it
    /// back to [`crate::Engine::sync_confirm_hosts`] /
    /// [`crate::Engine::sync_reject_hosts`]. A destination changed since
    /// (another device published again) is refused with
    /// `sync_not_pending`: the UI reloads and shows the new one.
    pub destination_hash: String,
}

/// A snippet changed by sync that this device has not confirmed yet (F8):
/// the UI shows the whole command before offering to confirm it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnconfirmedSnippet {
    pub id: String,
    /// What the user confirms, exactly as it will run.
    pub snippet: SyncSnippetValue,
    /// Labels of the devices that wrote it (as in
    /// [`PendingHostChange::devices`]). Known once a pass has read the
    /// folder since launch; empty while sync is locked or off (the gate
    /// stays, the UI says "another device").
    pub devices: Vec<String>,
    /// Identifies the content shown: pass it back to
    /// [`crate::Engine::confirm_synced_snippet`]. Content changed since is
    /// refused with `sync_not_pending`.
    pub hash: String,
}

/// What a folder holds, before create or join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FolderProbe {
    /// No Portico sync yet: "create" puts `portico-sync/` there.
    Usable,
    /// A Portico sync: "join" it.
    ExistingBundle {
        devices: usize,
    },
    /// `portico-sync/` without `bundle.toml` (not downloaded yet?).
    Incomplete,
    Missing,
    Refused {
        summary: ErrorSummary,
    },
}

/// "Create a sync folder" (first device).
#[derive(Debug)]
pub struct SyncCreate {
    pub folder: PathBuf,
    pub passphrase: NewPassphrase,
    pub device_name: String,
    /// Remember the data key in the system keychain (default `true`).
    pub remember_key: bool,
}

/// "Join a sync folder".
#[derive(Debug)]
pub struct SyncJoin {
    pub folder: PathBuf,
    pub passphrase: SecretString,
    pub device_name: String,
    pub remember_key: bool,
}

/// What joining would bring, computed without writing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPreview {
    /// Single use, expires after a few minutes (F14).
    pub token: String,
    pub devices: Vec<SyncDevice>,
    pub hosts: usize,
    pub host_meta: usize,
    pub snippets: usize,
    pub settings: usize,
    /// Records already on this device with another value.
    pub conflicts: usize,
    /// Records only a newer Portico reads.
    pub needs_newer: usize,
    /// This device verified this bundle before (it joins again, S2a): only
    /// [`JoinBulkChoice::ReviewEach`] is accepted, and every local record
    /// that differs from the synced one waits for a decision
    /// ([`crate::Engine::sync_revoked_values`], RR4).
    pub rejoin: bool,
}

/// How to settle the conflicts found when joining. Neither bulk choice
/// confirms a received host destination (F1): those stay pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinBulkChoice {
    ReviewEach,
    PreferRemote,
    PreferLocal,
}

/// "Disable sync on this device". Local hosts, snippets and settings are
/// always kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisableOptions {
    /// Forget the data key in the keychain (default `true`, F14), and
    /// every older sync key of this bundle indexed here (S2a).
    pub forget_key: bool,
    /// Remove this device's file from the folder.
    pub remove_my_file: bool,
    /// Leave the device list (S2a, §13.4.5): this device moves to the
    /// roster's forgotten list with its own clock, without a key rotation
    /// (an S2 operation: the bundle becomes version 2). Best effort: sync
    /// is turned off even when the bundle cannot be opened. It does **not**
    /// exclude this device: only forgetting it from another device does.
    #[serde(default)]
    pub leave_roster: bool,
}

impl Default for DisableOptions {
    fn default() -> Self {
        Self {
            forget_key: true,
            remove_my_file: false,
            leave_roster: false,
        }
    }
}

// ── S2a: key rotation and forgetting a device (`docs/SYNC.md` §13.7) ────

/// How the user authorizes a key rotation (§13.3.2).
#[derive(Debug)]
#[non_exhaustive]
pub enum RotateAuth {
    /// The current passphrase (it must open the current key slot).
    CurrentPassphrase(SecretString),
    /// "I no longer know it", behind the reset-level confirmation of the
    /// UI (owner decision Q6): this device's data key authorizes.
    ResetConfirmed,
}

/// "Rotate keys" (§13.3): a new epoch, a new data key, a mandatory new
/// passphrase every other device must type.
#[derive(Debug)]
pub struct RotateKeys {
    pub authorization: RotateAuth,
    pub new_passphrase: NewPassphrase,
    /// Remember the new data key in the system keychain.
    pub remember_key: bool,
}

/// "Forget a device" (§13.4): always a rotation, plus the devices moved to
/// the roster's forgotten list.
#[derive(Debug)]
pub struct ForgetDevice {
    /// Ids (32 hex characters) of the devices to forget; never this one.
    pub device_ids: Vec<String>,
    pub authorization: RotateAuth,
    pub new_passphrase: NewPassphrase,
    pub remember_key: bool,
}

/// What a rotation (or a forget) did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationOutcome {
    /// The new epoch.
    pub epoch: u32,
    /// The status after the rotation (`Waiting { RotationPending }` while
    /// the folder does not answer: it completes later, forward only).
    pub status: SyncStatus,
    /// Labels of the devices that must type the new passphrase.
    pub enter_new_passphrase_on: Vec<String>,
    /// Labels of the devices forgotten by this rotation.
    pub forgotten: Vec<String>,
    /// Server accounts whose password was shared and should be changed
    /// (F2). Always empty before password sync (S2b).
    pub exposed_accounts: Vec<String>,
}

/// One device in a rotation preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationPeer {
    pub id: String,
    /// `"name (3f9c0ae1)"`.
    pub label: String,
    pub this_device: bool,
    /// What its last verified snapshot announced.
    pub app_version: Option<String>,
    /// Older than Portico 0.7: it stops syncing at the first S2 operation
    /// (warning `sync_peers_outdated`, Q5).
    pub outdated: bool,
    /// Some of its data is still on its way (withheld or uploading): what
    /// it alone holds may be lost if it never adopts the new keys.
    pub waiting: bool,
}

/// What a rotation would affect (shown before it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationPreview {
    pub devices: Vec<RotationPeer>,
    /// Labels of the outdated devices.
    pub outdated: Vec<String>,
    /// The bundle switches to version 2 with this operation.
    pub format_upgrade: bool,
    /// Always empty before password sync (S2b).
    pub exposed_accounts: Vec<String>,
}

/// What forgetting a device would affect (§13.4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetPreview {
    pub id: String,
    pub label: String,
    /// Local time its last new snapshot was seen.
    pub last_seen: Option<i64>,
    /// Records whose surviving value it wrote (they stay).
    pub last_written: Vec<SyncSubject>,
    /// Always empty before password sync (S2b).
    pub exposed_accounts: Vec<String>,
    /// Devices that must type the new passphrase afterwards.
    pub enter_new_passphrase_on: Vec<String>,
}

/// The user's answer to a rotation conflict (§13.3.7).
#[derive(Debug)]
#[non_exhaustive]
pub enum RotationChoice {
    /// Type the other device's passphrase: its keys are checked like an
    /// adoption (lineage from the last common epoch, passphrase history,
    /// and none of this device's forgotten devices may be active there).
    UseOther(SecretString),
    /// Rotate again from this device's keys (epoch + 1). Devices the other
    /// rotation removed are not removed by this one: remove them again.
    KeepMine {
        new_passphrase: NewPassphrase,
        remember_key: bool,
    },
}

/// A device file that announces newer keys than this device's (§13.3.5,
/// RR5): [`crate::Engine::sync_discard_newer_file`] removes it when nobody
/// changed the keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewerEpochFile {
    /// Device id (32 hex characters).
    pub device: String,
    /// `"name (3f9c0ae1)"` when the roster knows it.
    pub label: String,
    /// `devices/<id>.snap` (or a provider copy of it).
    pub file: String,
    /// The epoch its clear header announces (not authenticated).
    pub epoch: u32,
    /// Identifies the file shown: pass it back to discard it.
    pub fingerprint: String,
}

/// An old sync key in the keychain (§13.3.8): never the key in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncKeyEntry {
    /// `sync-key:<bundle>:<epoch>`: pass it back to remove it.
    pub account: String,
    /// The bundle id (hex).
    pub bundle: String,
    pub epoch: u32,
    /// It belongs to the bundle this device syncs with.
    pub this_bundle: bool,
}

/// A local value written by a forgotten device (or of unknown origin after
/// a re-join), held out of sync until the user decides (§13.4.2, R3, RR4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedValueItem {
    /// Pass it back to [`crate::Engine::sync_revoked_value`].
    pub key: String,
    pub subject: SyncSubject,
    /// The forgotten device's label, or `None` for "unknown origin".
    pub device: Option<String>,
    /// The value held on this device (what [`RevokedValueChoice::KeepAsMine`]
    /// shares). `None`: not shown (a secret).
    pub local: Option<SyncValue>,
    /// The value the other devices hold (what
    /// [`RevokedValueChoice::TakeSynced`] takes). `None`: they hold none,
    /// so taking it removes the record here (or a secret, never shown).
    pub synced: Option<SyncValue>,
    /// Identifies the content shown (both values and the device): pass it
    /// back so a value that changed since is refused (`sync_not_pending`).
    pub hash: String,
}

/// The decision on a [`RevokedValueItem`]: nothing is preselected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevokedValueChoice {
    /// Publish this device's value as its own write.
    KeepAsMine,
    /// Take the value the other devices hold (or delete it here when they
    /// hold none).
    TakeSynced,
}

/// Options of this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOptions {
    pub remember_key: bool,
}

/// What `sync_status` adds to the status: the folder and this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOverview {
    pub status: SyncStatus,
    pub folder: Option<PathBuf>,
    pub device: Option<String>,
    pub remember_key: bool,
}
