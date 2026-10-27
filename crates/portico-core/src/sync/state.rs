//! This device's sync state in `data_dir/sync/` (S1b, `docs/SYNC.md`
//! §3.4, §5.1, security review F5).
//!
//! | File | Content | Protection |
//! |---|---|---|
//! | `sync.toml` | folder (as chosen and canonical), bundle id, epoch, device id, "remember the key", skipped records | plain TOML, `0600`, **no secret** |
//! | `replica.snap` | name, dot counter, publish sequence, merged state, high-water marks, last applied projection, host confirmations, deferred deletions | sealed under the local subkey, one atomic file (F5) |
//! | `confirm.toml` | snippets changed by sync and not yet confirmed (id → hash of the received command) | plain TOML: the engine refuses to run them (F8) even while sync is locked **or disabled**. An execution gate, not sync state: written **before** the snippet it guards, never removed by "disable"; an entry leaves only through a confirmation of that exact content or the deletion of the snippet |
//! | `sync.lock` | inter-process lock, from projection to publication (§4.9) | — |
//! | `rotation.toml` (S2a) | `next_epoch` of a rotation or adoption in progress: the local epoch floor is `max(sync.toml.epoch, next_epoch)` | plain TOML, `0600`, no secret; present only during a transition |
//! | `bundle.verified` (S2a) | exact copy of the last `bundle.toml` this device verified or wrote | `0600`, no secret (already in the folder) |
//! | `bundle.next` (S2a) | the bundle of the epoch being adopted or rotated to | `0600`, no secret; only during a transition |
//! | `bundle.history/` (S2a) | the last 8 distinct passphrase slots verified here (whole verified files: enough to replay an unwrap, RR1) | `0600`, no secret (already in the folder and its provider history) |
//! | `commitment.toml` (S2a, E1) | `C_E` of the current epoch, computed from the data key at every open (v1 included) | plain TOML, `0600`, not secret |
//!
//! `sync.toml` keeps its S1 shape (a 0.6 device still reads it,
//! `deny_unknown_fields`): its `epoch` is the **local epoch floor** (R2);
//! the S2a state lives in the new files above and in additive fields of
//! the sealed replica, never in `HighWater` (`deny_unknown_fields` too).
//!
//! The data dir is local (`%LOCALAPPDATA%` on Windows): the sync identity
//! never roams with a profile (case E4). Everything here is blocking.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::SyncError;
use super::clock::{DeviceId, Dot, VersionVector};
use super::container::{self, Binding, Expected, MAX_CONTAINER_FILE_BYTES, ReplicaContent};
use super::crypto::{ID_LEN, KeyBytes, MAC_LEN, Subkeys};
use super::model::{WireKey, WireValue};
use crate::error::Result;
use crate::store::{self, Paths};

const CONFIG_FILE: &str = "sync.toml";
const REPLICA_FILE: &str = "replica.snap";
const CONFIRM_FILE: &str = "confirm.toml";
const LOCK_FILE: &str = "sync.lock";
const ROTATION_FILE: &str = "rotation.toml";
const VERIFIED_FILE: &str = "bundle.verified";
const NEXT_FILE: &str = "bundle.next";
const HISTORY_DIR: &str = "bundle.history";
const COMMITMENT_FILE: &str = "commitment.toml";
/// Distinct passphrase slots kept in `bundle.history/` (RR1).
pub(crate) const HISTORY_SLOTS: usize = 8;
/// Epochs kept in the replica's lineage memory (concurrent rotations).
pub(crate) const LINEAGE_MEMORY: usize = 16;
/// Largest bundle copy kept locally (`bundle.toml`'s own cap).
const MAX_BUNDLE_FILE_BYTES: u64 = 512 * 1024;
/// Largest `sync.toml` / `confirm.toml`.
const MAX_SMALL_FILE_BYTES: u64 = 256 * 1024;
/// Current `sync.toml` version.
const CONFIG_VERSION: u32 = 1;

