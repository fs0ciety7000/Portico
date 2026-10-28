//! The sync engine proper (S1b, `docs/SYNC.md` §5): one pass, and the
//! user operations (create, join, unlock, resolve, confirm, disable…).
//!
//! [`SyncCore`] lives on the dedicated sync thread (`engine::sync_api`):
//! every function here is blocking (folder, local files, keychain), and
//! the Argon2 derivations are done by the caller on `spawn_blocking`.
//!
//! ## One pass (crash-safe order, §5.1)
//!
//! 1. `sync.lock` (another Portico process syncing: skipped, §4.9); read
//!    `sync.toml`; the data key from memory or the keychain (else
//!    `Locked`).
//! 2. Safe root (F9): the chosen folder still canonicalizes to the root
//!    recorded at set-up; `portico-sync/` and `devices/` are real
//!    directories. Listing with its limits (F15).
//! 3. `bundle.toml` and its provider copies opened with the data key (MAC
//!    first, F14); the highest `(epoch, keyset generation, revision)` wins,
//!    ties go to the smallest fingerprint; the bundle high-water mark
//!    refuses an older one (F4); a [`BundleObservation::Diverged`] bundle
//!    (same triple, other content) is rewritten with the union of the
//!    rosters (revision + 1) so every device converges on one file —
//!    only once step 4 checked this device's own file.
//! 4. The replica (sealed, F5); **this device's own file** is opened and
//!    checked ([`guard::check_own_file`], `LocalClock::observe_own_*`)
//!    before any write: ahead of the replica (or forked) means a restored
//!    or cloned data dir → re-join under a new id. Only our own files can
//!    say so; another device's claim about our dots is an alert
//!    (`sync_third_party_claim`), never a re-join.
//! 5. Every other device's best candidate is opened and observed (rollback
//!    and fork refused, marks updated) **before** any context is judged:
//!    a snapshot whose context claims dots not backed by the claimed
//!    devices' own snapshots is deferred ([`guard::check_context_claims`]).
//! 6. Local projection → local writes (new dots), then the merge of every
//!    accepted snapshot (`MergeRefused` names the device; divergent dots
//!    and values dropped without successor are alerts), then tombstone
//!    collection on the stable context (meet of the contexts of every
//!    non-revoked, non-stale device of the roster; none when one is
//!    unknown).
//! 7. Replica persisted (**a dot is durable before it is published**),
//!    validated application (`apply`), last applied projection persisted,
//!    then the snapshot is published when the state changed (or our file
//!    is behind).
//!
//! ## S2a additions (`docs/SYNC.md` §13)
//!
//! - **Epoch floor first** (R2): a transition in progress (`rotation.toml`)
//!   is carried forward before anything else ([`rotation`]); then the
//!   clear header of `bundle.toml` is judged against the floor before any
//!   key is used: a newer epoch is checked against this device's lineage
//!   (`KeysRotatedElsewhere`, `RotationConflict` or tampering), an older
//!   one is a rollback (the folder is rewritten from `bundle.verified`
//!   only when strictly lower, RR2).
//! - **Snapshot headers** of every active device are read before any
//!   merge: a newer epoch stops the pass (RR5: `sync_discard_newer_file`),
//!   an older one waits (`NeedsNewKeys`) and is never merged.
//! - **Forgotten devices** (roster v2): their files are ignored and
//!   removed, their writes are bounded by their exact `revoked_clock`
//!   (`RevokedWrites`), their entries are immutable once observed, and the
//!   roster union is field by field (§13.4.6).
//! - **Password policy**: while it is off, `secret` registers are dropped
//!   on every side of the merge; while it is on they are relayed opaque.
//! - **Retained lineage** (E1): `C_E` is computed at every open and kept in
//!   `commitment.toml` and the replica, with `bundle.verified` and the
//!   history of slots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::broadcast;
use zeroize::Zeroizing;

use super::SyncError;
use super::api::{
    AlertKind, ConflictSide, FolderProbe, FolderReason, JoinBulkChoice, JoinPreview,
    PendingHostChange, SyncConflict, SyncDevice, SyncHostValue, SyncIssue, SyncResolution,
    SyncSnippetValue, SyncStatus, SyncSubject, SyncValue, WaitReason,
};
use super::apply::{self, ApplyInput, Issue, destination_hash, issue_key};
use super::bundle::ClearHeader;
use super::bundle::{self, OpenedBundle, Roster, RosterDevice};
use super::clock::{DeviceId, DeviceName, LocalClock, VersionVector};
use super::container::{
    self, Binding, Expected, MAX_CONTAINER_FILE_BYTES, Opened, ReplicaContent, SnapshotContent,
};
use super::crypto::{self, DataKey, KdfBounds, KdfParams};
use super::folder::{self, FolderFile, FolderIo, FolderOpener, Listing, PASS_BYTE_BUDGET};
use super::guard::{self, BundleObservation, ClaimCheck, OwnFile, STALE_HORIZON_SECS};
use super::model::{
    HostValue, MetaField, RecordKey, RecordValue, RegisterView, Resolution, SyncState,
    TypedSibling, WireKey, WireValue,
};
use super::project::{self, LocalView};
use super::state::{self, LocalReplica, SyncConfig};
use super::{api, lineage};
use crate::engine::EngineConfig;
use crate::events::EngineEvent;
use crate::secrets::SecretKey;
use crate::snippets::SnippetStore;

/// Knobs of the sync (production values by default; the tests shorten
/// them and swap the folder for an in-memory provider).
#[derive(Debug, Clone)]
pub(crate) struct SyncTuning {
    pub(crate) kdf_bounds: KdfBounds,
    pub(crate) kdf_params: KdfParams,
    pub(crate) opener: Arc<dyn FolderOpener>,
    /// Polling interval (§5.4: 30 s).
    pub(crate) poll: Duration,
    /// Pass after the last local change (§5.4: 5 s).
    pub(crate) debounce: Duration,
    /// Longest wait for one folder operation (§5.4: 60 s).
    pub(crate) op_timeout: Duration,
    /// Lifetime of a join token (F14).
    pub(crate) join_ttl: Duration,
    /// [`crate::Engine::start_sync`] starts the polling task (the tests
    /// drive their passes themselves).
    pub(crate) background: bool,
    /// Tests: stop a key change there, as a crash would.
    #[cfg(test)]
    pub(crate) crash_at: Option<CrashPoint>,
}

impl Default for SyncTuning {
    fn default() -> Self {
        Self {
            kdf_bounds: KdfBounds::PRODUCTION,
            kdf_params: KdfParams::DEFAULT,
            opener: Arc::new(folder::StdOpener),
            poll: Duration::from_secs(30),
            debounce: Duration::from_secs(5),
            op_timeout: Duration::from_secs(60),
            join_ttl: Duration::from_secs(600),
            background: true,
            #[cfg(test)]
            crash_at: None,
        }
    }
}

/// A join being reviewed: nothing written yet (F14). Single use.
#[derive(Debug)]
struct PendingJoin {
    folder: PathBuf,
    root: PathBuf,
    bundle_id: [u8; crypto::ID_LEN],
    epoch: u32,
    keyset_generation: u64,
    data_key: DataKey,
    device_name: DeviceName,
    remember_key: bool,
    expires: Instant,
    /// This device verified this bundle before: no bulk choice, local
    /// records of unknown origin (FAIBLE-5 of the S2a review).
    rejoin: bool,
}

/// An alert raised by a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Alert {
    kind: AlertKind,
    device: Option<String>,
    error: SyncError,
}

/// What the last pass found (served to the UIs without a new pass).
#[derive(Debug, Clone, Default)]
struct Report {
    issues: Vec<Issue>,
    alerts: Vec<Alert>,
    pending: Vec<PendingHostChange>,
    conflicts: Vec<SyncConflict>,
    devices: Vec<SyncDevice>,
    waiting: Option<(WaitReason, Option<String>)>,
    /// Values held out of sync (R3, RR4).
    held: Vec<api::RevokedValueItem>,
}

/// Everything a pass works on, loaded under `sync.lock`.
struct Session {
    sync: SyncConfig,
    folder: Arc<dyn FolderIo>,
    bundle: OpenedBundle,
    replica: ReplicaContent,
    replica_gen: u64,
    clock: LocalClock,
    listing: Listing,
    /// Bytes left to read in this pass (F15).
    budget: u64,
    /// `bundle.toml` to rewrite (a provider copy won, rosters differ, or a
    /// diverged bundle): done by the pass **after** this device's own file
    /// was checked (§5.1: nothing is written before).
    roster_rewrite: Option<Roster>,
}

mod rotation;

#[cfg(test)]
pub(crate) use rotation::CrashPoint;
pub(crate) use rotation::{
    Authorization, RotationPlan, build_rotation, conflict_decide, unlock_decide,
};

/// Data keys in memory, indexed by `(bundle, epoch)` (S2a, R2: never the
/// key of another epoch for the one `sync.toml` names) and dropped when
/// `sync.toml`, `rotation.toml` or the retained commitment changed under
/// this process (the other Portico process adopted, rotated, or chose
/// another lineage).
#[derive(Debug, Default)]
struct KeyCache {
    stamp: Option<String>,
    entries: Vec<(String, u32, DataKey)>,
}

/// Most keys kept (the current epoch and the next one of a transition).
const KEY_CACHE_ENTRIES: usize = 4;

impl KeyCache {
    fn get(&self, bundle: &str, epoch: u32) -> Option<DataKey> {
        self.entries
            .iter()
            .find(|(b, e, _)| b == bundle && *e == epoch)
            .and_then(|(_, _, key)| DataKey::from_bytes(key.bytes()).ok())
    }

    fn put(&mut self, bundle: &str, epoch: u32, key: &DataKey) {
        let Ok(copy) = DataKey::from_bytes(key.bytes()) else {
            return;
        };
        self.entries
            .retain(|(b, e, _)| !(b == bundle && *e == epoch));
        self.entries.push((bundle.to_owned(), epoch, copy));
        let excess = self.entries.len().saturating_sub(KEY_CACHE_ENTRIES);
        self.entries.drain(..excess);
    }

    /// Drops the keys of `bundle` below `epoch` (step 8: `DK_N` leaves
    /// memory once the replica is migrated).
    fn forget_below(&mut self, bundle: &str, epoch: u32) {
        self.entries
            .retain(|(b, e, _)| !(b == bundle && *e < epoch));
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.stamp = None;
    }
}

/// A device file announcing a newer epoch (RR5).
#[derive(Debug, Clone)]
struct NewerFile {
    device: DeviceId,
    file: FolderFile,
    epoch: u32,
    fingerprint: String,
    label: String,
}

/// What [`SyncCore::open_bundle`] found.
struct OpenedSet {
    bundle: OpenedBundle,
    union: Roster,
    clock_conflicts: Vec<DeviceId>,
    main_won: bool,
    rewrite: bool,
}

/// The sync state machine (one per engine, on the sync thread).
#[derive(Debug)]
pub(crate) struct SyncCore {
    config: EngineConfig,
    snippets: Arc<SnippetStore>,
    events: broadcast::Sender<EngineEvent>,
    tuning: SyncTuning,
    keys: KeyCache,
    /// Files that announced a newer epoch at the last pass (RR5).
    newer_files: Vec<NewerFile>,
    /// Alerts raised outside a pass (a re-join during an adoption), shown
    /// by the next report.
    carry_alerts: Vec<Alert>,
    /// `sync-key:` entries written before the index were probed for this
    /// `(bundle, epoch)` (§13.3.8).
    probed_keys: Option<(String, u32)>,
    /// As of the last pass: each device's announced version (Q5), the
    /// withheld devices, the bundle's format version.
    peer_versions: BTreeMap<DeviceId, String>,
    withheld: BTreeSet<DeviceId>,
    bundle_version: u32,
    joins: BTreeMap<String, PendingJoin>,
    report: Report,
    status: Arc<Mutex<SyncStatus>>,
    writers: SnippetWriters,
    conflicts_count: usize,
    /// What awaits a confirmation (hosts: alias → destination hash;
    /// snippets: id → content hash) as of the last pass: a change of its
    /// **content**, not only of its size, is announced.
    pending_seen: Option<PendingSet>,
}

/// Snippet id → labels of the devices that wrote the gated content, as of
/// the last pass (shared with the engine: the register needs the data key,
/// the list of unconfirmed snippets does not).
pub(crate) type SnippetWriters = Arc<Mutex<BTreeMap<String, Vec<String>>>>;

/// Received changes awaiting a confirmation on this device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct PendingSet {
    hosts: BTreeMap<String, String>,
    snippets: BTreeMap<String, String>,
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if text.len() != 64 {
        return None;
    }
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn sync_err(error: crate::Error) -> SyncError {
    match error {
        crate::Error::Sync(e) => e,
        other => SyncError::FolderIo {
            op: "local state",
            kind: other.summary().code,
        },
    }
}

