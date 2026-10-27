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
//!    rosters (revision + 1) so every device converges on one file.
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
}

/// The sync state machine (one per engine, on the sync thread).
#[derive(Debug)]
pub(crate) struct SyncCore {
    config: EngineConfig,
    snippets: Arc<SnippetStore>,
    events: broadcast::Sender<EngineEvent>,
    tuning: SyncTuning,
    data_key: Option<DataKey>,
    joins: BTreeMap<String, PendingJoin>,
    report: Report,
    status: Arc<Mutex<SyncStatus>>,
    conflicts_count: usize,
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
            SyncError::Rollback { .. } => SyncStatus::Alert {
                kind: AlertKind::Rollback,
                device: None,
            },
            SyncError::KeysRotated => SyncStatus::Alert {
                kind: AlertKind::KeysRotatedElsewhere,
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
    SecretKey::SyncDataKey {
        bundle: sync.bundle_id.clone(),
        epoch: sync.epoch,
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
    ) -> Self {
        Self {
            config,
            snippets,
            events,
            tuning,
            data_key: None,
            joins: BTreeMap::new(),
            report: Report::default(),
            status,
            conflicts_count: 0,
        }
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

    /// The data key: in memory, else the keychain (when remembered).
    fn data_key_for(&mut self, sync: &SyncConfig) -> Result<DataKey, SyncError> {
        if let Some(key) = &self.data_key {
            return DataKey::from_bytes(key.bytes());
        }
        if !sync.remember_key {
            return Err(SyncError::Locked);
        }
        let stored = self
            .config
            .secrets
            .get(&data_key_account(sync))
            .map_err(|_| SyncError::Locked)?
            .ok_or(SyncError::Locked)?;
        let bytes = Zeroizing::new(
            B64.decode(stored.expose_secret())
                .map_err(|_| SyncError::Locked)?,
        );
        let key = DataKey::from_bytes(&bytes)?;
        self.data_key = Some(DataKey::from_bytes(key.bytes())?);
        Ok(key)
    }

    fn remember_key(&self, sync: &SyncConfig, key: &DataKey) -> crate::Result<()> {
        let encoded = Zeroizing::new(B64.encode(key.bytes()));
        let secret = SecretString::from(encoded.as_str().to_owned());
        self.config.secrets.set(&data_key_account(sync), &secret)
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

    /// Opens `bundle.toml` (and its provider copies) with the data key;
    /// returns the winner and whether `bundle.toml` itself must be
    /// rewritten (a copy won, or the rosters differ).
    fn open_bundle(
        folder: &dyn FolderIo,
        listing: &Listing,
        key: &DataKey,
        sync: &SyncConfig,
    ) -> Result<(OpenedBundle, Roster, bool), SyncError> {
        let max = bundle::MAX_BUNDLE_BYTES;
        let main_bytes = folder
            .read(&FolderFile::Bundle, max)?
            .ok_or(SyncError::FolderNotPortico)?;
        let main_text = std::str::from_utf8(&main_bytes).map_err(|_| SyncError::Corrupt {
            what: "bundle.toml",
            reason: "not UTF-8",
        })?;
        let main = bundle::open_with_key(main_text, key)?;
        let rank = |b: &OpenedBundle| {
            (
                b.header.epoch,
                b.header.keyset_generation,
                b.header.revision,
                std::cmp::Reverse(b.fingerprint),
            )
        };
        let mut union: BTreeMap<DeviceId, RosterDevice> = main
            .roster
            .devices
            .iter()
            .map(|d| (d.id, d.clone()))
            .collect();
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
            if opened.header.bundle_id != best.header.bundle_id {
                continue;
            }
            for device in &opened.roster.devices {
                union.entry(device.id).or_insert_with(|| device.clone());
            }
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
        let roster = Roster {
            devices: union.into_values().collect(),
        };
        let roster_differs = roster.devices.len() != best.roster.devices.len();
        Ok((best, roster, copy_won || roster_differs))
    }

    fn expected(bundle: &OpenedBundle, device: DeviceId) -> Expected {
        Expected {
            bundle_id: bundle.header.bundle_id,
            epoch: bundle.header.epoch,
            max_keyset_generation: bundle.header.keyset_generation,
            device,
        }
    }

    /// Loads everything a pass needs (steps 1–3 of the module doc). The
    /// caller holds `sync.lock`.
    fn open_session(&mut self, alerts: &mut Vec<Alert>) -> crate::Result<Session> {
        let paths = self.config.paths.clone();
        let paths = &paths;
        let sync = state::load_config(paths)?.ok_or(SyncError::Disabled)?;
        let key = self.data_key_for(&sync)?;
        let folder = self.open_folder(&sync)?;
        let listing = folder.list()?;
        let (bundle, union_roster, rewrite) =
            Self::open_bundle(folder.as_ref(), &listing, &key, &sync)?;
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
        };
        if unreadable {
            self.rejoin(&mut session)?;
        }
        // F4: never an older bundle; a diverged one is rewritten below.
        let header = session.bundle.header.clone();
        let observed = session.replica.high_water.observe_bundle(
            header.epoch,
            header.keyset_generation,
            header.revision,
            &session.bundle.fingerprint,
        )?;
        session
            .replica
            .high_water
            .ratchet_format("bundle.toml", bundle::BUNDLE_VERSION)?;
        if rewrite || observed == BundleObservation::Diverged {
            Self::write_roster(&mut session, union_roster);
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
        Ok(session)
    }

    /// Rewrites `bundle.toml` with `roster` (revision + 1, data key only),
    /// adding or renaming this device. A failure is retried next pass.
    fn write_roster(session: &mut Session, mut roster: Roster) {
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
        roster.devices.sort_by_key(|d| d.id);
        let Ok((text, opened)) = bundle::with_roster(&session.bundle, roster) else {
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
        if session
            .replica
            .high_water
            .observe_bundle(
                header.epoch,
                header.keyset_generation,
                header.revision,
                &opened.fingerprint,
            )
            .is_ok()
        {
            session.bundle = opened;
        }
    }

    /// Makes sure this device is in the roster under its current name.
    fn ensure_in_roster(session: &mut Session) {
        let me = session.sync.device;
        let ok = session
            .bundle
            .roster
            .devices
            .iter()
            .any(|d| d.id == me && d.name == session.replica.device_name);
        if !ok {
            let roster = session.bundle.roster.clone();
            Self::write_roster(session, roster);
        }
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
        Self::ensure_in_roster(session);
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
    /// candidates. Unauthenticated copies (torn, uploading) are skipped.
    fn best_snapshot(
        session: &mut Session,
        device: DeviceId,
        partial: &mut bool,
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
            self.data_key = None;
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

        // ── This device's own file (F5), before any write ──
        let mut partial = false;
        let own = Self::best_snapshot(&mut session, me, &mut partial, &mut issues);
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
        match own_check {
            Ok(OwnFile::InSync) => {}
            Ok(OwnFile::Behind) => force_publish = true,
            Err(e @ (SyncError::RestoredState { .. } | SyncError::Fork { .. })) => {
                alerts.push(Alert {
                    kind: AlertKind::Restored,
                    device: None,
                    error: e,
                });
                // What our old identity published is merged like any
                // device's snapshot (it is authenticated).
                if let Some((_, opened)) = &own {
                    let _ = session.replica.state.merge(&opened.content.state, &me);
                }
                self.rejoin(&mut session)?;
                force_publish = true;
            }
            Err(e) => return Err(e.into()),
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
        Self::ensure_in_roster(&mut session);

        // ── Every other device, observed before any context is judged ──
        let roster: BTreeSet<DeviceId> = session
            .bundle
            .roster
            .devices
            .iter()
            .filter(|d| !d.revoked)
            .map(|d| d.id)
            .collect();
        let others: Vec<DeviceId> = session
            .listing
            .devices
            .keys()
            .copied()
            .filter(|d| *d != me)
            .collect();
        let mut received: Vec<(DeviceId, Opened<SnapshotContent>)> = Vec::new();
        let mut present: BTreeMap<DeviceId, u64> = BTreeMap::new();
        let mut seen_vectors: Vec<BTreeMap<DeviceId, u64>> = Vec::new();
        for device in others {
            let Some((_, opened)) =
                Self::best_snapshot(&mut session, device, &mut partial, &mut issues)
            else {
                continue;
            };
            present.insert(device, opened.binding.seq);
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
        // Devices whose newer snapshot others merged but which is not in
        // the folder (yet): withheld or still uploading.
        let mut waiting: Option<(WaitReason, Option<String>)> = None;
        for withheld in guard::withheld(&present, seen_vectors.iter()) {
            if withheld.device == me {
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
        let mut accepted = Vec::new();
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
            match guard::check_context_claims(&device, context, &roster, &verified) {
                ClaimCheck::Accept => accepted.push((device, opened)),
                ClaimCheck::Defer(_) => {
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
        let view = project::observe(&self.config.paths, &self.snippets);
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
        let mut edited_snippets: Vec<String> = Vec::new();
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
                    if let RecordKey::Snippet { id } = &change.key {
                        edited_snippets.push(id.clone());
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
            match session.replica.state.merge(&opened.content.state, &device) {
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
        // ── Durable before anything is applied or published ──
        self.save_replica(&mut session)?;

        // ── Validated application ──
        let projection = session.replica.state.project();
        let outcome = apply::apply(&ApplyInput {
            config: &self.config,
            snippets: &self.snippets,
            projection: &projection,
            view: &view,
            excluded: &session.sync.excluded,
            confirmed: &session.replica.local.confirmed,
            rejected: &session.replica.local.rejected,
        });
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
            // yet: refused, conflict elsewhere) keeps its tombstone.
            let present: BTreeSet<WireKey> = after
                .records
                .keys()
                .filter_map(|key| WireKey::encode(key).ok())
                .collect();
            session
                .replica
                .state
                .collect_tombstones_except(&stable, |key| present.contains(key));
        }

        if !outcome.received_snippets.is_empty() || !edited_snippets.is_empty() {
            let received = outcome.received_snippets.clone();
            state::edit_confirmations(&self.config.paths, |c| {
                for id in &edited_snippets {
                    c.snippets.remove(id);
                }
                for (id, hash) in received {
                    c.snippets.insert(id, hash);
                }
            })?;
        }
        issues.extend(outcome.issues.iter().cloned());
        self.save_replica(&mut session)?;

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
        let report = Self::build_report(&session, issues, alerts, &outcome.pending, waiting, at);
        let status = Self::status_of(&report, at);
        let count = report.conflicts.len();
        self.report = report;
        if count != self.conflicts_count {
            self.conflicts_count = count;
            self.emit(EngineEvent::SyncConflicts { count });
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

    fn build_report(
        session: &Session,
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
        Report {
            issues,
            alerts,
            pending,
            conflicts,
            devices,
            waiting,
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
        state::save_config(&self.config.paths, &sync)?;
        self.data_key = Some(DataKey::from_bytes(opened.data_key.bytes())?);
        self.pass()
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
        let mut merged = SyncState::default();
        let mut devices = Vec::new();
        let mut budget = PASS_BYTE_BUDGET;
        for (device, candidates) in &listing.devices {
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
        };
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
        self.joins.retain(|_, join| join.expires > Instant::now());
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
            },
        );
        preview.token = token;
        Ok(preview)
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
        let keys = crypto::derive_subkeys(&join.data_key, &join.bundle_id, join.epoch)?;
        let replica = fresh_replica(join.device_name.clone(), join.keyset_generation);
        let binding = Binding {
            bundle_id: join.bundle_id,
            epoch: join.epoch,
            keyset_generation: join.keyset_generation,
            device,
            seq: 1,
        };
        crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
        state::save_replica(&self.config.paths, &keys, &binding, &replica)?;
        state::save_config(&self.config.paths, &sync)?;
        self.data_key = Some(join.data_key);
        let status = self.pass()?;
        if bulk == JoinBulkChoice::ReviewEach {
            return Ok(status);
        }
        let choices: Vec<(String, SyncResolution)> = self
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
                ))
            })
            .collect();
        for (id, resolution) in choices {
            self.record_resolution(&id, &resolution)?;
        }
        self.pass()
    }

    /// Unlock with a data key derived by the caller.
    pub(crate) fn finish_unlock(
        &mut self,
        sync: &SyncConfig,
        opened: &OpenedBundle,
    ) -> crate::Result<SyncStatus> {
        if opened.header.bundle_id != sync.bundle_bytes()? {
            return Err(SyncError::BundleTampered.into());
        }
        if sync.remember_key {
            self.remember_key(sync, &opened.data_key)?;
        }
        self.data_key = Some(DataKey::from_bytes(opened.data_key.bytes())?);
        self.pass()
    }

    // ── Changes of the state outside a pass ─────────────────────────────

    /// Runs `edit` on a loaded session (under `sync.lock`), saves the
    /// replica, then runs a pass.
    fn with_session<T>(
        &mut self,
        edit: impl FnOnce(&mut Session) -> crate::Result<T>,
    ) -> crate::Result<T> {
        let out = {
            crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
            let _lock = state::lock(&self.config.paths)?;
            let mut alerts = Vec::new();
            let mut session = self.open_session(&mut alerts)?;
            let out = edit(&mut session)?;
            self.save_replica(&mut session)?;
            state::save_config(&self.config.paths, &session.sync)?;
            out
        };
        let _ = self.pass();
        Ok(out)
    }

    fn record_resolution(&mut self, id: &str, resolution: &SyncResolution) -> crate::Result<()> {
        let key = key_of_conflict(id).ok_or(SyncError::Resolution {
            reason: "no such conflict",
        })?;
        let snippets = Arc::clone(&self.snippets);
        self.with_session(|session| {
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
                    // such by the next pass).
                    for (index, sibling) in siblings.iter().enumerate() {
                        if index == *keep {
                            continue;
                        }
                        if let RecordValue::Snippet(value) = &sibling.value {
                            let mut copy = project::snippet_from("", value);
                            copy.name = format!("{} ({})", value.name, index.saturating_add(1));
                            snippets.save(copy)?;
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
    ) -> crate::Result<SyncStatus> {
        self.record_resolution(id, resolution)?;
        Ok(self.status())
    }

    /// Confirms (or refuses) received host destinations on this device.
    pub(crate) fn decide_hosts(
        &mut self,
        aliases: &[String],
        accept: bool,
    ) -> crate::Result<SyncStatus> {
        let pending = self.report.pending.clone();
        self.with_session(|session| {
            for alias in aliases {
                if !pending.iter().any(|p| &p.alias == alias) {
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
        self.with_session(|session| {
            session.replica.device_name = name;
            Self::ensure_in_roster(session);
            Ok(())
        })?;
        Ok(self.status())
    }

    pub(crate) fn set_remember(&mut self, remember: bool) -> crate::Result<SyncStatus> {
        let mut sync = state::load_config(&self.config.paths)?.ok_or(SyncError::Disabled)?;
        if remember {
            let key = self.data_key_for(&sync)?;
            self.remember_key(&sync, &key)?;
        } else {
            self.config.secrets.delete(&data_key_account(&sync))?;
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

    /// Disables sync on this device; local data is kept.
    pub(crate) fn disable(
        &mut self,
        forget_key: bool,
        remove_my_file: bool,
    ) -> crate::Result<SyncStatus> {
        if let Some(sync) = state::load_config(&self.config.paths)? {
            let _lock = state::lock(&self.config.paths)?;
            if remove_my_file && let Ok(io) = self.open_folder(&sync) {
                let _ = io.remove(&FolderFile::primary(&sync.device));
            }
            if forget_key {
                self.config.secrets.delete(&data_key_account(&sync))?;
            }
            state::remove_local_state(&self.config.paths)?;
        }
        self.data_key = None;
        self.joins.clear();
        self.report = Report::default();
        self.set_status(SyncStatus::Disabled);
        Ok(SyncStatus::Disabled)
    }
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