/// `sync.toml`: what this device needs to find its sync (no secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncConfig {
    pub(crate) version: u32,
    /// The folder as the user chose it (shown in the UIs).
    pub(crate) folder: PathBuf,
    /// Its canonical form at set-up: re-checked on every pass (F9).
    pub(crate) root: PathBuf,
    /// Bundle id, 32 hex characters.
    pub(crate) bundle_id: String,
    pub(crate) epoch: u32,
    pub(crate) device: DeviceId,
    /// The data key is in the system keychain (default) or only in
    /// memory (shared machines: "Locked" at each start).
    pub(crate) remember_key: bool,
    /// Records skipped on this device ("skip on this device"): issue keys.
    #[serde(default)]
    pub(crate) excluded: BTreeSet<String>,
}

impl SyncConfig {
    pub(crate) fn new(
        folder: PathBuf,
        root: PathBuf,
        bundle_id: &[u8; ID_LEN],
        epoch: u32,
        device: DeviceId,
        remember_key: bool,
    ) -> Self {
        Self {
            version: CONFIG_VERSION,
            folder,
            root,
            bundle_id: DeviceId::from_bytes(*bundle_id).to_string(),
            epoch,
            device,
            remember_key,
            excluded: BTreeSet::new(),
        }
    }

    /// The bundle id as bytes.
    pub(crate) fn bundle_bytes(&self) -> Result<[u8; ID_LEN], SyncError> {
        DeviceId::from_hex(&self.bundle_id)
            .map(|id| *id.bytes())
            .ok_or(SyncError::Corrupt {
                what: "sync.toml",
                reason: "bad bundle id",
            })
    }
}

/// What the engine keeps inside the sealed replica besides the S1a fields
/// (all additive, `serde(default)`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LocalReplica {
    /// Publish sequence of each device's snapshot merged into the state
    /// (the `seen` vector this device publishes).
    pub(crate) seen: BTreeMap<DeviceId, u64>,
    /// Causal context of each device's last merged snapshot (tombstone
    /// collection: the stable context is their meet).
    pub(crate) peer_contexts: BTreeMap<DeviceId, VersionVector>,
    /// The last projection of the local files: key → value as the files
    /// held it after the last pass (F7: only the observed deletion of a
    /// value that was really there yields a tombstone).
    pub(crate) applied: Vec<(WireKey, WireValue)>,
    /// Hosts whose received destination the user confirmed on this
    /// device: alias → hash of the confirmed destination (F1).
    pub(crate) confirmed: BTreeMap<String, String>,
    /// Hosts whose received destination the user refused on this device:
    /// alias → hash of the refused destination (kept out of `hosts.conf`
    /// until another destination arrives).
    pub(crate) rejected: BTreeMap<String, String>,
    /// Fingerprint (hex) of this device's last published snapshot
    /// (`guard::check_own_file`).
    pub(crate) published_fingerprint: Option<String>,
    /// SHA-256 (hex) of the state last published: an unchanged state is
    /// not republished.
    pub(crate) published_state: Option<String>,
    /// Keyset generation this device made or acknowledged: a higher one
    /// is an alert ("passphrase changed or reset elsewhere").
    pub(crate) known_keyset_generation: u64,
    /// Received deletions this device could not apply yet (its file is
    /// suspended, or its own entry cannot be synced). The other devices
    /// may collect the tombstone meanwhile (their stable context only says
    /// this device **saw** it): kept here, applied once the file is
    /// readable, unless the key holds a value again by then.
    pub(crate) deferred_deletions: Vec<DeferredDeletion>,

    // ── S2a (all additive: a 0.6 device ignores them) ───────────────────
    /// The epoch this replica was (re)sealed in by a rotation or adoption.
    pub(crate) epoch_created_here: u32,
    /// `(e, C_e, K_rot_e)` of the last epochs this device held (newest
    /// last, at most [`LINEAGE_MEMORY`]): judging another lineage after
    /// concurrent rotations (§13.3.7).
    pub(crate) lineage: Vec<RetainedEpoch>,
    /// `revoked_clock` of every forgotten device as first observed: the
    /// value is immutable once seen (§13.4.6).
    pub(crate) revoked_seen: BTreeMap<DeviceId, u64>,
    /// Highest password policy generation seen (ratchet, R8).
    pub(crate) policy_generation: u64,
    /// Local values held out of sync until the user decides (R3, RR4):
    /// written by a forgotten device, or of unknown origin (one entry per
    /// key; a list, as record keys are JSON objects).
    pub(crate) held_values: Vec<(WireKey, HeldValue)>,
    /// Former ids of this device to move to the forgotten list with their
    /// last published clock (a re-join with provenance, §13.3.5).
    pub(crate) retire: BTreeMap<DeviceId, u64>,
    /// `app_version` of each device's last verified snapshot (Q5).
    pub(crate) peer_versions: BTreeMap<DeviceId, String>,
}