/// The status a failed pass leaves.
pub(crate) fn status_for(error: &crate::Error) -> SyncStatus {
    let folder = |reason| SyncStatus::FolderUnavailable { reason };
    match error {
        crate::Error::Sync(e) => match e {
            SyncError::Disabled => SyncStatus::Disabled,
            SyncError::Locked | SyncError::WrongPassphrase => SyncStatus::Locked,
            SyncError::FolderMissing => folder(FolderReason::Missing),
            SyncError::FolderNotPortico => folder(FolderReason::BundleMissing),
            SyncError::FolderUnsafe { .. } => folder(FolderReason::Unsafe),
            SyncError::FolderTooManyEntries { .. } => folder(FolderReason::TooManyEntries),
            SyncError::FolderTimeout { .. } => folder(FolderReason::NotMounted),
            SyncError::FolderIo { kind, .. } if kind.contains("ermission") => {
                folder(FolderReason::PermissionDenied)
            }
            SyncError::BundleTampered => SyncStatus::Alert {
                kind: AlertKind::BundleTampered,
                device: None,
            },
            SyncError::Rollback { .. } | SyncError::FormatDowngrade { .. } => SyncStatus::Alert {
                kind: AlertKind::Rollback,
                device: None,
            },
            SyncError::KeysRotated => SyncStatus::Alert {
                kind: AlertKind::KeysRotatedElsewhere,
                device: None,
            },
            SyncError::NewerEpochFile { device } => SyncStatus::Alert {
                kind: AlertKind::KeysRotatedElsewhere,
                device: Some(device.clone()),
            },
            SyncError::RotationConflict | SyncError::ConflictKeepsForgotten { .. } => {
                SyncStatus::Alert {
                    kind: AlertKind::RotationConflict,
                    device: None,
                }
            }
            SyncError::DeviceForgotten { by, .. } => SyncStatus::Alert {
                kind: AlertKind::DeviceForgotten,
                device: Some(by.clone()),
            },
            SyncError::RotationPending => SyncStatus::Waiting {
                reason: WaitReason::RotationPending,
                device: None,
            },
            SyncError::Busy => SyncStatus::Waiting {
                reason: WaitReason::Busy,
                device: None,
            },
            _ => SyncStatus::Error {
                summary: error.summary(),
            },
        },
        _ => SyncStatus::Error {
            summary: error.summary(),
        },
    }
}

fn data_key_account(sync: &SyncConfig) -> SecretKey {
    key_account(&sync.bundle_id, sync.epoch)
}

/// `sync-key:<bundle>:<epoch>`.
fn key_account(bundle: &str, epoch: u32) -> SecretKey {
    SecretKey::SyncDataKey {
        bundle: bundle.to_owned(),
        epoch,
    }
}

fn fresh_replica(name: DeviceName, keyset_generation: u64) -> ReplicaContent {
    ReplicaContent {
        device_name: name,
        clock: 0,
        publish_seq: 0,
        state: SyncState::default(),
        high_water: guard::HighWater::default(),
        local: LocalReplica {
            known_keyset_generation: keyset_generation,
            ..LocalReplica::default()
        },
    }
}

fn host_dto(value: &HostValue) -> SyncHostValue {
    SyncHostValue {
        hostname: value.hostname.clone(),
        user: value.user.clone(),
        port: value.port,
        identity_file: value.identity_file.as_ref().map(|p| p.as_str().to_owned()),
        proxy_jump: value.proxy_jump.clone(),
    }
}

fn subject(key: &RecordKey) -> SyncSubject {
    match key {
        RecordKey::Host { alias } => SyncSubject::Host {
            alias: alias.clone(),
        },
        RecordKey::HostMeta { alias, field } => SyncSubject::HostMeta {
            alias: alias.clone(),
            field: match field {
                MetaField::Tags => "tags",
                MetaField::Group => "group",
                MetaField::Favorite => "favorite",
                MetaField::Notes => "notes",
            }
            .to_owned(),
        },
        RecordKey::Snippet { id } => SyncSubject::Snippet { id: id.clone() },
        RecordKey::Setting { key } => SyncSubject::Setting { key: key.path() },
        RecordKey::Secret { .. } => SyncSubject::Other {
            kind: "secret".to_owned(),
        },
    }
}

fn value_dto(value: &RecordValue) -> SyncValue {
    match value {
        RecordValue::Tombstone | RecordValue::Sealed(_) => SyncValue::Deleted,
        RecordValue::Host(host) => SyncValue::Host(host_dto(host)),
        RecordValue::Tags(tags) => SyncValue::Tags(tags.iter().cloned().collect()),
        RecordValue::Text(text) => SyncValue::Text(text.clone()),
        RecordValue::Bool(on) => SyncValue::Bool(*on),
        RecordValue::Snippet(snippet) => SyncValue::Snippet(SyncSnippetValue {
            name: snippet.name.clone(),
            command: snippet.command.clone(),
            description: snippet.description.clone(),
            run: snippet.run,
        }),
    }
}

/// The id of a conflict: the canonical wire key, hex-encoded (opaque).
fn conflict_id(key: &RecordKey) -> String {
    WireKey::encode(key)
        .map(|wire| state::hex(wire.as_str().as_bytes()))
        .unwrap_or_default()
}

fn key_of_conflict(id: &str) -> Option<RecordKey> {
    let bytes: Vec<u8> = (0..id.len())
        .step_by(2)
        .map(|i| {
            id.get(i..i.saturating_add(2))
                .and_then(|p| u8::from_str_radix(p, 16).ok())
        })
        .collect::<Option<Vec<u8>>>()?;
    let text = String::from_utf8(bytes).ok()?;
    let key: RecordKey = serde_json::from_str(&text).ok()?;
    (conflict_id(&key) == id).then_some(key)
}

impl SyncCore {
    pub(crate) fn new(
        config: EngineConfig,
        snippets: Arc<SnippetStore>,
        events: broadcast::Sender<EngineEvent>,
        tuning: SyncTuning,
        status: Arc<Mutex<SyncStatus>>,
        writers: SnippetWriters,
    ) -> Self {
        Self {
            config,
            snippets,
            events,
            tuning,
            keys: KeyCache::default(),
            newer_files: Vec::new(),
            carry_alerts: Vec::new(),
            probed_keys: None,
            peer_versions: BTreeMap::new(),
            withheld: BTreeSet::new(),
            bundle_version: bundle::BUNDLE_V1,
            joins: BTreeMap::new(),
            report: Report::default(),
            status,
            writers,
            conflicts_count: 0,
            pending_seen: None,
        }
    }

    /// Drops the join tokens past their lifetime, with their data key
    /// (zeroized on drop). Run before every operation of the sync thread.
    pub(crate) fn purge_expired_joins(&mut self) {
        let now = Instant::now();
        self.joins.retain(|_, join| join.expires > now);
    }

    fn emit(&self, event: EngineEvent) {
        let _ = self.events.send(event);
    }

    fn set_status(&mut self, status: SyncStatus) {
        let changed = {
            let mut current = self.status.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = *current != status;
            current.clone_from(&status);
            changed
        };
        if changed {
            self.emit(EngineEvent::Sync { status });
        }
    }

    pub(crate) fn status(&self) -> SyncStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn label(roster: &Roster, device: &DeviceId) -> String {
        roster
            .devices
            .iter()
            .find(|d| d.id == *device)
            .map_or_else(|| device.short(), |d| d.name.label(device))
    }

    // ── Loading ─────────────────────────────────────────────────────────

    /// Drops the cached keys when `sync.toml`, `rotation.toml` or the
    /// retained commitment changed since the last check (R2: the other
    /// Portico process adopted, rotated or switched lineage).
    fn stamp_keys(&mut self, sync: &SyncConfig) {
        let paths = &self.config.paths;
        let next = state::load_rotation(paths)
            .ok()
            .flatten()
            .map_or(0, |r| r.next_epoch);
        let commit = state::load_commitment(paths)
            .ok()
            .flatten()
            .map(|c| c.commit)
            .unwrap_or_default();
        let stamp = format!(
            "{}:{}:{}:{}:{commit}",
            sync.bundle_id, sync.epoch, sync.device, next
        );
        if self.keys.stamp.as_deref() != Some(stamp.as_str()) {
            if self.keys.stamp.is_some() {
                self.keys.entries.clear();
            }
            self.keys.stamp = Some(stamp);
        }
    }

    /// [`Self::restamp_keys`] from the `sync.toml` on disk (after a pass,
    /// under `sync.lock`).
    fn restamp_from_disk(&mut self) {
        if let Ok(Some(sync)) = state::load_config(&self.config.paths) {
            self.restamp_keys(&sync);
        }
    }

    /// Re-stamps after this process changed its own files (the keys it
    /// holds stay valid).
    fn restamp_keys(&mut self, sync: &SyncConfig) {
        let entries = std::mem::take(&mut self.keys.entries);
        self.keys.stamp = None;
        self.stamp_keys(sync);
        self.keys.entries = entries;
    }

    /// The data key of the current epoch (`sync.toml`): in memory, else
    /// the keychain (when remembered).
    fn data_key_for(&mut self, sync: &SyncConfig) -> Result<DataKey, SyncError> {
        self.stamp_keys(sync);
        self.epoch_key(sync, sync.epoch).ok_or(SyncError::Locked)
    }

    /// The data key of `(bundle, epoch)`: cached, else the keychain
    /// (cached then). The keys of the current epoch and of a transition
    /// are read there only while this device remembers its key ("don't
    /// remember" is honoured during a transition too, FAIBLE-1 of the S2a
    /// review); an older epoch's entry (the replica not migrated yet after
    /// a crash) is read whatever the choice: it exists only if it was
    /// remembered then, and it is collected once migrated.
    fn epoch_key(&mut self, sync: &SyncConfig, epoch: u32) -> Option<DataKey> {
        if let Some(key) = self.keys.get(&sync.bundle_id, epoch) {
            return Some(key);
        }
        if epoch >= sync.epoch && !sync.remember_key {
            return None;
        }
        let stored = self
            .config
            .secrets
            .get(&key_account(&sync.bundle_id, epoch))
            .ok()
            .flatten()?;
        let bytes = Zeroizing::new(B64.decode(stored.expose_secret()).ok()?);
        let key = DataKey::from_bytes(&bytes).ok()?;
        self.keys.put(&sync.bundle_id, epoch, &key);
        Some(key)
    }

    /// Stores `key` as `sync-key:<bundle>:<epoch>` (indexed first, §13.3.8).
    fn remember_key_at(&self, bundle: &str, epoch: u32, key: &DataKey) -> crate::Result<()> {
        let encoded = Zeroizing::new(B64.encode(key.bytes()));
        let secret = SecretString::from(encoded.as_str().to_owned());
        self.config
            .secrets
            .set(&key_account(bundle, epoch), &secret)
    }

    fn remember_key(&self, sync: &SyncConfig, key: &DataKey) -> crate::Result<()> {
        self.remember_key_at(&sync.bundle_id, sync.epoch, key)
    }

    fn open_folder(&self, sync: &SyncConfig) -> Result<Arc<dyn FolderIo>, SyncError> {
        // F9: the chosen folder must still lead to the root recorded at
        // set-up (a link swapped in since is refused).
        let root = folder::check_root(&sync.folder, &self.config.paths)?;
        if root != sync.root {
            return Err(SyncError::FolderUnsafe {
                reason: "the folder now leads elsewhere than when sync was set up",
            });
        }
        let io = self.tuning.opener.open(&sync.root);
        io.check()?;
        Ok(io)
    }

    /// `bundle.toml` of the folder, as text.
    fn read_bundle(folder: &dyn FolderIo) -> Result<String, SyncError> {
        let bytes = folder
            .read(&FolderFile::Bundle, bundle::MAX_BUNDLE_BYTES)?
            .ok_or(SyncError::FolderNotPortico)?;
        String::from_utf8(bytes).map_err(|_| SyncError::Corrupt {
            what: "bundle.toml",
            reason: "not UTF-8",
        })
    }

    /// Opens `bundle.toml` (and its provider copies of the same epoch)
    /// with the data key; returns the winner, the field-by-field union of
    /// the rosters (§13.4.6) with its clock conflicts, whether the main
    /// file won, and whether `bundle.toml` itself must be rewritten (a copy
    /// won, or the union differs from the winner's roster).
    fn open_bundle(
        folder: &dyn FolderIo,
        listing: &Listing,
        main_text: &str,
        key: &DataKey,
        sync: &SyncConfig,
    ) -> Result<OpenedSet, SyncError> {
        let max = bundle::MAX_BUNDLE_BYTES;
        let main = bundle::open_with_key(main_text, key)?;
        let rank = |b: &OpenedBundle| (b.header.rank(), std::cmp::Reverse(b.fingerprint));
        let mut union = main.roster.clone();
        let mut clock_conflicts: Vec<DeviceId> = Vec::new();
        let mut best = main;
        let mut copy_won = false;
        for copy in &listing.bundle_copies {
            let Ok(Some(bytes)) = folder.read(copy, max) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(opened) = bundle::open_with_key(text, key) else {
                continue;
            };
            if opened.header.bundle_id != best.header.bundle_id
                || opened.header.epoch != best.header.epoch
                || opened.commit != best.commit
            {
                continue;
            }
            let (merged, report) = bundle::union_rosters(&union, &opened.roster);
            union = merged;
            clock_conflicts.extend(report.clock_conflicts);
            if rank(&opened) > rank(&best) {
                best = opened;
                copy_won = true;
            }
        }
        if best.header.bundle_id != sync.bundle_bytes()? {
            return Err(SyncError::BundleTampered);
        }
        if best.header.epoch > sync.epoch {
            return Err(SyncError::KeysRotated);
        }
        let rewrite = copy_won || union != best.roster;
        Ok(OpenedSet {
            bundle: best,
            union,
            clock_conflicts,
            main_won: !copy_won,
            rewrite,
        })
    }

    fn expected(bundle: &OpenedBundle, device: DeviceId) -> Expected {
        Expected {
            bundle_id: bundle.header.bundle_id,
            epoch: bundle.header.epoch,
            max_keyset_generation: bundle.header.keyset_generation,
            device,
        }
    }

    /// What a locked device can say about a newer `bundle.toml` (§13.3.2):
    /// its link at this device's epoch carries the retained `C_E` (E1) →
    /// "keys changed elsewhere" (the proof is checked after the new
    /// passphrase opens it); else plain "locked".
    fn locked_verdict(&self, sync: &SyncConfig, clear: &ClearHeader) -> SyncError {
        if clear.epoch <= sync.epoch {
            return SyncError::Locked;
        }
        let retained = state::retained_commitment(&self.config.paths, &sync.bundle_id, sync.epoch)
            .ok()
            .flatten();
        match (retained, clear.link_at(sync.epoch)) {
            (Some(commit), Some((link, _))) if link.digest == commit => SyncError::KeysRotated,
            _ => SyncError::Locked,
        }
    }

