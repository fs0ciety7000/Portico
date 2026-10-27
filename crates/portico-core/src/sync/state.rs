//! This device's sync state in `data_dir/sync/` (S1b, `docs/SYNC.md`
//! §3.4, §5.1, security review F5).
//!
//! | File | Content | Protection |
//! |---|---|---|
//! | `sync.toml` | folder (as chosen and canonical), bundle id, epoch, device id, "remember the key", skipped records | plain TOML, `0600`, **no secret** |
//! | `replica.snap` | name, dot counter, publish sequence, merged state, high-water marks, last applied projection, confirmations | sealed under the local subkey, one atomic file (F5) |
//! | `confirm.toml` | snippets changed by sync and not yet confirmed (id → hash of the received command) | plain TOML: the engine refuses to run them (F8) even while sync is locked |
//! | `sync.lock` | inter-process lock, from projection to publication (§4.9) | — |
//!
//! The data dir is local (`%LOCALAPPDATA%` on Windows): the sync identity
//! never roams with a profile (case E4). Everything here is blocking.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::SyncError;
use super::clock::{DeviceId, VersionVector};
use super::container::{self, Binding, Expected, MAX_CONTAINER_FILE_BYTES, ReplicaContent};
use super::crypto::{ID_LEN, Subkeys};
use super::model::{WireKey, WireValue};
use crate::error::Result;
use crate::store::{self, Paths};

const CONFIG_FILE: &str = "sync.toml";
const REPLICA_FILE: &str = "replica.snap";
const CONFIRM_FILE: &str = "confirm.toml";
const LOCK_FILE: &str = "sync.lock";
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
}

impl LocalReplica {
    pub(crate) fn applied_map(&self) -> BTreeMap<WireKey, WireValue> {
        self.applied.iter().cloned().collect()
    }

    pub(crate) fn set_applied(&mut self, applied: BTreeMap<WireKey, WireValue>) {
        self.applied = applied.into_iter().collect();
    }
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
/// and the confirmations. Local hosts, snippets and settings are kept.
pub(crate) fn remove_local_state(paths: &Paths) -> Result<()> {
    for path in [replica_path(paths), config_path(paths), confirm_path(paths)] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(crate::Error::io(&path, e)),
        }
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
}