/// One epoch of the lineage memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetainedEpoch {
    pub(crate) epoch: u32,
    /// `C_e`, hex.
    pub(crate) commit: String,
    /// `K_rot_e` (redacted `Debug`, zeroized).
    pub(crate) rotate: KeyBytes,
}

/// Why a local value is held out of sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HeldValue {
    /// The forgotten device that wrote it; `None`: unknown origin (RR4).
    pub(crate) from: Option<DeviceId>,
    /// Its label when it was held (display).
    pub(crate) label: Option<String>,
}

/// A received deletion not applied yet on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeferredDeletion {
    pub(crate) key: WireKey,
    /// The tombstone's dots (never empty) and display stamp.
    pub(crate) dots: Vec<Dot>,
    pub(crate) at: i64,
}

impl LocalReplica {
    pub(crate) fn applied_map(&self) -> BTreeMap<WireKey, WireValue> {
        self.applied.iter().cloned().collect()
    }

    pub(crate) fn set_applied(&mut self, applied: BTreeMap<WireKey, WireValue>) {
        self.applied = applied.into_iter().collect();
    }

    /// Records `(epoch, C_e, K_rot_e)` in the lineage memory (idempotent,
    /// the newest [`LINEAGE_MEMORY`] kept).
    pub(crate) fn remember_epoch(&mut self, epoch: u32, commit: &[u8; MAC_LEN], rotate: KeyBytes) {
        let commit = hex(commit);
        self.lineage.retain(|retained| retained.epoch != epoch);
        self.lineage.push(RetainedEpoch {
            epoch,
            commit,
            rotate,
        });
        self.lineage.sort_by_key(|retained| retained.epoch);
        let excess = self.lineage.len().saturating_sub(LINEAGE_MEMORY);
        self.lineage.drain(..excess);
    }
}

impl RetainedEpoch {
    pub(crate) fn commit_bytes(&self) -> Option<[u8; MAC_LEN]> {
        unhex32(&self.commit)
    }
}