    /// The replica's lineage memory, read with this epoch's keys (empty
    /// when the replica does not open), plus this epoch itself.
    fn lineage_memory(
        &self,
        sync: &SyncConfig,
        bundle_id: &[u8; crypto::ID_LEN],
        keys: &crypto::Subkeys,
    ) -> Vec<state::RetainedEpoch> {
        let expected = Expected {
            bundle_id: *bundle_id,
            epoch: sync.epoch,
            max_keyset_generation: u64::MAX,
            device: sync.device,
        };
        let mut local = state::load_replica(&self.config.paths, keys, &expected)
            .ok()
            .flatten()
            .map(|opened| opened.content.local)
            .unwrap_or_default();
        if let Ok(commit) = crypto::epoch_commitment(&keys.rotate, bundle_id, sync.epoch) {
            local.remember_epoch(sync.epoch, &commit, crypto::KeyBytes::of(&keys.rotate));
        }
        local.lineage
    }

    /// Rewrites `bundle.toml` from `bundle.verified` when the folder holds
    /// a **strictly lower** `(epoch, generation, revision)` of this bundle
    /// (RR2: never over an equal or newer file). Best effort, once.
    fn restore_if_lower(paths: &crate::store::Paths, folder: &dyn FolderIo, found: &ClearHeader) {
        let Ok(Some(verified)) = state::load_bundle_copy(paths, state::BundleCopy::Verified) else {
            return;
        };
        let Ok(mine) = bundle::peek(&verified) else {
            return;
        };
        if mine.bundle_id == found.bundle_id && found.rank() < mine.rank() {
            let _ = folder.write(&FolderFile::Bundle, verified.as_bytes());
        }
    }

    /// Rewrites `bundle.toml` from `bundle.verified` (a file forged from
    /// this device's own lineage, §13.3.2). Best effort.
    fn restore_verified(paths: &crate::store::Paths, folder: &dyn FolderIo, epoch: u32) {
        let Ok(Some(verified)) = state::load_bundle_copy(paths, state::BundleCopy::Verified) else {
            return;
        };
        if bundle::peek(&verified).is_ok_and(|mine| mine.epoch == epoch) {
            let _ = folder.write(&FolderFile::Bundle, verified.as_bytes());
        }
    }

    /// A `bundle.toml` of an epoch above this device's floor (§13.3.2).
    fn judge_newer(
        &self,
        sync: &SyncConfig,
        folder: &dyn FolderIo,
        clear: &ClearHeader,
        rotate: &crypto::Subkey,
        commit: &[u8; crypto::MAC_LEN],
        memory: &[state::RetainedEpoch],
    ) -> SyncError {
        match lineage::judge_newer(clear, sync.epoch, commit, Some(rotate), memory) {
            lineage::Verdict::RotatedElsewhere => SyncError::KeysRotated,
            lineage::Verdict::ForgedFromOurs => {
                Self::restore_verified(&self.config.paths, folder, sync.epoch);
                SyncError::BundleTampered
            }
            lineage::Verdict::OtherLineage => SyncError::RotationConflict,
            lineage::Verdict::Tampered => SyncError::BundleTampered,
        }
    }

    /// "This device was removed by …" (§13.4.4).
    fn forgotten(roster: &Roster, entry: &bundle::RevokedDevice) -> SyncError {
        SyncError::DeviceForgotten {
            by: Self::label(roster, &entry.revoked_by),
            at: entry.revoked_at,
        }
    }

    /// Reads the clear header of every candidate of every **active**
    /// device before anything is merged or written (§13.3.5): a newer
    /// epoch stops the pass, naming the file (RR5).
    fn scan_newer(
        &mut self,
        folder: &dyn FolderIo,
        listing: &Listing,
        bundle: &OpenedBundle,
    ) -> Result<(), SyncError> {
        let mut found = Vec::new();
        for device in bundle.roster.devices.iter().filter(|d| !d.revoked) {
            let Some(candidates) = listing.devices.get(&device.id) else {
                continue;
            };
            for candidate in candidates {
                let Ok(Some(bytes)) = folder.read(&candidate.file, MAX_CONTAINER_FILE_BYTES) else {
                    continue;
                };
                let Some(envelope) = container::peek(container::ContainerKind::Snapshot, &bytes)
                else {
                    continue;
                };
                if envelope.device == device.id && envelope.epoch > bundle.header.epoch {
                    found.push(NewerFile {
                        device: device.id,
                        file: candidate.file.clone(),
                        epoch: envelope.epoch,
                        fingerprint: state::hex(&crypto::sha256(&bytes)),
                        label: device.name.label(&device.id),
                    });
                }
            }
        }
        let first = found.first().map(|f| f.label.clone());
        self.newer_files = found;
        match first {
            Some(device) => Err(SyncError::NewerEpochFile { device }),
            None => Ok(()),
        }
    }

    /// Loads everything a pass needs (steps 1–3 of the module doc, with
    /// the S2a checks). The caller holds `sync.lock`.
    // One ordered sequence of checks (module doc): splitting it would
    // scatter the order that makes it safe.
    #[allow(clippy::too_many_lines)]
    fn open_session(&mut self, alerts: &mut Vec<Alert>) -> crate::Result<Session> {
        let paths = self.config.paths.clone();
        let paths = &paths;
        let mut sync = state::load_config(paths)?.ok_or(SyncError::Disabled)?;
        self.local_cleanups(&sync);
        if let Some(rotation) = state::load_rotation(paths)? {
            // A transition in progress goes forward first (§13.3.4).
            self.advance_transition(&sync, &rotation)?;
            sync = state::load_config(paths)?.ok_or(SyncError::Disabled)?;
        }
        self.stamp_keys(&sync);
        let folder = self.open_folder(&sync)?;
        let listing = folder.list()?;
        let main_text = Self::read_bundle(folder.as_ref())?;
        let clear = bundle::peek(&main_text)?;
        if clear.bundle_id != sync.bundle_bytes()? {
            return Err(SyncError::BundleTampered.into());
        }
        let key = match self.data_key_for(&sync) {
            Ok(key) => key,
            Err(SyncError::Locked) => return Err(self.locked_verdict(&sync, &clear).into()),
            Err(e) => return Err(e.into()),
        };
        let epoch_keys = crypto::derive_subkeys(&key, &clear.bundle_id, sync.epoch)?;
        let commit = crypto::epoch_commitment(&epoch_keys.rotate, &clear.bundle_id, sync.epoch)?;
        match clear.epoch.cmp(&sync.epoch) {
            std::cmp::Ordering::Greater => {
                let memory = self.lineage_memory(&sync, &clear.bundle_id, &epoch_keys);
                return Err(self
                    .judge_newer(
                        &sync,
                        folder.as_ref(),
                        &clear,
                        &epoch_keys.rotate,
                        &commit,
                        &memory,
                    )
                    .into());
            }
            std::cmp::Ordering::Less => {
                // Below the floor (R2): never opened; put back what this
                // device verified.
                Self::restore_if_lower(paths, folder.as_ref(), &clear);
                return Err(SyncError::Rollback {
                    what: "bundle.toml".to_owned(),
                }
                .into());
            }
            std::cmp::Ordering::Equal => {}
        }
        if clear.commit.is_some_and(|found| found != commit) {
            // Another lineage at this device's epoch (§13.3.7): never
            // rewritten.
            let memory = self.lineage_memory(&sync, &clear.bundle_id, &epoch_keys);
            return Err(lineage::judge_other_lineage(&clear, sync.epoch, &memory).into());
        }
        let opened = match Self::open_bundle(folder.as_ref(), &listing, &main_text, &key, &sync) {
            Ok(opened) => opened,
            Err(e @ SyncError::BundleTampered) => {
                Self::restore_if_lower(paths, folder.as_ref(), &clear);
                return Err(e.into());
            }
            Err(e) => return Err(e.into()),
        };
        let OpenedSet {
            bundle,
            union: union_roster,
            clock_conflicts,
            main_won,
            rewrite,
        } = opened;
        if let Some(entry) = bundle.roster.revoked_entry(&sync.device) {
            return Err(Self::forgotten(&bundle.roster, entry).into());
        }
        self.scan_newer(folder.as_ref(), &listing, &bundle)?;
        // Never a re-join downward (§13.3.5): a replica sealed under a
        // newer epoch than the bundle means the folder went back.
        if state::replica_epoch(paths)?.is_some_and(|epoch| epoch > bundle.header.epoch) {
            return Err(SyncError::Rollback {
                what: "bundle.toml".to_owned(),
            }
            .into());
        }
        let expected = Self::expected(&bundle, sync.device);
        let loaded = match state::load_replica(paths, &bundle.keys, &expected) {
            Ok(loaded) => loaded,
            Err(crate::Error::Sync(
                SyncError::Unauthenticated { .. } | SyncError::Corrupt { .. },
            )) => {
                // An unreadable replica: start over under a new identity
                // (no dot of the old one can be reused).
                alerts.push(Alert {
                    kind: AlertKind::Restored,
                    device: None,
                    error: SyncError::RestoredState {
                        device: sync.device.short(),
                    },
                });
                None
            }
            Err(e) => return Err(e),
        };
        let name = bundle
            .roster
            .devices
            .iter()
            .find(|d| d.id == sync.device)
            .map(|d| d.name.clone());
        let (replica, replica_gen) = if let Some(opened) = loaded {
            (opened.content, opened.binding.seq)
        } else {
            let name = name.map_or_else(|| DeviceName::parse("Portico"), Ok)?;
            (fresh_replica(name, bundle.header.keyset_generation), 0)
        };
        let unreadable = loaded_was_unreadable(alerts);
        let clock = LocalClock::new(sync.device, replica.clock);
        let mut session = Session {
            sync,
            folder,
            bundle,
            replica,
            replica_gen,
            clock,
            listing,
            budget: PASS_BYTE_BUDGET,
            roster_rewrite: None,
        };
        if unreadable {
            self.rejoin(&mut session)?;
        }
        // F4: never an older bundle; a diverged one is rewritten below.
        let header = session.bundle.header.clone();
        let observed = session
            .replica
            .high_water
            .observe_bundle(
                header.epoch,
                header.keyset_generation,
                header.revision,
                &session.bundle.fingerprint,
            )
            .and_then(|observed| {
                // Ratchet on the version read (R13): v1 after v2 is a
                // rollback.
                session
                    .replica
                    .high_water
                    .ratchet_format("bundle.toml", header.version)
                    .map(|()| observed)
            });
        let observed = match observed {
            Ok(observed) => observed,
            Err(e) => {
                Self::restore_if_lower(paths, session.folder.as_ref(), &clear);
                return Err(e.into());
            }
        };
        // E1: this epoch's commitment, the verified copy and its slot.
        self.retain_lineage(&mut session, main_won.then_some(main_text.as_str()))?;
        let mut roster = union_roster;
        let mut fix = Self::enforce_revoked(&mut session, &mut roster, &clock_conflicts, alerts);
        // The merge filters of this pass use the corrected list, whether
        // or not the corrective rewrite lands (FAIBLE-6 of the S2a review).
        session.bundle.roster.revoked.clone_from(&roster.revoked);
        session.bundle.roster.devices.clone_from(&roster.devices);
        fix |= Self::ratchet_policy(&mut session, alerts);
        if rewrite || fix || observed == BundleObservation::Diverged {
            session.roster_rewrite = Some(roster);
        }
        let known = session.replica.local.known_keyset_generation;
        if known == 0 {
            session.replica.local.known_keyset_generation = header.keyset_generation;
        } else if header.keyset_generation > known {
            alerts.push(Alert {
                kind: AlertKind::PassphraseChanged,
                device: None,
                error: SyncError::PassphraseChanged,
            });
        }
        // The commitment and `sync.toml` this session may have written are
        // this process's own (MOYEN-2 of the S2a review).
        self.restamp_keys(&session.sync);
        Ok(session)
    }

    /// E1: keeps `C_E` (`commitment.toml` and the replica's lineage
    /// memory), `bundle.verified` (the main file, when it won) and its
    /// passphrase slot in `bundle.history/`.
    fn retain_lineage(&self, session: &mut Session, main_text: Option<&str>) -> crate::Result<()> {
        let paths = &self.config.paths;
        let bundle = &session.bundle;
        state::save_commitment(
            paths,
            &session.sync.bundle_id,
            bundle.header.epoch,
            &bundle.commit,
        )?;
        session.replica.local.remember_epoch(
            bundle.header.epoch,
            &bundle.commit,
            crypto::KeyBytes::of(&bundle.keys.rotate),
        );
        if let Some(text) = main_text {
            state::save_bundle_copy(paths, state::BundleCopy::Verified, text)?;
            if let Some(slot) = bundle.slot_id() {
                state::remember_slot(paths, &slot, text)?;
            }
        }
        Ok(())
    }