/// 64 hex characters → 32 bytes.
pub(crate) fn unhex32(text: &str) -> Option<[u8; 32]> {
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

fn config_path(paths: &Paths) -> PathBuf {
    paths.sync_dir().join(CONFIG_FILE)
}

fn replica_path(paths: &Paths) -> PathBuf {
    paths.sync_dir().join(REPLICA_FILE)
}

fn confirm_path(paths: &Paths) -> PathBuf {
    paths.sync_dir().join(CONFIRM_FILE)
}

/// `sync.toml`, `None` when sync is not set up.
pub(crate) fn load_config(paths: &Paths) -> Result<Option<SyncConfig>> {
    let path = config_path(paths);
    match store::read_capped(&path, MAX_SMALL_FILE_BYTES)? {
        None => Ok(None),
        Some(text) => {
            let config: SyncConfig = store::parse_toml(&path, &text)?;
            if config.version != CONFIG_VERSION {
                return Err(SyncError::UnsupportedVersion {
                    what: "sync.toml",
                    found: config.version,
                    supported: CONFIG_VERSION,
                }
                .into());
            }
            Ok(Some(config))
        }
    }
}

pub(crate) fn save_config(paths: &Paths, config: &SyncConfig) -> Result<()> {
    let text = toml::to_string_pretty(config).map_err(|e| crate::Error::Serde {
        what: "toml",
        message: e.to_string(),
    })?;
    store::write_atomic_capped(&config_path(paths), text.as_bytes(), MAX_SMALL_FILE_BYTES)
}

/// The sealed replica of `device` (`None`: never written).
pub(crate) fn load_replica(
    paths: &Paths,
    keys: &Subkeys,
    expected: &Expected,
) -> Result<Option<container::Opened<ReplicaContent>>> {
    let path = replica_path(paths);
    let max = u64::try_from(MAX_CONTAINER_FILE_BYTES).unwrap_or(u64::MAX);
    match store::read_capped_bytes(&path, max)? {
        None => Ok(None),
        Some(bytes) => Ok(Some(container::open_replica(keys, expected, &bytes)?)),
    }
}

/// The epoch the replica's clear envelope announces (`None`: no replica,
/// or not a replica). Read before any key (§13.3.5: never a re-join
/// downward).
pub(crate) fn replica_epoch(paths: &Paths) -> Result<Option<u32>> {
    let max = u64::try_from(MAX_CONTAINER_FILE_BYTES).unwrap_or(u64::MAX);
    Ok(store::read_capped_bytes(&replica_path(paths), max)?
        .and_then(|bytes| container::peek(container::ContainerKind::Replica, &bytes))
        .map(|envelope| envelope.epoch))
}

/// Seals and writes the replica (atomic, `0600`).
pub(crate) fn save_replica(
    paths: &Paths,
    keys: &Subkeys,
    binding: &Binding,
    content: &ReplicaContent,
) -> Result<()> {
    let sealed = container::seal_replica(keys, binding, content)?;
    let max = u64::try_from(MAX_CONTAINER_FILE_BYTES).unwrap_or(u64::MAX);
    store::write_atomic_capped(&replica_path(paths), sealed.text.as_bytes(), max)
}

/// Forgets this device's sync state (disable): `sync.toml`, the replica
/// and the S2a transition files. Local hosts, snippets and settings are
/// kept, and so is `confirm.toml`: a received snippet never confirmed
/// stays blocked after sync is disabled (F8). `bundle.history/` is kept
/// too: a device that joins the same bundle again as a new device still
/// refuses its old passphrases (§13.3.6; deviation recorded in §13).
pub(crate) fn remove_local_state(paths: &Paths) -> Result<()> {
    remove_transition_files(paths)?;
    for path in [replica_path(paths), config_path(paths)] {
        remove_file(&path)?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(crate::Error::io(path, e)),
    }
}

/// `sync.toml` is gone (sync disabled, possibly by 0.6): the S2a files
/// left behind mean nothing any more (RR6).
pub(crate) fn remove_transition_files(paths: &Paths) -> Result<()> {
    for name in [ROTATION_FILE, NEXT_FILE, VERIFIED_FILE, COMMITMENT_FILE] {
        remove_file(&paths.sync_dir().join(name))?;
    }
    Ok(())
}

// ── S2a: transition, verified copies, history, commitment ───────────────

/// `rotation.toml`: a rotation or adoption in progress (§13.3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rotation {
    pub(crate) version: u32,
    /// Bundle id, 32 hex characters.
    pub(crate) bundle_id: String,
    pub(crate) next_epoch: u32,
}

pub(crate) fn load_rotation(paths: &Paths) -> Result<Option<Rotation>> {
    let path = paths.sync_dir().join(ROTATION_FILE);
    match store::read_capped(&path, MAX_SMALL_FILE_BYTES)? {
        None => Ok(None),
        Some(text) => Ok(Some(store::parse_toml(&path, &text)?)),
    }
}