    /// Immutability of forgotten entries (§13.4.6): an entry observed once
    /// keeps its `revoked_clock` and is never dropped; a change is an
    /// alert naming the device and the observed value is put back.
    /// Returns whether `roster` was corrected.
    fn enforce_revoked(
        session: &mut Session,
        roster: &mut Roster,
        clock_conflicts: &[DeviceId],
        alerts: &mut Vec<Alert>,
    ) -> bool {
        let mut fixed = false;
        let mut tampered: BTreeSet<DeviceId> = clock_conflicts.iter().copied().collect();
        let seen = session.replica.local.revoked_seen.clone();
        for (device, clock) in &seen {
            match roster.revoked.iter_mut().find(|entry| entry.id == *device) {
                Some(entry) if entry.revoked_clock == *clock => {}
                Some(entry) => {
                    entry.revoked_clock = *clock;
                    tampered.insert(*device);
                    fixed = true;
                }
                None => {
                    roster.devices.retain(|d| d.id != *device);
                    roster.revoked.push(bundle::RevokedDevice {
                        id: *device,
                        revoked_at: 0,
                        revoked_by: *device,
                        revoked_clock: *clock,
                    });
                    tampered.insert(*device);
                    fixed = true;
                }
            }
        }
        roster.revoked.sort_by_key(|entry| entry.id);
        for entry in &roster.revoked {
            session
                .replica
                .local
                .revoked_seen
                .entry(entry.id)
                .or_insert(entry.revoked_clock);
        }
        for device in tampered {
            alerts.push(Alert {
                kind: AlertKind::BundleTampered,
                device: Some(Self::label(&session.bundle.roster, &device)),
                error: SyncError::BundleTampered,
            });
        }
        fixed
    }

    /// Ratchet of the policy generation (R8): an older one than seen is
    /// tampering; the bundle is then treated as "passwords off" (closed in
    /// doubt). Returns `false`: nothing to rewrite (a device holding the
    /// key wrote it; a rotation fixes it).
    fn ratchet_policy(session: &mut Session, alerts: &mut Vec<Alert>) -> bool {
        let seen = session.replica.local.policy_generation;
        let found = session.bundle.roster.policy.generation;
        if found < seen {
            alerts.push(Alert {
                kind: AlertKind::BundleTampered,
                device: None,
                error: SyncError::BundleTampered,
            });
            session.bundle.roster.policy.include_passwords = false;
        } else {
            session.replica.local.policy_generation = found;
        }
        false
    }

    /// Rewrites `bundle.toml` with `roster` (revision + 1, data key only),
    /// adding or renaming this device and moving its former ids to the
    /// forgotten list (a re-join with provenance, §13.3.5). A roster that
    /// needs v2 fields upgrades a v1 bundle ([`bundle::to_v2`]); otherwise
    /// the version read is kept. A failure is retried next pass.
    fn write_roster(&self, session: &mut Session, roster: Roster) {
        let roster = Self::with_self(session, roster);
        let needs_v2 = !roster.revoked.is_empty()
            || roster.policy != bundle::Policy::default()
            || roster.devices.iter().any(|d| d.revoked);
        let rendered = if needs_v2 {
            bundle::to_v2(&session.bundle, roster)
        } else {
            bundle::with_roster(&session.bundle, roster)
        };
        let Ok((text, opened)) = rendered else {
            return;
        };
        if session
            .folder
            .write(&FolderFile::Bundle, text.as_bytes())
            .is_err()
        {
            return;
        }
        let header = opened.header.clone();
        let observed = session
            .replica
            .high_water
            .observe_bundle(
                header.epoch,
                header.keyset_generation,
                header.revision,
                &opened.fingerprint,
            )
            .and_then(|_| {
                session
                    .replica
                    .high_water
                    .ratchet_format("bundle.toml", header.version)
            });
        if observed.is_ok() {
            let _ = state::save_bundle_copy(&self.config.paths, state::BundleCopy::Verified, &text);
            if let Some(slot) = opened.slot_id() {
                let _ = state::remember_slot(&self.config.paths, &slot, &text);
            }
            for entry in &opened.roster.revoked {
                session
                    .replica
                    .local
                    .revoked_seen
                    .entry(entry.id)
                    .or_insert(entry.revoked_clock);
            }
            session.replica.local.retire.clear();
            session.bundle = opened;
        }
    }

    /// `roster` with this device active under its current name and its
    /// former ids forgotten with their last clock.
    fn with_self(session: &Session, mut roster: Roster) -> Roster {
        let me = session.sync.device;
        let name = session.replica.device_name.clone();
        match roster.devices.iter_mut().find(|d| d.id == me) {
            Some(entry) => entry.name = name,
            None => roster.devices.push(RosterDevice {
                id: me,
                name,
                added_at: now(),
                revoked: false,
            }),
        }
        for (former, clock) in &session.replica.local.retire {
            roster.devices.retain(|d| d.id != *former);
            if roster.revoked_entry(former).is_none() {
                roster.revoked.push(bundle::RevokedDevice {
                    id: *former,
                    revoked_at: now(),
                    revoked_by: *former,
                    revoked_clock: *clock,
                });
            }
        }
        roster.devices.sort_by_key(|d| d.id);
        roster.revoked.sort_by_key(|d| d.id);
        roster
    }

    /// Makes sure this device is in the roster under its current name (and
    /// its former ids are forgotten).
    fn ensure_in_roster(&self, session: &mut Session) {
        let me = session.sync.device;
        let present = session
            .bundle
            .roster
            .devices
            .iter()
            .any(|d| d.id == me && d.name == session.replica.device_name);
        let retired = session
            .replica
            .local
            .retire
            .keys()
            .all(|former| session.bundle.roster.revoked_entry(former).is_some());
        if !present || !retired {
            let roster = session.bundle.roster.clone();
            self.write_roster(session, roster);
        }
    }

    /// The non-revoked devices of the roster.
    fn active_roster(session: &Session) -> BTreeSet<DeviceId> {
        session
            .bundle
            .roster
            .devices
            .iter()
            .filter(|d| !d.revoked)
            .map(|d| d.id)
            .collect()
    }

    /// Forgotten devices and their exact bound (§13.4.2).
    fn revoked_clocks(session: &Session) -> BTreeMap<DeviceId, u64> {
        session
            .bundle
            .roster
            .revoked
            .iter()
            .map(|entry| (entry.id, entry.revoked_clock))
            .collect()
    }

    /// Merges what this device's **former** identity published (its data
    /// dir was restored or cloned), before it re-joins under a new id. Like
    /// any device's snapshot: its context claims are checked first (claims
    /// on other devices not backed by their own files: deferred, merged by
    /// a later pass as the former id's file), and a refused merge is an
    /// alert. Both name the former id.
    fn merge_former_self(
        session: &mut Session,
        opened: &Opened<SnapshotContent>,
        alerts: &mut Vec<Alert>,
        issues: &mut Vec<Issue>,
        waiting: &mut Option<(WaitReason, Option<String>)>,
    ) {
        let former = session.sync.device;
        let label = opened.content.device_name.label(&former);
        let roster = Self::active_roster(session);
        let revoked = Self::revoked_clocks(session);
        let verified = session.replica.high_water.verified_clocks();
        let context = opened.content.state.context();
        match guard::check_context_claims(&former, context, &roster, &revoked, &verified) {
            ClaimCheck::Accept => {
                if let Err(error) = session.replica.state.merge(&opened.content.state, &former) {
                    alerts.push(Alert {
                        kind: AlertKind::MergeRefused,
                        device: Some(label),
                        error,
                    });
                }
            }
            ClaimCheck::Defer(_) => {
                issues.push(Issue {
                    key: format!("device:{former}"),
                    record: None,
                    error: SyncError::Deferred {
                        device: label.clone(),
                    },
                    warning: true,
                    skippable: false,
                });
                waiting.get_or_insert((WaitReason::Deferred, Some(label)));
            }
        }
    }

    /// A deferred deletion (state doc) whose tombstone is gone from the
    /// state (collected by the other devices) and whose key is readable
    /// again is applied like a received tombstone; a key that holds a
    /// register again (a value written since) is left to it.
    fn inject_deferred_deletions(
        session: &Session,
        view: &LocalView,
        projection: &mut super::model::Projection,
    ) {
        for deferred in &session.replica.local.deferred_deletions {
            let Some(key) = deferred.key.decode() else {
                continue;
            };
            if view.is_suspended(&key)
                || view.unsynced.contains(&key)
                || session.replica.state.view(&key).is_some()
                || deferred.dots.is_empty()
            {
                continue;
            }
            projection.known.insert(
                key,
                vec![TypedSibling {
                    value: RecordValue::Tombstone,
                    dots: deferred.dots.clone(),
                    at: deferred.at,
                }],
            );
        }
    }

    /// Records the deletions this pass could not apply (suspended kind,
    /// unsynced key) and forgets those whose key was readable in it.
    fn track_deferred_deletions(
        session: &mut Session,
        after: &LocalView,
        projection: &super::model::Projection,
    ) {
        let blocked = |key: &RecordKey| after.is_suspended(key) || after.unsynced.contains(key);
        let mut deferred: BTreeMap<WireKey, state::DeferredDeletion> = session
            .replica
            .local
            .deferred_deletions
            .drain(..)
            .filter(|d| d.key.decode().is_some_and(|key| blocked(&key)))
            .map(|d| (d.key.clone(), d))
            .collect();
        for (key, siblings) in &projection.known {
            let ([sibling], true) = (siblings.as_slice(), blocked(key)) else {
                continue;
            };
            if !sibling.value.is_tombstone() {
                continue;
            }
            if let Ok(wire) = WireKey::encode(key) {
                deferred.insert(
                    wire.clone(),
                    state::DeferredDeletion {
                        key: wire,
                        dots: sibling.dots.clone(),
                        at: sibling.at,
                    },
                );
            }
        }
        session.replica.local.deferred_deletions = deferred.into_values().collect();
    }

    /// Re-joins under a new device id (restored or cloned data dir, F5):
    /// the merged state is kept, the clock and publish sequence restart
    /// under the new id, so no dot is ever reused.
    fn rejoin(&self, session: &mut Session) -> crate::Result<()> {
        let device = DeviceId::generate()?;
        session.sync.device = device;
        session.clock = LocalClock::new(device, 0);
        session.replica.clock = 0;
        session.replica.publish_seq = 0;
        session.replica.local.published_fingerprint = None;
        session.replica.local.published_state = None;
        session.replica_gen = 0;
        state::save_config(&self.config.paths, &session.sync)?;
        self.save_replica(session)?;
        self.ensure_in_roster(session);
        Ok(())
    }

    fn save_replica(&self, session: &mut Session) -> crate::Result<()> {
        session.replica.clock = session.clock.counter();
        let next = session
            .replica_gen
            .checked_add(1)
            .ok_or(SyncError::Corrupt {
                what: "local replica",
                reason: "generation exhausted",
            })?;
        let binding = Binding {
            bundle_id: session.bundle.header.bundle_id,
            epoch: session.bundle.header.epoch,
            keyset_generation: session.bundle.header.keyset_generation,
            device: session.sync.device,
            seq: next,
        };
        state::save_replica(
            &self.config.paths,
            &session.bundle.keys,
            &binding,
            &session.replica,
        )?;
        session.replica_gen = next;
        Ok(())
    }

    /// The best (highest valid sequence) snapshot of `device` among its
    /// candidates. Unauthenticated copies (torn, uploading) are skipped; a
    /// copy whose clear header announces an older epoch is never merged
    /// and marks the device "needs the new keys" (§13.3.6).
    fn best_snapshot(
        session: &mut Session,
        device: DeviceId,
        partial: &mut bool,
        stale_keys: &mut BTreeSet<DeviceId>,
        issues: &mut Vec<Issue>,
    ) -> Option<(FolderFile, Opened<SnapshotContent>)> {
        let candidates = session
            .listing
            .devices
            .get(&device)
            .cloned()
            .unwrap_or_default();
        let expected = Self::expected(&session.bundle, device);
        let mut best: Option<(FolderFile, Opened<SnapshotContent>)> = None;
        for candidate in candidates {
            if candidate.size > session.budget {
                *partial = true;
                continue;
            }
            session.budget = session.budget.saturating_sub(candidate.size);
            let bytes = match session
                .folder
                .read(&candidate.file, MAX_CONTAINER_FILE_BYTES)
            {
                Ok(Some(bytes)) => bytes,
                Ok(None) => continue,
                Err(e) => {
                    issues.push(Issue {
                        key: format!("device:{device}"),
                        record: None,
                        error: e,
                        warning: false,
                        skippable: false,
                    });
                    continue;
                }
            };
            if container::peek(container::ContainerKind::Snapshot, &bytes)
                .is_some_and(|envelope| envelope.epoch < session.bundle.header.epoch)
            {
                // This device's own older file is only behind (published
                // again under the new keys).
                if device != session.sync.device {
                    stale_keys.insert(device);
                }
                continue;
            }
            match container::open_snapshot(&session.bundle.keys, &expected, &bytes) {
                Ok(opened) => {
                    if best
                        .as_ref()
                        .is_none_or(|(_, b)| opened.binding.seq > b.binding.seq)
                    {
                        best = Some((candidate.file, opened));
                    }
                }
                // Torn or still uploading (E2): "waiting", never a
                // deletion; a version or limit problem is reported.
                Err(SyncError::Unauthenticated { .. } | SyncError::Corrupt { .. }) => {
                    *partial = true;
                }
                Err(e) => issues.push(Issue {
                    key: format!("device:{device}"),
                    record: None,
                    error: e,
                    warning: false,
                    skippable: false,
                }),
            }
        }
        best
    }

    // ── The pass ────────────────────────────────────────────────────────

    /// One pass (see the module doc), waiting for the other Portico
    /// process when it holds `sync.lock` (user commands). Errors become
    /// the status.
    pub(crate) fn pass(&mut self) -> crate::Result<SyncStatus> {
        self.pass_with(true)
    }

    /// The background pass: skipped when the other process is syncing.
    pub(crate) fn background_pass(&mut self) -> crate::Result<SyncStatus> {
        self.pass_with(false)
    }