pub(crate) fn save_rotation(paths: &Paths, rotation: &Rotation) -> Result<()> {
    let text = toml::to_string_pretty(rotation).map_err(|e| crate::Error::Serde {
        what: "toml",
        message: e.to_string(),
    })?;
    store::write_atomic_capped(
        &paths.sync_dir().join(ROTATION_FILE),
        text.as_bytes(),
        MAX_SMALL_FILE_BYTES,
    )
}

pub(crate) fn remove_rotation(paths: &Paths) -> Result<()> {
    remove_file(&paths.sync_dir().join(ROTATION_FILE))
}

/// The local epoch floor (R2): no epoch below it is ever opened, adopted
/// or published under. `rotation.toml` counts only for the same bundle.
pub(crate) fn floor(sync: &SyncConfig, rotation: Option<&Rotation>) -> u32 {
    rotation
        .filter(|r| r.bundle_id == sync.bundle_id)
        .map_or(sync.epoch, |r| r.next_epoch.max(sync.epoch))
}

/// Which local copy of a bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleCopy {
    /// `bundle.verified`.
    Verified,
    /// `bundle.next`.
    Next,
}

impl BundleCopy {
    fn name(self) -> &'static str {
        match self {
            Self::Verified => VERIFIED_FILE,
            Self::Next => NEXT_FILE,
        }
    }
}

pub(crate) fn load_bundle_copy(paths: &Paths, which: BundleCopy) -> Result<Option<String>> {
    store::read_capped(&paths.sync_dir().join(which.name()), MAX_BUNDLE_FILE_BYTES)
}

/// Writes a local copy (atomic, `0600`) when it differs.
pub(crate) fn save_bundle_copy(paths: &Paths, which: BundleCopy, text: &str) -> Result<()> {
    if load_bundle_copy(paths, which)?.as_deref() == Some(text) {
        return Ok(());
    }
    store::write_atomic_capped(
        &paths.sync_dir().join(which.name()),
        text.as_bytes(),
        MAX_BUNDLE_FILE_BYTES,
    )
}

pub(crate) fn remove_bundle_copy(paths: &Paths, which: BundleCopy) -> Result<()> {
    remove_file(&paths.sync_dir().join(which.name()))
}

/// `commitment.toml` (E1): `C_E` of the current epoch, kept outside the
/// bundle and the replica so a device without its data key (locked) can
/// still check a new lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Commitment {
    pub(crate) version: u32,
    pub(crate) bundle_id: String,
    pub(crate) epoch: u32,
    /// `C_E`, hex.
    pub(crate) commit: String,
}

impl Commitment {
    pub(crate) fn bytes(&self) -> Option<[u8; MAC_LEN]> {
        unhex32(&self.commit)
    }
}

pub(crate) fn load_commitment(paths: &Paths) -> Result<Option<Commitment>> {
    let path = paths.sync_dir().join(COMMITMENT_FILE);
    match store::read_capped(&path, MAX_SMALL_FILE_BYTES)? {
        None => Ok(None),
        Some(text) => Ok(store::parse_toml(&path, &text).ok()),
    }
}

/// Writes `commitment.toml` when it changed.
pub(crate) fn save_commitment(
    paths: &Paths,
    bundle_id: &str,
    epoch: u32,
    commit: &[u8; MAC_LEN],
) -> Result<()> {
    let record = Commitment {
        version: CONFIG_VERSION,
        bundle_id: bundle_id.to_owned(),
        epoch,
        commit: hex(commit),
    };
    if load_commitment(paths)?.as_ref() == Some(&record) {
        return Ok(());
    }
    let text = toml::to_string_pretty(&record).map_err(|e| crate::Error::Serde {
        what: "toml",
        message: e.to_string(),
    })?;
    store::write_atomic_capped(
        &paths.sync_dir().join(COMMITMENT_FILE),
        text.as_bytes(),
        MAX_SMALL_FILE_BYTES,
    )
}