    fn pass_with(&mut self, wait: bool) -> crate::Result<SyncStatus> {
        let paths = self.config.paths.clone();
        if !state::is_set_up(&paths) {
            self.keys.clear();
            // Left behind by a disable (possibly by 0.6): meaningless now
            // (RR6).
            let _ = state::remove_transition_files(&paths);
            self.set_status(SyncStatus::Disabled);
            return Ok(SyncStatus::Disabled);
        }
        crate::store::create_private_dir_all(&paths.sync_dir())?;
        let _lock = if wait {
            state::lock(&paths)?
        } else if let Some(lock) = state::try_lock(&paths)? {
            lock
        } else {
            // The other Portico process is syncing (§4.9): next poll.
            return Ok(self.status());
        };
        let result = self.pass_locked();
        // Still under `sync.lock`: whatever changed `sync.toml`,
        // `rotation.toml` or `commitment.toml` during the pass was this
        // process (a first commitment, a re-join): the keys it holds stay
        // valid (a "don't remember" device must not lock itself).
        self.restamp_from_disk();
        match result {
            Ok(status) => {
                self.set_status(status.clone());
                Ok(status)
            }
            Err(e) => {
                let status = status_for(&e);
                self.set_status(status);
                Err(e)
            }
        }
    }

    // One pass is one ordered sequence (module doc): splitting it would
    // scatter the crash-safety order.
    #[allow(clippy::too_many_lines)]
    fn pass_locked(&mut self) -> crate::Result<SyncStatus> {
        let mut alerts: Vec<Alert> = Vec::new();
        let mut issues: Vec<Issue> = Vec::new();
        let mut session = self.open_session(&mut alerts)?;
        let at = now();
        let me = session.sync.device;

        alerts.append(&mut self.carry_alerts);
        // ── This device's own file (F5), before any write ──
        let mut partial = false;
        let mut stale_keys: BTreeSet<DeviceId> = BTreeSet::new();
        let own = Self::best_snapshot(&mut session, me, &mut partial, &mut stale_keys, &mut issues);
        let local_fingerprint = session
            .replica
            .local
            .published_fingerprint
            .as_deref()
            .and_then(unhex32);
        let published = own
            .as_ref()
            .map(|(_, o)| (o.binding.seq, o.content.clock, &o.fingerprint));
        let mut force_publish = false;
        let own_check = guard::check_own_file(
            &me,
            session.replica.publish_seq,
            session.clock.counter(),
            local_fingerprint.as_ref(),
            published,
        )
        .and_then(|verdict| {
            if let Some((_, opened)) = &own {
                session.clock.observe_own_counter(opened.content.clock)?;
                session
                    .clock
                    .observe_own_context(opened.content.state.context())?;
            }
            Ok(verdict)
        });
        let mut waiting: Option<(WaitReason, Option<String>)> = None;
        match own_check {
            Ok(OwnFile::InSync) => {}
            Ok(OwnFile::Behind) => force_publish = true,
            Err(e @ (SyncError::RestoredState { .. } | SyncError::Fork { .. })) => {
                alerts.push(Alert {
                    kind: AlertKind::Restored,
                    device: None,
                    error: e,
                });
                if let Some((_, opened)) = &own {
                    Self::merge_former_self(
                        &mut session,
                        opened,
                        &mut alerts,
                        &mut issues,
                        &mut waiting,
                    );
                }
                self.rejoin(&mut session)?;
                force_publish = true;
            }
            Err(e) => return Err(e.into()),
        }
        // Only now (own file checked, §5.1) may `bundle.toml` be written.
        if let Some(union_roster) = session.roster_rewrite.take() {
            self.write_roster(&mut session, union_roster);
        }
        let me = session.sync.device;
        // Our own provider copies, verified and older: removed (only ever
        // copies of our own id).
        if let Some(candidates) = session.listing.devices.get(&me).cloned() {
            let expected = Self::expected(&session.bundle, me);
            for candidate in candidates.iter().filter(|c| !c.primary) {
                let verified_older = session
                    .folder
                    .read(&candidate.file, MAX_CONTAINER_FILE_BYTES)
                    .ok()
                    .flatten()
                    .and_then(|bytes| {
                        container::open_snapshot(&session.bundle.keys, &expected, &bytes).ok()
                    })
                    .is_some_and(|copy| copy.binding.seq <= session.replica.publish_seq);
                if verified_older {
                    let _ = session.folder.remove(&candidate.file);
                }
            }
        }
        self.ensure_in_roster(&mut session);

        // ── Every other device, observed before any context is judged ──
        let roster = Self::active_roster(&session);
        let revoked = Self::revoked_clocks(&session);
        // Files of forgotten devices: ignored and removed (§13.4.3).
        Self::remove_forgotten_files(&session, &revoked);
        let others: Vec<DeviceId> = session
            .listing
            .devices
            .keys()
            .copied()
            .filter(|d| *d != me && !revoked.contains_key(d))
            .collect();
        let mut received: Vec<(DeviceId, Opened<SnapshotContent>)> = Vec::new();
        let mut present: BTreeMap<DeviceId, u64> = BTreeMap::new();
        let mut seen_vectors: Vec<BTreeMap<DeviceId, u64>> = Vec::new();
        for device in others {
            let Some((_, opened)) = Self::best_snapshot(
                &mut session,
                device,
                &mut partial,
                &mut stale_keys,
                &mut issues,
            ) else {
                continue;
            };
            present.insert(device, opened.binding.seq);
            session
                .replica
                .local
                .peer_versions
                .insert(device, opened.content.app_version.clone());
            seen_vectors.push(opened.content.seen.clone());
            let label = opened.content.device_name.label(&device);
            match session.replica.high_water.observe_snapshot(
                device,
                opened.binding.seq,
                opened.content.clock,
                &opened.fingerprint,
                at,
            ) {
                Ok(_) => {}
                Err(e) => {
                    let kind = if matches!(e, SyncError::Fork { .. }) {
                        AlertKind::Fork
                    } else {
                        AlertKind::Rollback
                    };
                    alerts.push(Alert {
                        kind,
                        device: Some(label),
                        error: e,
                    });
                    continue;
                }
            }
            let seen = session
                .replica
                .local
                .seen
                .get(&device)
                .copied()
                .unwrap_or(0);
            if opened.binding.seq > seen {
                received.push((device, opened));
            }
        }
        // This device's own former ids (a re-join of unknown origin) are
        // neither waited for nor "behind": their file is this device's past
        // (FAIBLE-2 of the S2a review).
        let superseded: BTreeSet<DeviceId> =
            session.replica.local.superseded.iter().copied().collect();
        stale_keys.retain(|device| !superseded.contains(device));
        // Devices whose newer snapshot others merged but which is not in
        // the folder (yet): withheld or still uploading.
        for withheld in guard::withheld(&present, seen_vectors.iter()) {
            // A device still on older keys is "needs the new keys", not
            // withheld: its file is there, under the previous epoch. A
            // forgotten (or retired) id is never waited for.
            if withheld.device == me
                || stale_keys.contains(&withheld.device)
                || superseded.contains(&withheld.device)
                || revoked.contains_key(&withheld.device)
            {
                continue;
            }
            let label = Self::label(&session.bundle.roster, &withheld.device);
            issues.push(Issue {
                key: format!("device:{}", withheld.device),
                record: None,
                error: SyncError::PendingFile {
                    device: label.clone(),
                },
                warning: true,
                skippable: false,
            });
            waiting.get_or_insert((WaitReason::PartialSync, Some(label)));
        }
        let mut verified = session.replica.high_water.verified_clocks();
        verified.insert(me, session.clock.counter());
        // A former id of this device whose clock it no longer knows: what
        // the others relay about it is accepted until it is retired.
        for former in &superseded {
            verified.insert(*former, u64::MAX);
        }
        let mut accepted = Vec::new();
        let mut deferred_any = false;
        for (device, opened) in received {
            let label = opened.content.device_name.label(&device);
            let context = opened.content.state.context();
            if let Err(e) = session.clock.check_claim(context, &device) {
                alerts.push(Alert {
                    kind: AlertKind::ThirdPartyClaim,
                    device: Some(label.clone()),
                    error: e,
                });
            }
            match guard::check_context_claims(&device, context, &roster, &revoked, &verified) {
                ClaimCheck::Accept => accepted.push((device, opened)),
                ClaimCheck::Defer(claims) => {
                    deferred_any = true;
                    // Writes of a forgotten device beyond its exact bound,
                    // relayed by `device` (§13.4.2).
                    for claim in claims.iter().filter(|claim| claim.revoked) {
                        alerts.push(Alert {
                            kind: AlertKind::RevokedWrites,
                            device: Some(label.clone()),
                            error: SyncError::RevokedWrites {
                                device: label.clone(),
                                forgotten: Self::label(&session.bundle.roster, &claim.device),
                            },
                        });
                    }
                    issues.push(Issue {
                        key: format!("device:{device}"),
                        record: None,
                        error: SyncError::Deferred {
                            device: label.clone(),
                        },
                        warning: true,
                        skippable: false,
                    });
                    waiting.get_or_insert((WaitReason::Deferred, Some(label)));
                }
            }
        }

        // ── Local changes (projection), then the merge ──
        let mut view = project::observe(&self.config.paths, &self.snippets);
        let held = Self::hold_values(&mut session, &mut view);
        for problem in &view.problems {
            issues.push(Issue {
                key: "kind:hosts".to_owned(),
                record: None,
                error: problem.clone(),
                warning: false,
                skippable: false,
            });
        }
        let applied_before = session.replica.local.applied_map();
        let changes = project::local_changes(&view, &applied_before);
        let mut failed: BTreeSet<RecordKey> = BTreeSet::new();
        for change in changes {
            let result = match &change.value {
                Some(value) => session
                    .replica
                    .state
                    .write(&mut session.clock, &change.key, value, at)
                    .map(|_| ()),
                None => session
                    .replica
                    .state
                    .delete(&mut session.clock, &change.key, at)
                    .map(|_| ()),
            };
            match result {
                Ok(()) => {
                    // A local change of a host ends any confirmation or
                    // refusal recorded for it (single use, F1).
                    if let RecordKey::Host { alias } = &change.key {
                        session.replica.local.confirmed.remove(alias);
                        session.replica.local.rejected.remove(alias);
                    }
                }
                Err(e @ SyncError::RestoredState { .. }) => return Err(e.into()),
                Err(e) => {
                    issues.push(Issue {
                        key: issue_key(&change.key),
                        record: Some(change.key.clone()),
                        error: e,
                        warning: false,
                        skippable: false,
                    });
                    failed.insert(change.key);
                }
            }
        }
        for (device, opened) in accepted {
            let label = opened.content.device_name.label(&device);
            match merge_received(&mut session.replica.state, &opened.content.state, &device) {
                Ok(report) => {
                    session
                        .replica
                        .local
                        .seen
                        .insert(device, opened.binding.seq);
                    session
                        .replica
                        .local
                        .peer_contexts
                        .insert(device, opened.content.state.context().clone());
                    if report.divergent_dots > 0 {
                        alerts.push(Alert {
                            kind: AlertKind::Divergent,
                            device: Some(label.clone()),
                            error: SyncError::DivergentDots {
                                device: label.clone(),
                                count: report.divergent_dots,
                            },
                        });
                    }
                    if report.dropped_live_without_successor > 0 {
                        alerts.push(Alert {
                            kind: AlertKind::DroppedValues,
                            device: Some(label.clone()),
                            error: SyncError::DroppedWithoutSuccessor {
                                device: label,
                                count: report.dropped_live_without_successor,
                            },
                        });
                    }
                }
                Err(e) => alerts.push(Alert {
                    kind: AlertKind::MergeRefused,
                    device: Some(label),
                    error: e,
                }),
            }
        }
        // Former ids retired once every current snapshot was merged: the
        // bound is the highest clock the others relay for it, as a forget
        // fixes it (§13.4.2).
        if !superseded.is_empty() && !deferred_any && !partial {
            for former in &superseded {
                let clock = session.replica.state.context().get(former);
                session.replica.local.retire.insert(*former, clock);
            }
            session.replica.local.superseded.clear();
            self.ensure_in_roster(&mut session);
        }
        // ── Durable before anything is applied or published ──
        self.save_replica(&mut session)?;

        // ── Validated application ──
        let mut projection = session.replica.state.project();
        Self::inject_deferred_deletions(&session, &view, &mut projection);
        let outcome = apply::apply(&ApplyInput {
            config: &self.config,
            snippets: &self.snippets,
            projection: &projection,
            view: &view,
            excluded: &session.sync.excluded,
            confirmed: &session.replica.local.confirmed,
            rejected: &session.replica.local.rejected,
        });
        // A confirmation is used up once its destination is written (F1):
        // the same destination arriving again later asks again.
        for alias in &outcome.written_hosts {
            session.replica.local.confirmed.remove(alias);
        }
        let after = project::observe(&self.config.paths, &self.snippets);
        let mut applied = settle_applied(&view, &after, &projection, &applied_before);
        for key in &failed {
            if let Ok(wire) = WireKey::encode(key) {
                match applied_before.get(&wire) {
                    Some(value) => {
                        applied.insert(wire, value.clone());
                    }
                    None => {
                        applied.remove(&wire);
                    }
                }
            }
        }
        session.replica.local.set_applied(applied);
        Self::track_deferred_deletions(&mut session, &after, &projection);
        // Tombstone collection on the stable context of the roster, once the
        // deletions were applied here.
        let stale_devices: BTreeSet<DeviceId> = session
            .replica
            .high_water
            .stale_devices(at, STALE_HORIZON_SECS)
            .into_iter()
            .collect();
        let mut contexts: Vec<&VersionVector> = Vec::new();
        let mut complete = true;
        for device in roster.iter().filter(|d| !stale_devices.contains(d)) {
            if *device == me {
                contexts.push(session.replica.state.context());
            } else if let Some(context) = session.replica.local.peer_contexts.get(device) {
                contexts.push(context);
            } else {
                complete = false;
            }
        }
        if complete && !contexts.is_empty() {
            let stable = VersionVector::meet(contexts);
            // A deletion still present in the local files (not applied
            // yet: refused, conflict elsewhere), or whose kind or key cannot
            // be read here (suspended file, unsynced entry), keeps its
            // tombstone: it is applied once the file is readable again.
            let present: BTreeSet<WireKey> = after
                .records
                .keys()
                .filter_map(|key| WireKey::encode(key).ok())
                .collect();
            session
                .replica
                .state
                .collect_tombstones_except(&stable, |wire| {
                    present.contains(wire)
                        || wire.decode().is_none_or(|key| {
                            after.is_suspended(&key) || after.unsynced.contains(&key)
                        })
                });
        }

        // Held values (R3, RR4): their own issue replaces "unsynced".
        issues.extend(
            outcome
                .issues
                .iter()
                .filter(|issue| issue.record.as_ref().is_none_or(|key| !held.contains(key)))
                .cloned(),
        );
        issues.extend(Self::held_issues(&session));
        for device in &stale_keys {
            let label = Self::label(&session.bundle.roster, device);
            issues.push(Issue {
                key: format!("device:{device}"),
                record: None,
                error: SyncError::NeedsNewKeys {
                    device: label.clone(),
                },
                warning: true,
                skippable: false,
            });
            waiting.get_or_insert((WaitReason::NeedsNewKeys, Some(label)));
        }
        self.save_replica(&mut session)?;
        self.collect_old_keys(&session.sync);

        // ── Publication ──
        let state_hash = state::hex(&crypto::sha256(
            serde_json::to_string(&session.replica.state)
                .unwrap_or_default()
                .as_bytes(),
        ));
        let state_changed =
            session.replica.local.published_state.as_deref() != Some(state_hash.as_str());
        if (state_changed || force_publish)
            && let Err(e) = self.publish(&mut session, state_hash, at)
        {
            issues.push(Issue {
                key: "kind:publish".to_owned(),
                record: None,
                error: e,
                warning: true,
                skippable: false,
            });
        }

        // ── Report, events ──
        if outcome.hosts_changed {
            self.emit(EngineEvent::HostsChanged);
        }
        for (alias, meta) in &outcome.meta_changed {
            self.emit(EngineEvent::HostMeta {
                alias: alias.clone(),
                meta: meta.clone(),
            });
        }
        if outcome.snippets_changed {
            self.emit(EngineEvent::SnippetsChanged);
        }
        if let Some(settings) = &outcome.settings_changed {
            self.emit(EngineEvent::SettingsChanged {
                settings: settings.clone(),
            });
        }
        if partial {
            waiting.get_or_insert((WaitReason::PartialSync, None));
        }
        if let Some(device) = session.listing.placeholders.iter().next() {
            waiting.get_or_insert((
                WaitReason::Placeholder,
                Some(Self::label(&session.bundle.roster, device)),
            ));
            for device in &session.listing.placeholders {
                issues.push(Issue {
                    key: format!("device:{device}"),
                    record: None,
                    error: SyncError::PendingFile {
                        device: Self::label(&session.bundle.roster, device),
                    },
                    warning: true,
                    skippable: false,
                });
            }
        }
        let report = Self::build_report(
            &session,
            &after.records,
            issues,
            alerts,
            &outcome.pending,
            waiting,
            at,
        );
        let status = Self::status_of(&report, at);
        self.peer_versions = session.replica.local.peer_versions.clone();
        self.withheld = guard::withheld(&present, seen_vectors.iter())
            .into_iter()
            .map(|w| w.device)
            .collect();
        self.bundle_version = session.bundle.header.version;
        let held_before: BTreeSet<(String, String)> = self
            .report
            .held
            .iter()
            .map(|h| (h.key.clone(), h.hash.clone()))
            .collect();
        let held_now: BTreeSet<(String, String)> = report
            .held
            .iter()
            .map(|h| (h.key.clone(), h.hash.clone()))
            .collect();
        let count = report.conflicts.len();
        let pending = PendingSet {
            hosts: report
                .pending
                .iter()
                .map(|p| (p.alias.clone(), p.destination_hash.clone()))
                .collect(),
            snippets: apply::unconfirmed_snippets(&self.config.paths, &self.snippets)
                .unwrap_or_default()
                .into_iter()
                .map(|u| (u.id, u.hash))
                .collect(),
        };
        let writers = Self::snippet_writers(&session, &pending.snippets);
        *self.writers.lock().unwrap_or_else(PoisonError::into_inner) = writers;
        self.report = report;
        if count != self.conflicts_count {
            self.conflicts_count = count;
            self.emit(EngineEvent::SyncConflicts { count });
        }
        let announced = held_before != held_now
            || match &self.pending_seen {
                Some(seen) => *seen != pending,
                None => pending != PendingSet::default(),
            };
        self.pending_seen = Some(pending);
        if announced {
            self.emit(EngineEvent::SyncPendingChanged);
        }
        Ok(status)
    }