/// The retained `C_E` of `epoch` in `bundle` (E1).
pub(crate) fn retained_commitment(
    paths: &Paths,
    bundle_id: &str,
    epoch: u32,
) -> Result<Option<[u8; MAC_LEN]>> {
    Ok(load_commitment(paths)?
        .filter(|c| c.bundle_id == bundle_id && c.epoch == epoch)
        .and_then(|c| c.bytes()))
}

fn history_dir(paths: &Paths) -> PathBuf {
    paths.sync_dir().join(HISTORY_DIR)
}

/// The verified files of `bundle.history/`, newest first: `(sequence,
/// slot id hex, text)`.
pub(crate) fn load_history(paths: &Paths) -> Result<Vec<(u64, String, String)>> {
    let dir = history_dir(paths);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(crate::Error::io(&dir, e)),
    };
    let mut out = Vec::new();
    for entry in entries.take(HISTORY_SLOTS.saturating_mul(4)) {
        let entry = entry.map_err(|e| crate::Error::io(&dir, e))?;
        let name = entry.file_name();
        let Some((seq, slot)) = name.to_str().and_then(parse_history_name) else {
            continue;
        };
        if let Some(text) = store::read_capped(&entry.path(), MAX_BUNDLE_FILE_BYTES)? {
            out.push((seq, slot, text));
        }
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    Ok(out)
}

/// `<sequence 20 digits>-<slot id 32 hex>.toml`.
fn parse_history_name(name: &str) -> Option<(u64, String)> {
    let stem = name.strip_suffix(".toml")?;
    let (seq, slot) = stem.split_once('-')?;
    let valid = seq.len() == 20
        && seq.bytes().all(|b| b.is_ascii_digit())
        && slot.len() == 32
        && slot.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid {
        return None;
    }
    Some((seq.parse().ok()?, slot.to_owned()))
}

/// Adds a verified file to `bundle.history/` when its passphrase slot is
/// new, keeping the newest [`HISTORY_SLOTS`] distinct slots (RR1).
pub(crate) fn remember_slot(paths: &Paths, slot_id: &[u8; ID_LEN], text: &str) -> Result<()> {
    let slot = hex(slot_id);
    let history = load_history(paths)?;
    if history.iter().any(|(_, known, _)| *known == slot) {
        return Ok(());
    }
    let dir = history_dir(paths);
    store::create_private_dir_all(&dir)?;
    let seq = history
        .first()
        .map_or(1, |(seq, _, _)| seq.saturating_add(1));
    store::write_atomic_capped(
        &dir.join(format!("{seq:020}-{slot}.toml")),
        text.as_bytes(),
        MAX_BUNDLE_FILE_BYTES,
    )?;
    for (old, known, _) in history.iter().skip(HISTORY_SLOTS.saturating_sub(1)) {
        remove_file(&dir.join(format!("{old:020}-{known}.toml")))?;
    }
    Ok(())
}

/// The inter-process lock of a pass; `None` when another Portico process
/// holds it (the pass is skipped and retried at the next poll).
pub(crate) fn try_lock(paths: &Paths) -> Result<Option<std::fs::File>> {
    store::try_lock_exclusive(&paths.sync_dir().join(LOCK_FILE))
}

/// The same lock, waited for (user commands).
pub(crate) fn lock(paths: &Paths) -> Result<std::fs::File> {
    store::lock_exclusive(&paths.sync_dir().join(LOCK_FILE))
}

/// `confirm.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Confirmations {
    /// Snippet id → hash of the command received through sync, awaiting
    /// the user's confirmation on this device (F8).
    pub(crate) snippets: BTreeMap<String, String>,
}

pub(crate) fn load_confirmations(paths: &Paths) -> Result<Confirmations> {
    store::load_toml_capped(&confirm_path(paths), MAX_SMALL_FILE_BYTES)
}

/// Read-modify-write of `confirm.toml` under its own lock.
pub(crate) fn edit_confirmations<T>(
    paths: &Paths,
    edit: impl FnOnce(&mut Confirmations) -> T,
) -> Result<T> {
    let path = confirm_path(paths);
    let _lock = store::lock_exclusive(&store::lock_path(&path))?;
    let mut confirmations = load_confirmations(paths)?;
    let before = confirmations.clone();
    let out = edit(&mut confirmations);
    if confirmations != before {
        let text = toml::to_string_pretty(&confirmations).map_err(|e| crate::Error::Serde {
            what: "toml",
            message: e.to_string(),
        })?;
        store::write_atomic_capped(&path, text.as_bytes(), MAX_SMALL_FILE_BYTES)?;
    }
    Ok(out)
}

/// Drops the confirmation gate of a deleted snippet (best effort: a gate
/// left behind only blocks that exact content under that id).
pub(crate) fn forget_confirmation(paths: &Paths, id: &str) {
    let gated = load_confirmations(paths).is_ok_and(|c| c.snippets.contains_key(id));
    if gated {
        let _ = edit_confirmations(paths, |c| {
            c.snippets.remove(id);
        });
    }
}

/// Hash (hex SHA-256) identifying the content a confirmation is about.
pub(crate) fn content_hash(domain: &str, text: &str) -> String {
    let digest = super::crypto::content_fingerprint(domain, &[text.as_bytes()]);
    hex(&digest)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut text, b| {
            let _ = write!(text, "{b:02x}");
            text
        },
    )
}