    fn publish(&self, session: &mut Session, state_hash: String, at: i64) -> Result<(), SyncError> {
        let me = session.sync.device;
        let seq = session
            .replica
            .publish_seq
            .checked_add(1)
            .ok_or(SyncError::Corrupt {
                what: "local replica",
                reason: "publish sequence exhausted",
            })?;
        let content = SnapshotContent {
            device_name: session.replica.device_name.clone(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            published_at: at,
            clock: session.clock.counter(),
            seen: session.replica.local.seen.clone(),
            state: session.replica.state.clone(),
        };
        let binding = Binding {
            bundle_id: session.bundle.header.bundle_id,
            epoch: session.bundle.header.epoch,
            keyset_generation: session.bundle.header.keyset_generation,
            device: me,
            seq,
        };
        let sealed = container::seal_snapshot(&session.bundle.keys, &binding, &content)?;
        session.replica.publish_seq = seq;
        session.replica.local.published_fingerprint = Some(state::hex(&sealed.fingerprint));
        session.replica.local.published_state = Some(state_hash);
        // Durable before the file exists (a crash leaves our file behind,
        // which only means "publish again").
        self.save_replica(session).map_err(sync_err)?;
        session
            .folder
            .write(&FolderFile::primary(&me), sealed.text.as_bytes())
    }

    /// Who wrote each gated snippet (`id → hash` as gated): the devices of
    /// the register's side with that content (all sides when none matches),
    /// this device left out (a "keep both" copy is its own write).
    fn snippet_writers(
        session: &Session,
        gated: &BTreeMap<String, String>,
    ) -> BTreeMap<String, Vec<String>> {
        let me = session.sync.device;
        let roster = &session.bundle.roster;
        gated
            .iter()
            .filter_map(|(id, hash)| {
                let key = RecordKey::Snippet { id: id.clone() };
                let Some(RegisterView::Known(siblings)) = session.replica.state.view(&key) else {
                    return None;
                };
                let same = |sibling: &&TypedSibling| match &sibling.value {
                    RecordValue::Snippet(value) => {
                        let snippet = project::snippet_from(id, value);
                        apply::snippet_hash(&snippet.command, snippet.run) == *hash
                    }
                    _ => false,
                };
                let matching: Vec<&TypedSibling> = siblings.iter().filter(same).collect();
                let sides = if matching.is_empty() {
                    siblings.iter().collect()
                } else {
                    matching
                };
                let devices: BTreeSet<DeviceId> = sides
                    .iter()
                    .flat_map(|s| s.dots.iter().map(|d| d.device))
                    .filter(|d| *d != me)
                    .collect();
                (!devices.is_empty()).then(|| {
                    let labels = devices.iter().map(|d| Self::label(roster, d)).collect();
                    (id.clone(), labels)
                })
            })
            .collect()
    }

    fn build_report(
        session: &Session,
        records: &BTreeMap<RecordKey, RecordValue>,
        issues: Vec<Issue>,
        alerts: Vec<Alert>,
        pending: &[apply::PendingHost],
        waiting: Option<(WaitReason, Option<String>)>,
        at: i64,
    ) -> Report {
        let me = session.sync.device;
        let roster = &session.bundle.roster;
        let labels = |dots: &[super::clock::Dot]| -> Vec<String> {
            let devices: BTreeSet<DeviceId> = dots.iter().map(|d| d.device).collect();
            devices.iter().map(|d| Self::label(roster, d)).collect()
        };
        let conflicts = session
            .replica
            .state
            .conflicts()
            .into_iter()
            .map(|conflict| SyncConflict {
                id: conflict_id(&conflict.key),
                subject: subject(&conflict.key),
                sides_hash: apply::sides_hash(&conflict.siblings),
                sides: conflict
                    .siblings
                    .iter()
                    .enumerate()
                    .map(|(index, sibling)| ConflictSide {
                        index,
                        devices: labels(&sibling.dots),
                        local: sibling.dots.iter().any(|d| d.device == me),
                        at: sibling.at,
                        value: value_dto(&sibling.value),
                    })
                    .collect(),
            })
            .collect();
        let pending = pending
            .iter()
            .map(|p| PendingHostChange {
                alias: p.alias.clone(),
                new: p.current.is_none(),
                current: p.current.as_ref().map(host_dto),
                incoming: host_dto(&p.incoming),
                renamed_from: p.incoming.renamed_from.clone(),
                devices: labels(&p.dots),
                destination_hash: p.hash.clone(),
            })
            .collect();
        let stale: BTreeSet<DeviceId> = session
            .replica
            .high_water
            .stale_devices(at, STALE_HORIZON_SECS)
            .into_iter()
            .collect();
        let devices = roster
            .devices
            .iter()
            .map(|d| {
                let mark = session.replica.high_water.devices.get(&d.id);
                SyncDevice {
                    id: d.id.to_string(),
                    name: d.name.as_str().to_owned(),
                    label: d.name.label(&d.id),
                    this_device: d.id == me,
                    seq: if d.id == me {
                        session.replica.publish_seq
                    } else {
                        mark.map_or(0, |m| m.seq)
                    },
                    published_at: None,
                    last_seen: mark.map(|m| m.observed_at),
                    stale: stale.contains(&d.id),
                    revoked: d.revoked,
                }
            })
            .collect();
        let held = session
            .replica
            .local
            .held_values
            .iter()
            .filter_map(|(wire, value)| {
                let key = wire.decode()?;
                let local = records.get(&key);
                let synced = session.replica.state.value(&key);
                Some(api::RevokedValueItem {
                    key: conflict_id(&key),
                    subject: subject(&key),
                    device: value.label.clone(),
                    local: Self::held_shown(&key, local),
                    synced: Self::held_shown(&key, synced.as_ref()),
                    hash: Self::held_hash(value.label.as_deref(), local, synced.as_ref()),
                })
            })
            .collect();
        Report {
            issues,
            alerts,
            pending,
            conflicts,
            devices,
            waiting,
            held,
        }
    }

    fn status_of(report: &Report, at: i64) -> SyncStatus {
        if let Some(alert) = report.alerts.first() {
            return SyncStatus::Alert {
                kind: alert.kind,
                device: alert.device.clone(),
            };
        }
        if let Some((reason, device)) = &report.waiting {
            return SyncStatus::Waiting {
                reason: *reason,
                device: device.clone(),
            };
        }
        SyncStatus::Idle {
            last_pass: at,
            devices: report.devices.len(),
            conflicts: report.conflicts.len(),
            issues: report.issues.iter().filter(|i| !i.warning).count(),
            pending_confirmations: report.pending.len(),
        }
    }

    // ── Reports for the UIs ─────────────────────────────────────────────

    pub(crate) fn conflicts(&self) -> Vec<SyncConflict> {
        self.report.conflicts.clone()
    }

    pub(crate) fn pending(&self) -> Vec<PendingHostChange> {
        self.report.pending.clone()
    }

    pub(crate) fn devices(&self) -> Vec<SyncDevice> {
        self.report.devices.clone()
    }

    pub(crate) fn issues(&self) -> Vec<SyncIssue> {
        let alerts = self.report.alerts.iter().map(|alert| SyncIssue {
            key: format!("alert:{:?}", alert.kind).to_lowercase(),
            subject: None,
            summary: crate::Error::from(alert.error.clone()).summary(),
            warning: false,
            skippable: false,
        });
        let issues = self.report.issues.iter().map(|issue| SyncIssue {
            key: issue.key.clone(),
            subject: issue.record.as_ref().map(subject),
            summary: crate::Error::from(issue.error.clone()).summary(),
            warning: issue.warning,
            skippable: issue.skippable,
        });
        alerts.chain(issues).collect()
    }

    // ── Set-up ──────────────────────────────────────────────────────────

    /// What `folder` holds (no write).
    pub(crate) fn probe(&self, folder: &Path) -> FolderProbe {
        let root = match folder::check_root(folder, &self.config.paths) {
            Ok(root) => root,
            Err(SyncError::FolderMissing) => return FolderProbe::Missing,
            Err(e) => {
                return FolderProbe::Refused {
                    summary: crate::Error::from(e).summary(),
                };
            }
        };
        let io = self.tuning.opener.open(&root);
        match io.check() {
            Err(SyncError::FolderNotPortico) => FolderProbe::Usable,
            Err(e) => FolderProbe::Refused {
                summary: crate::Error::from(e).summary(),
            },
            Ok(()) => match io.read(&FolderFile::Bundle, bundle::MAX_BUNDLE_BYTES) {
                Ok(Some(_)) => FolderProbe::ExistingBundle {
                    devices: io.list().map_or(0, |l| l.devices.len()),
                },
                Ok(None) => FolderProbe::Incomplete,
                Err(e) => FolderProbe::Refused {
                    summary: crate::Error::from(e).summary(),
                },
            },
        }
    }

    /// The canonical root for a new sync in `folder` (refused when sync is
    /// already set up, or the folder holds a bundle).
    pub(crate) fn root_for_create(&self, folder: &Path) -> crate::Result<PathBuf> {
        if state::is_set_up(&self.config.paths) {
            return Err(SyncError::AlreadyEnabled.into());
        }
        let root = self.root_of(folder)?;
        let io = self.tuning.opener.open(&root);
        if io.check().is_ok()
            && io
                .read(&FolderFile::Bundle, bundle::MAX_BUNDLE_BYTES)?
                .is_some()
        {
            return Err(SyncError::BundleExists.into());
        }
        Ok(root)
    }

    fn root_of(&self, folder: &Path) -> Result<PathBuf, SyncError> {
        folder::check_root(folder, &self.config.paths)
    }

    /// Writes a bundle created (KDF done) by the caller: keychain first,
    /// then the folder, then the local state, then a first pass.
    pub(crate) fn finish_create(
        &mut self,
        folder: PathBuf,
        root: &Path,
        device: DeviceId,
        text: &str,
        opened: &OpenedBundle,
        remember_key: bool,
    ) -> crate::Result<SyncStatus> {
        if state::is_set_up(&self.config.paths) {
            return Err(SyncError::AlreadyEnabled.into());
        }
        let sync = SyncConfig::new(
            folder,
            root.to_path_buf(),
            &opened.header.bundle_id,
            opened.header.epoch,
            device,
            remember_key,
        );
        if remember_key {
            self.remember_key(&sync, &opened.data_key)?;
        }
        let set_up = (|| -> crate::Result<()> {
            let io = self.tuning.opener.open(root);
            io.init()?;
            io.create(&FolderFile::Bundle, text.as_bytes())?;
            crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
            let name = opened
                .roster
                .devices
                .iter()
                .find(|d| d.id == device)
                .map(|d| d.name.clone())
                .ok_or(SyncError::Corrupt {
                    what: "bundle.toml",
                    reason: "the first device is missing from the roster",
                })?;
            self.write_fresh_replica(&sync, opened, name)?;
            state::save_config(&self.config.paths, &sync)
        })();
        if let Err(e) = set_up {
            self.forget_orphan_key(&sync, remember_key);
            return Err(e);
        }
        self.keys.clear();
        self.keys.put(&sync.bundle_id, sync.epoch, &opened.data_key);
        self.pass()
    }

    /// Removes a data key stored in the keychain for a create or join that
    /// failed before `sync.toml` was written (nothing else would ever
    /// forget it: "disable" needs `sync.toml` to name the entry).
    fn forget_orphan_key(&self, sync: &SyncConfig, remembered: bool) {
        if remembered {
            let _ = self.config.secrets.delete(&data_key_account(sync));
        }
    }

    fn write_fresh_replica(
        &self,
        sync: &SyncConfig,
        bundle: &OpenedBundle,
        name: DeviceName,
    ) -> crate::Result<()> {
        let replica = fresh_replica(name, bundle.header.keyset_generation);
        let binding = Binding {
            bundle_id: bundle.header.bundle_id,
            epoch: bundle.header.epoch,
            keyset_generation: bundle.header.keyset_generation,
            device: sync.device,
            seq: 1,
        };
        crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
        state::save_replica(&self.config.paths, &bundle.keys, &binding, &replica)
    }

    /// `bundle.toml` of a folder, to derive the key from (join, unlock).
    pub(crate) fn read_bundle_text(&self, folder: &Path) -> crate::Result<(PathBuf, String)> {
        let root = self.root_of(folder)?;
        let io = self.tuning.opener.open(&root);
        io.check()?;
        let bytes = io
            .read(&FolderFile::Bundle, bundle::MAX_BUNDLE_BYTES)?
            .ok_or(SyncError::FolderNotPortico)?;
        let text = String::from_utf8(bytes).map_err(|_| SyncError::Corrupt {
            what: "bundle.toml",
            reason: "not UTF-8",
        })?;
        Ok((root, text))
    }

    /// The configured folder's `bundle.toml` (unlock, passphrase change).
    pub(crate) fn configured_bundle_text(&self) -> crate::Result<(SyncConfig, String)> {
        let sync = state::load_config(&self.config.paths)?.ok_or(SyncError::Disabled)?;
        let io = self.open_folder(&sync)?;
        let bytes = io
            .read(&FolderFile::Bundle, bundle::MAX_BUNDLE_BYTES)?
            .ok_or(SyncError::FolderNotPortico)?;
        let text = String::from_utf8(bytes).map_err(|_| SyncError::Corrupt {
            what: "bundle.toml",
            reason: "not UTF-8",
        })?;
        Ok((sync, text))
    }

    /// Computes what joining brings, without writing anything (F14), and
    /// keeps the data key under a single-use token.
    /// What joining would bring: records per kind, and how many differ
    /// from this device's own.
    fn count_preview(
        preview: &mut JoinPreview,
        projection: &super::model::Projection,
        view: &LocalView,
    ) {
        for (key, siblings) in &projection.known {
            let live: Vec<&TypedSibling> = siblings
                .iter()
                .filter(|s| !s.value.is_tombstone())
                .collect();
            if live.is_empty() {
                continue;
            }
            match key {
                RecordKey::Host { .. } => preview.hosts = preview.hosts.saturating_add(1),
                RecordKey::HostMeta { .. } => {
                    preview.host_meta = preview.host_meta.saturating_add(1);
                }
                RecordKey::Snippet { .. } => preview.snippets = preview.snippets.saturating_add(1),
                RecordKey::Setting { .. } => preview.settings = preview.settings.saturating_add(1),
                RecordKey::Secret { .. } => {}
            }
            if let Some(local) = view.records.get(key)
                && (siblings.len() > 1 || live.iter().all(|s| s.value != *local))
            {
                preview.conflicts = preview.conflicts.saturating_add(1);
            }
        }
    }

    /// This device held `bundle_id` before (its history keeps a copy):
    /// joining it again is a re-join (FAIBLE-5 of the S2a review).
    fn held_before(&self, bundle_id: &[u8; crypto::ID_LEN]) -> bool {
        state::load_history(&self.config.paths)
            .unwrap_or_default()
            .iter()
            .any(|(_, _, text)| bundle::peek(text).is_ok_and(|c| c.bundle_id == *bundle_id))
    }

    pub(crate) fn join_preview(
        &mut self,
        folder: PathBuf,
        root: PathBuf,
        opened: &OpenedBundle,
        device_name: DeviceName,
        remember_key: bool,
    ) -> crate::Result<JoinPreview> {
        if state::is_set_up(&self.config.paths) {
            return Err(SyncError::AlreadyEnabled.into());
        }
        let io = self.tuning.opener.open(&root);
        let listing = io.list()?;
        Self::refuse_put_back(io.as_ref(), &listing, opened.header.epoch)?;
        let mut merged = SyncState::default();
        let mut devices = Vec::new();
        let mut budget = PASS_BYTE_BUDGET;
        let joinable = |(device, _): &(&DeviceId, _)| opened.roster.revoked_entry(device).is_none();
        for (device, candidates) in listing.devices.iter().filter(joinable) {
            let expected = Self::expected(opened, *device);
            let mut best: Option<Opened<SnapshotContent>> = None;
            for candidate in candidates {
                if candidate.size > budget {
                    continue;
                }
                budget = budget.saturating_sub(candidate.size);
                let Ok(Some(bytes)) = io.read(&candidate.file, MAX_CONTAINER_FILE_BYTES) else {
                    continue;
                };
                if let Ok(snapshot) = container::open_snapshot(&opened.keys, &expected, &bytes)
                    && best
                        .as_ref()
                        .is_none_or(|b| snapshot.binding.seq > b.binding.seq)
                {
                    best = Some(snapshot);
                }
            }
            if let Some(snapshot) = best {
                let _ = merged.merge(&snapshot.content.state, device);
                devices.push(SyncDevice {
                    id: device.to_string(),
                    name: snapshot.content.device_name.as_str().to_owned(),
                    label: snapshot.content.device_name.label(device),
                    this_device: false,
                    seq: snapshot.binding.seq,
                    published_at: Some(snapshot.content.published_at),
                    last_seen: None,
                    stale: false,
                    revoked: false,
                });
            }
        }
        let projection = merged.project();
        let view = project::observe(&self.config.paths, &self.snippets);
        let mut preview = JoinPreview {
            token: String::new(),
            devices,
            hosts: 0,
            host_meta: 0,
            snippets: 0,
            settings: 0,
            conflicts: 0,
            needs_newer: projection.held.len(),
            rejoin: self.held_before(&opened.header.bundle_id),
        };
        Self::count_preview(&mut preview, &projection, &view);
        self.purge_expired_joins();
        let token = state::hex(&crypto::random_array::<16>()?);
        self.joins.insert(
            token.clone(),
            PendingJoin {
                folder,
                root,
                bundle_id: opened.header.bundle_id,
                epoch: opened.header.epoch,
                keyset_generation: opened.header.keyset_generation,
                data_key: DataKey::from_bytes(opened.data_key.bytes())?,
                device_name,
                remember_key,
                expires: Instant::now()
                    .checked_add(self.tuning.join_ttl)
                    .unwrap_or_else(Instant::now),
                rejoin: preview.rejoin,
            },
        );
        preview.token = token;
        Ok(preview)
    }

    /// R2 (§13.3.6): a device file newer than `bundle.toml` means the
    /// bundle was put back: a new device never joins it.
    fn refuse_put_back(io: &dyn FolderIo, listing: &Listing, epoch: u32) -> Result<(), SyncError> {
        for candidates in listing.devices.values() {
            for candidate in candidates {
                let Ok(Some(bytes)) = io.read(&candidate.file, MAX_CONTAINER_FILE_BYTES) else {
                    continue;
                };
                if container::peek(container::ContainerKind::Snapshot, &bytes)
                    .is_some_and(|envelope| envelope.epoch > epoch)
                {
                    return Err(SyncError::Rollback {
                        what: "bundle.toml".to_owned(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Joins (single-use token): keychain, local state, first pass, then
    /// the bulk choice as explicit resolutions. Received hosts stay
    /// pending whatever the choice (F1).
    pub(crate) fn join_commit(
        &mut self,
        token: &str,
        bulk: JoinBulkChoice,
    ) -> crate::Result<SyncStatus> {
        // Taken out first: single use even when what follows fails.
        let join = self.joins.remove(token).ok_or(SyncError::JoinExpired)?;
        if join.expires <= Instant::now() {
            return Err(SyncError::JoinExpired.into());
        }
        if state::is_set_up(&self.config.paths) {
            return Err(SyncError::AlreadyEnabled.into());
        }
        if join.rejoin && bulk != JoinBulkChoice::ReviewEach {
            return Err(SyncError::Resolution {
                reason: "a device that joins again reviews each difference",
            }
            .into());
        }
        let device = DeviceId::generate()?;
        let sync = SyncConfig::new(
            join.folder.clone(),
            join.root.clone(),
            &join.bundle_id,
            join.epoch,
            device,
            join.remember_key,
        );
        if join.remember_key {
            self.remember_key(&sync, &join.data_key)?;
        }
        let set_up = (|| -> crate::Result<()> {
            let keys = crypto::derive_subkeys(&join.data_key, &join.bundle_id, join.epoch)?;
            let mut replica = fresh_replica(join.device_name.clone(), join.keyset_generation);
            if join.rejoin {
                // Every local record is of unknown origin until it matches
                // the synced value or the user decides (RR4).
                let view = project::observe(&self.config.paths, &self.snippets);
                for key in view.records.keys() {
                    if let Ok(wire) = WireKey::encode(key) {
                        replica.local.held_values.push((
                            wire,
                            state::HeldValue {
                                from: None,
                                label: None,
                            },
                        ));
                    }
                }
            }
            let binding = Binding {
                bundle_id: join.bundle_id,
                epoch: join.epoch,
                keyset_generation: join.keyset_generation,
                device,
                seq: 1,
            };
            crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
            state::save_replica(&self.config.paths, &keys, &binding, &replica)?;
            state::save_config(&self.config.paths, &sync)
        })();
        if let Err(e) = set_up {
            self.forget_orphan_key(&sync, join.remember_key);
            return Err(e);
        }
        self.keys.clear();
        self.keys.put(&sync.bundle_id, sync.epoch, &join.data_key);
        let status = self.pass()?;
        if bulk == JoinBulkChoice::ReviewEach {
            return Ok(status);
        }
        let choices: Vec<(String, SyncResolution, String)> = self
            .report
            .conflicts
            .iter()
            .filter_map(|conflict| {
                let side = match bulk {
                    JoinBulkChoice::PreferLocal => conflict.sides.iter().find(|s| s.local),
                    _ => conflict.sides.iter().find(|s| !s.local),
                }?;
                Some((
                    conflict.id.clone(),
                    SyncResolution::Pick { index: side.index },
                    conflict.sides_hash.clone(),
                ))
            })
            .collect();
        for (id, resolution, sides_hash) in choices {
            self.record_resolution(&id, &resolution, &sides_hash)?;
        }
        self.pass()
    }

    // ── Changes of the state outside a pass ─────────────────────────────

    /// Runs `edit` on a loaded session (under `sync.lock`), saves the
    /// replica, then runs a pass.
    fn with_session<T>(
        &mut self,
        edit: impl FnOnce(&mut Session) -> crate::Result<T>,
    ) -> crate::Result<T> {
        self.with_session_core(|_, session| edit(session))
    }

    /// [`Self::with_session`] whose edit also sees the core (roster
    /// writes).
    fn with_session_core<T>(
        &mut self,
        edit: impl FnOnce(&Self, &mut Session) -> crate::Result<T>,
    ) -> crate::Result<T> {
        let out = {
            crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
            let _lock = state::lock(&self.config.paths)?;
            let mut alerts = Vec::new();
            let mut session = self.open_session(&mut alerts)?;
            let out = edit(self, &mut session)?;
            self.save_replica(&mut session)?;
            state::save_config(&self.config.paths, &session.sync)?;
            out
        };
        let _ = self.pass();
        Ok(out)
    }

    fn record_resolution(
        &mut self,
        id: &str,
        resolution: &SyncResolution,
        sides_hash: &str,
    ) -> crate::Result<()> {
        let key = key_of_conflict(id).ok_or(SyncError::Resolution {
            reason: "no such conflict",
        })?;
        let snippets = Arc::clone(&self.snippets);
        let paths = self.config.paths.clone();
        self.with_session(|session| {
            // The sides the user saw must still be the sides (A5).
            let shown = match session.replica.state.view(&key) {
                Some(RegisterView::Known(siblings)) if siblings.len() > 1 => {
                    apply::sides_hash(&siblings) == sides_hash
                }
                _ => false,
            };
            if !shown {
                return Err(SyncError::NotPending {
                    alias: id.to_owned(),
                }
                .into());
            }
            let at = now();
            let model = match resolution {
                SyncResolution::Pick { index } => Resolution::Pick(*index),
                SyncResolution::Union => Resolution::Union,
                SyncResolution::KeepBoth { keep } => {
                    let RecordKey::Snippet { id } = &key else {
                        return Err(SyncError::Resolution {
                            reason: "keep both only applies to snippets",
                        }
                        .into());
                    };
                    let Some(RegisterView::Known(siblings)) = session.replica.state.view(&key)
                    else {
                        return Err(SyncError::Resolution {
                            reason: "no such conflict",
                        }
                        .into());
                    };
                    // The other sides become new local snippets (synced as
                    // such by the next pass). A side no dot of this device
                    // wrote was received: its copy is gated (F8) before it
                    // is written, like any received snippet.
                    let me = session.sync.device;
                    for (index, sibling) in siblings.iter().enumerate() {
                        if index == *keep {
                            continue;
                        }
                        if let RecordValue::Snippet(value) = &sibling.value {
                            let mut copy = project::snippet_from("", value);
                            copy.name = format!("{} ({})", value.name, index.saturating_add(1));
                            let received = !sibling.dots.iter().any(|dot| dot.device == me);
                            snippets.save_guarded(copy, |stored| {
                                if received {
                                    state::edit_confirmations(&paths, |c| {
                                        c.snippets.insert(
                                            stored.id.clone(),
                                            apply::snippet_hash(&stored.command, stored.run),
                                        );
                                    })?;
                                }
                                Ok(())
                            })?;
                        }
                    }
                    let _ = id;
                    Resolution::Pick(*keep)
                }
            };
            session
                .replica
                .state
                .resolve(&mut session.clock, &key, &model, at)?;
            Ok(())
        })
    }

    pub(crate) fn resolve(
        &mut self,
        id: &str,
        resolution: &SyncResolution,
        sides_hash: &str,
    ) -> crate::Result<SyncStatus> {
        self.record_resolution(id, resolution, sides_hash)?;
        Ok(self.status())
    }

    /// Confirms (or refuses) received host destinations on this device.
    /// Each decision names the destination the user was shown (its hash):
    /// one that changed since — another device published again and a pass
    /// merged it — is refused (`NotPending`) and nothing is recorded.
    pub(crate) fn decide_hosts(
        &mut self,
        decisions: &[(String, String)],
        accept: bool,
    ) -> crate::Result<SyncStatus> {
        let pending = self.report.pending.clone();
        self.with_session(|session| {
            for (alias, shown) in decisions {
                if !pending
                    .iter()
                    .any(|p| &p.alias == alias && &p.destination_hash == shown)
                {
                    return Err(SyncError::NotPending {
                        alias: alias.clone(),
                    }
                    .into());
                }
                let key = RecordKey::Host {
                    alias: alias.clone(),
                };
                let Some(RecordValue::Host(value)) = session.replica.state.value(&key) else {
                    return Err(SyncError::NotPending {
                        alias: alias.clone(),
                    }
                    .into());
                };
                let hash = destination_hash(&value);
                if &hash != shown {
                    return Err(SyncError::NotPending {
                        alias: alias.clone(),
                    }
                    .into());
                }
                let local = &mut session.replica.local;
                if accept {
                    local.rejected.remove(alias);
                    local.confirmed.insert(alias.clone(), hash);
                } else {
                    local.confirmed.remove(alias);
                    local.rejected.insert(alias.clone(), hash);
                }
            }
            Ok(())
        })?;
        Ok(self.status())
    }

    pub(crate) fn skip(&mut self, key: String) -> crate::Result<SyncStatus> {
        self.with_session(|session| {
            session.sync.excluded.insert(key);
            Ok(())
        })?;
        Ok(self.status())
    }

    pub(crate) fn acknowledge(&mut self) -> crate::Result<SyncStatus> {
        self.with_session(|session| {
            session.replica.local.known_keyset_generation = session.bundle.header.keyset_generation;
            Ok(())
        })?;
        Ok(self.status())
    }

    pub(crate) fn rename_device(&mut self, name: DeviceName) -> crate::Result<SyncStatus> {
        self.with_session_core(|core, session| {
            session.replica.device_name = name;
            core.ensure_in_roster(session);
            Ok(())
        })?;
        Ok(self.status())
    }

    pub(crate) fn set_remember(&mut self, remember: bool) -> crate::Result<SyncStatus> {
        let mut sync = state::load_config(&self.config.paths)?.ok_or(SyncError::Disabled)?;
        // The current key and, during a transition, the next one.
        let next = state::load_rotation(&self.config.paths)?
            .filter(|r| r.bundle_id == sync.bundle_id)
            .map(|r| r.next_epoch);
        if remember {
            let key = self.data_key_for(&sync)?;
            self.remember_key(&sync, &key)?;
            if let Some(epoch) = next
                && let Some(key) = self.keys.get(&sync.bundle_id, epoch)
            {
                self.remember_key_at(&sync.bundle_id, epoch, &key)?;
            }
        } else {
            self.config.secrets.delete(&data_key_account(&sync))?;
            if let Some(epoch) = next {
                self.config
                    .secrets
                    .delete(&key_account(&sync.bundle_id, epoch))?;
            }
        }
        sync.remember_key = remember;
        state::save_config(&self.config.paths, &sync)?;
        Ok(self.status())
    }

    /// The opened bundle with the remembered key (passphrase reset).
    pub(crate) fn opened_with_key(&mut self) -> crate::Result<(SyncConfig, String, OpenedBundle)> {
        let (sync, text) = self.configured_bundle_text()?;
        let key = self.data_key_for(&sync)?;
        let opened = bundle::open_with_key(&text, &key)?;
        Ok((sync, text, opened))
    }

    /// Writes a re-wrapped `bundle.toml` (passphrase changed or reset
    /// here), if the file did not change meanwhile.
    pub(crate) fn finish_rewrap(
        &mut self,
        previous: &str,
        text: &str,
        opened: &OpenedBundle,
    ) -> crate::Result<SyncStatus> {
        let (sync, current) = self.configured_bundle_text()?;
        if current != previous {
            return Err(SyncError::Busy.into());
        }
        let io = self.open_folder(&sync)?;
        io.write(&FolderFile::Bundle, text.as_bytes())?;
        let generation = opened.header.keyset_generation;
        self.with_session(|session| {
            session.replica.local.known_keyset_generation = generation;
            Ok(())
        })?;
        Ok(self.status())
    }
}

/// Merges `incoming` (the state of `source`). `secret` registers merge and
/// are relayed **opaque** whatever the bundle's password policy and this
/// device's consent (§13.5.3, RR3, and the S2a review's MOYEN on the
/// filter): "passwords off" only ever means "never decrypted, projected
/// nor applied here" (S2b). Dropping them at the merge while joining the
/// source's causal context would leave this device's context covering
/// dots it does not hold; any device it relays to would then read
/// "covered and absent" and delete the shared passwords, whichever policy
/// view (stale, or closed in doubt) each device has.
pub(crate) fn merge_received(
    state: &mut SyncState,
    incoming: &SyncState,
    source: &DeviceId,
) -> Result<super::model::MergeReport, SyncError> {
    state.merge(incoming, source)
}

/// `true` when the replica could not be read (an alert was pushed).
fn loaded_was_unreadable(alerts: &[Alert]) -> bool {
    alerts.iter().any(|a| a.kind == AlertKind::Restored)
}

/// The last applied projection after a pass: for each key, what the files
/// hold now when it is what the state wanted (the pass wrote it, or it
/// already was so), else what they held when the pass observed them (an
/// edit made meanwhile is then seen by the next pass).
fn settle_applied(
    before: &LocalView,
    after: &LocalView,
    projection: &super::model::Projection,
    previous: &BTreeMap<WireKey, WireValue>,
) -> BTreeMap<WireKey, WireValue> {
    let mut applied = project::next_applied(before, previous);
    let keys: BTreeSet<&RecordKey> = before.records.keys().chain(after.records.keys()).collect();
    for key in keys {
        if after.is_suspended(key) || after.unsynced.contains(key) {
            continue;
        }
        let now = after.records.get(key);
        let wanted = projection
            .known
            .get(key)
            .and_then(|siblings| match siblings.as_slice() {
                [one] => Some(&one.value),
                _ => None,
            });
        let settled = match (wanted, now) {
            (Some(RecordValue::Tombstone), None) => true,
            (Some(wanted), Some(now)) => match (wanted, now) {
                (RecordValue::Host(a), RecordValue::Host(b)) => {
                    project::destination(a) == project::destination(b)
                }
                (a, b) => a == b,
            },
            _ => false,
        };
        if !settled {
            continue;
        }
        let Ok(wire) = WireKey::encode(key) else {
            continue;
        };
        match now.and_then(|value| WireValue::encode(value).ok()) {
            Some(value) => {
                applied.insert(wire, value);
            }
            None => {
                applied.remove(&wire);
            }
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::super::clock::{DeviceId, LocalClock};
    use super::super::model::{RecordKey, RecordValue, SealedValue, SyncState};
    use super::merge_received;

    fn device(n: u8) -> DeviceId {
        DeviceId::from_bytes([n; 16])
    }

    fn sealed(n: u8) -> RecordValue {
        RecordValue::Sealed(SealedValue {
            nonce: format!("nonce-{n}"),
            ciphertext: format!("ct-{n}"),
            compare: format!("mac-{n}"),
        })
    }

    fn secret_key(account: u8) -> RecordKey {
        RecordKey::Secret {
            account: format!("password:root@host{account}:22#SHA256:x"),
        }
    }

    struct Node {
        id: DeviceId,
        clock: LocalClock,
        state: SyncState,
        /// This device's view of the policy (on, generation): it never
        /// changes what the merge keeps.
        policy: (bool, u64),
    }

    impl Node {
        fn new(n: u8, policy: (bool, u64)) -> Self {
            Self {
                id: device(n),
                clock: LocalClock::new(device(n), 0),
                state: SyncState::default(),
                policy,
            }
        }

        /// Merges `other` in; returns the values dropped without successor.
        fn pull(&mut self, other: &Node) -> usize {
            merge_received(&mut self.state, &other.state, &other.id)
                .expect("merge")
                .dropped_live_without_successor
        }

        /// Only a device whose own view says "on" writes a password (S2b).
        fn write(&mut self, account: u8, value: u8) -> bool {
            if !self.policy.0 {
                return false;
            }
            self.state
                .write_unvalidated(&mut self.clock, &secret_key(account), &sealed(value), 0)
                .expect("write");
            true
        }
    }

    /// Writes by A or C, every exchange through B, then the last values
    /// must be everywhere and no merge may drop one.
    fn relay_through_b(
        a: &mut Node,
        b: &mut Node,
        c: &mut Node,
        writes: Vec<(bool, u8, u8)>,
    ) -> Result<(), TestCaseError> {
        let mut dropped = 0;
        let mut last: std::collections::BTreeMap<u8, RecordValue> =
            std::collections::BTreeMap::new();
        for (by_a, account, value) in writes {
            let wrote = if by_a {
                a.write(account, value)
            } else {
                c.write(account, value)
            };
            if wrote {
                last.insert(account, sealed(value));
            }
            if by_a {
                dropped += b.pull(a);
                dropped += c.pull(b);
                dropped += b.pull(c);
                dropped += a.pull(b);
            } else {
                dropped += b.pull(c);
                dropped += a.pull(b);
                dropped += b.pull(a);
                dropped += c.pull(b);
            }
        }
        prop_assert_eq!(dropped, 0);
        for (account, value) in &last {
            let key = secret_key(*account);
            prop_assert_eq!(a.state.value(&key), Some(value.clone()));
            prop_assert_eq!(c.state.value(&key), Some(value.clone()));
            prop_assert_eq!(b.state.value(&key), Some(value.clone()), "relayed");
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// RR3: policy on, B never consents (never writes nor applies a
        /// password) but relays the registers opaque. A and C only talk
        /// through B: nothing is lost, nothing dropped.
        #[test]
        fn non_consenting_device_relays_secrets_and_drops_nothing(
            writes in proptest::collection::vec((any::<bool>(), 0u8..4, 0u8..8), 1..24)
        ) {
            let mut a = Node::new(1, (true, 1));
            let mut b = Node::new(2, (true, 1));
            let mut c = Node::new(3, (true, 1));
            relay_through_b(&mut a, &mut b, &mut c, writes)?;
        }

        /// S2a review (MOYEN, S2b-only): B is stuck on another view of the
        /// policy (older generation, or "off", or closed in doubt): where
        /// the policy is on, no shared password is ever lost.
        #[test]
        fn stale_policy_view_never_deletes_shared_passwords(
            writes in proptest::collection::vec((any::<bool>(), 0u8..4, 0u8..8), 1..24),
            b_on in any::<bool>(),
            b_generation in 0u64..3,
        ) {
            let mut a = Node::new(1, (true, 3));
            let mut b = Node::new(2, (b_on, b_generation));
            let mut c = Node::new(3, (true, 3));
            relay_through_b(&mut a, &mut b, &mut c, writes)?;
        }
    }
}