/// `true` when the sync dir holds a `sync.toml` (cheap probe).
pub(crate) fn is_set_up(paths: &Paths) -> bool {
    Path::new(&config_path(paths)).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_holds_no_secret() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        assert_eq!(load_config(&paths).expect("load"), None);
        let config = SyncConfig::new(
            "/x/Dropbox".into(),
            "/x/Dropbox".into(),
            &[7; ID_LEN],
            1,
            DeviceId::from_bytes([1; ID_LEN]),
            true,
        );
        save_config(&paths, &config).expect("save");
        assert!(is_set_up(&paths));
        assert_eq!(load_config(&paths).expect("load"), Some(config.clone()));
        assert_eq!(config.bundle_bytes().expect("id"), [7; ID_LEN]);
        let text = std::fs::read_to_string(paths.sync_dir().join("sync.toml")).expect("r");
        assert!(
            !text.contains("passphrase") && !text.contains("data_key"),
            "{text}"
        );
        remove_local_state(&paths).expect("rm");
        assert_eq!(load_config(&paths).expect("load"), None);
    }

    #[test]
    fn confirmations_edit_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        edit_confirmations(&paths, |c| {
            c.snippets.insert("deploy".into(), content_hash("x", "ls"));
        })
        .expect("edit");
        let loaded = load_confirmations(&paths).expect("load");
        assert_eq!(loaded.snippets.len(), 1);
        let lock = try_lock(&paths).expect("lock");
        assert!(lock.is_some());
        assert!(try_lock(&paths).expect("second").is_none(), "held");
    }

    #[test]
    fn floor_counts_a_transition_of_this_bundle_only() {
        let config = SyncConfig::new(
            "/x".into(),
            "/x".into(),
            &[7; ID_LEN],
            3,
            DeviceId::from_bytes([1; ID_LEN]),
            true,
        );
        assert_eq!(floor(&config, None), 3);
        let next = Rotation {
            version: 1,
            bundle_id: config.bundle_id.clone(),
            next_epoch: 4,
        };
        assert_eq!(floor(&config, Some(&next)), 4);
        let other = Rotation {
            bundle_id: "00".repeat(16),
            ..next.clone()
        };
        assert_eq!(floor(&config, Some(&other)), 3);
        let behind = Rotation {
            next_epoch: 2,
            ..next
        };
        assert_eq!(floor(&config, Some(&behind)), 3, "never below sync.toml");
    }

    #[test]
    fn transition_files_round_trip_and_go_with_disable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        store::create_private_dir_all(&paths.sync_dir()).expect("mkdir");
        let rotation = Rotation {
            version: 1,
            bundle_id: "ab".repeat(16),
            next_epoch: 2,
        };
        save_rotation(&paths, &rotation).expect("save");
        assert_eq!(load_rotation(&paths).expect("load"), Some(rotation));
        save_bundle_copy(&paths, BundleCopy::Next, "next").expect("next");
        save_bundle_copy(&paths, BundleCopy::Verified, "verified").expect("verified");
        save_commitment(&paths, &"ab".repeat(16), 2, &[5; MAC_LEN]).expect("commit");
        assert_eq!(
            retained_commitment(&paths, &"ab".repeat(16), 2).expect("read"),
            Some([5; MAC_LEN])
        );
        assert_eq!(
            retained_commitment(&paths, &"ab".repeat(16), 1).expect("read"),
            None
        );
        remember_slot(&paths, &[1; ID_LEN], "slot").expect("slot");
        remove_local_state(&paths).expect("rm");
        assert_eq!(load_rotation(&paths).expect("load"), None);
        assert_eq!(load_bundle_copy(&paths, BundleCopy::Next).expect("n"), None);
        assert_eq!(
            load_bundle_copy(&paths, BundleCopy::Verified).expect("v"),
            None
        );
        assert_eq!(load_commitment(&paths).expect("c"), None);
        assert_eq!(load_history(&paths).expect("h").len(), 1, "history kept");
    }

    #[test]
    fn history_keeps_the_newest_distinct_slots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        for n in 0..12u8 {
            remember_slot(&paths, &[n; ID_LEN], &format!("bundle {n}")).expect("slot");
            remember_slot(&paths, &[n; ID_LEN], "again").expect("same slot: no entry");
        }
        let history = load_history(&paths).expect("history");
        assert_eq!(history.len(), HISTORY_SLOTS);
        let texts: Vec<&str> = history.iter().map(|(_, _, text)| text.as_str()).collect();
        assert_eq!(texts.first().copied(), Some("bundle 11"), "newest first");
        assert_eq!(texts.last().copied(), Some("bundle 4"));
        // Stray files are ignored.
        std::fs::write(
            paths.sync_dir().join("bundle.history").join("junk.toml"),
            "x",
        )
        .expect("w");
        assert_eq!(load_history(&paths).expect("history").len(), HISTORY_SLOTS);
    }

    #[test]
    fn lineage_memory_keeps_sixteen_epochs_and_redacts_keys() {
        use super::super::crypto::{DataKey, derive_subkeys};
        let mut local = LocalReplica::default();
        let dk = DataKey::from_bytes(&[3; 32]).expect("dk");
        for epoch in 1..=20 {
            let keys = derive_subkeys(&dk, &[1; ID_LEN], epoch).expect("keys");
            local.remember_epoch(
                epoch,
                &[u8::try_from(epoch).expect("u8"); MAC_LEN],
                KeyBytes::of(&keys.rotate),
            );
        }
        assert_eq!(local.lineage.len(), LINEAGE_MEMORY);
        assert_eq!(local.lineage.first().map(|r| r.epoch), Some(5));
        assert_eq!(
            local.lineage.first().and_then(RetainedEpoch::commit_bytes),
            Some([5; MAC_LEN])
        );
        let debug = format!("{local:?}");
        assert!(debug.contains("<redacted>"));
        // The keys round-trip through the sealed JSON.
        let json = serde_json::to_string(&local).expect("json");
        let back: LocalReplica = serde_json::from_str(&json).expect("back");
        assert_eq!(back, local);
    }
}
