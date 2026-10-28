//! Secret storage. **No secret is ever written in clear text.**
//!
//! Backends, in order of preference:
//! 1. [`KeyringStore`]: macOS Keychain / Linux Secret Service (GNOME Keyring,
//!    `KWallet` via Secret Service) / Windows Credential Manager (generic
//!    credentials, target `<account>.dev.portico`, the crate's default
//!    `Enterprise` persistence) through the `keyring` crate.
//! 2. [`VaultStore`]: local file encrypted with XChaCha20-Poly1305, key
//!    derived from a master passphrase with Argon2id (see [`crate::vault`]).
//!    Used when no system keyring is available (headless Linux, SSH
//!    sessions). [`default_store`] picks the backend.
//!
//! Secrets are passed around as [`SecretString`] (redacted `Debug`, zeroized
//! on drop) and must never reach logs, events or error messages.
//!
//! [`IndexedSecretStore`] (S2a, `docs/SYNC.md` §13.6.1) wraps the engine's
//! store: every `set` and `delete` is recorded in
//! `data_dir/secrets-index.toml` (account → generation, present), a local,
//! non-secret index with a global monotonic counter. The engine wraps its
//! store once ([`IndexedSecretStore::wrap`]), so sessions, managed hosts,
//! the vault backend and the sync all write through it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::Paths;
use crate::vault::VaultStore;

/// Keyring "service" name for every Portico entry.
pub const SERVICE: &str = "dev.portico";

/// What a secret unlocks. Rendered as the keyring account name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretKey {
    /// Password for `user@host:port`, bound to the host key that was
    /// verified for that connection (`host` is the `known_hosts` name, i.e.
    /// `HostKeyAlias` or the lowercased `HostName`). A different server
    /// identity never receives a remembered password.
    HostPassword {
        user: String,
        host: String,
        port: u16,
        host_key: String,
    },
    /// Passphrase of a private key file.
    KeyPassphrase { fingerprint: String },
    /// Password typed when a host was created in Portico (phase 6b),
    /// before any connection: the server's key is not known yet, so it
    /// cannot be bound to it. `host` and `port` are the `known_hosts` name
    /// and port, as for [`Self::HostPassword`]. It is only offered once the
    /// host key passed the usual verification (a changed key is still a
    /// blocking refusal), and the first login it opens moves it to
    /// [`Self::HostPassword`], bound to that key.
    UnboundHostPassword {
        user: String,
        host: String,
        port: u16,
    },
    /// Data key of a folder sync bundle (phase 11, `docs/SYNC.md` §2.2),
    /// remembered when the user keeps "remember on this device": base64 of
    /// the 32 bytes. `bundle` is the bundle id in hex. On Windows the entry
    /// is `Local` (never roaming, case E4); on macOS a keychain item that is
    /// not synchronizable (the `keyring` default).
    SyncDataKey { bundle: String, epoch: u32 },
}

impl SecretKey {
    pub fn account(&self) -> String {
        match self {
            Self::HostPassword {
                user,
                host,
                port,
                host_key,
            } => format!("password:{user}@{host}:{port}#{host_key}"),
            Self::KeyPassphrase { fingerprint } => format!("passphrase:{fingerprint}"),
            Self::UnboundHostPassword { user, host, port } => {
                format!("unbound-password:{user}@{host}:{port}")
            }
            Self::SyncDataKey { bundle, epoch } => format!("sync-key:{bundle}:{epoch}"),
        }
    }
}

/// Backend-agnostic secret storage. Implementations are blocking: callers in
/// async contexts must use `tokio::task::spawn_blocking`.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>>;
    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()>;
    fn delete(&self, key: &SecretKey) -> Result<()>;

    /// `true` when a secret is stored under `key`. The default reads it
    /// (dropped, zeroized, at once); stores that can tell without reading
    /// the secret override it.
    fn contains(&self, key: &SecretKey) -> Result<bool> {
        Ok(self.get(key)?.is_some())
    }

    /// The vault behind this store, when it is one (lock / unlock from the
    /// [`crate::Engine`]).
    fn vault(&self) -> Option<&VaultStore> {
        None
    }

    /// `true` for an [`IndexedSecretStore`] (never wrapped twice).
    fn is_indexed(&self) -> bool {
        false
    }
}

impl SecretKey {
    /// The data key entry an account name designates, when it has the
    /// exact form `sync-key:<32 hex>:<u32>` (§13.3.8, R15): nothing else
    /// (`password:`, `passphrase:`, `unbound-password:`…) parses.
    pub(crate) fn parse_sync_key(account: &str) -> Option<Self> {
        let rest = account.strip_prefix("sync-key:")?;
        let (bundle, epoch) = rest.split_once(':')?;
        let hex = bundle.len() == 32
            && bundle
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        let digits =
            !epoch.is_empty() && epoch.len() <= 10 && epoch.bytes().all(|b| b.is_ascii_digit());
        if !hex || !digits {
            return None;
        }
        let epoch: u32 = epoch.parse().ok()?;
        let key = Self::SyncDataKey {
            bundle: bundle.to_owned(),
            epoch,
        };
        // Canonical only: `sync-key:<b>:007` is not the entry of epoch 7.
        (key.account() == account).then_some(key)
    }
}

/// Name of the index file in the data dir.
const INDEX_FILE: &str = "secrets-index.toml";
/// Largest index read or written.
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
/// Absent entries kept (the oldest beyond are pruned; present ones never).
const MAX_ABSENT_ENTRIES: usize = 4096;
const INDEX_VERSION: u32 = 1;

/// One account of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexEntry {
    /// The global counter value of its last `set` or `delete` (R9: a
    /// `set`, `delete`, `set` of the same value is seen).
    pub(crate) generation: u64,
    /// `false` after a `delete`.
    pub(crate) present: bool,
}

/// `secrets-index.toml`: account names only, never a secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SecretIndex {
    pub(crate) version: u32,
    /// Global monotonic counter.
    pub(crate) counter: u64,
    pub(crate) entries: BTreeMap<String, IndexEntry>,
}

impl SecretIndex {
    /// Present accounts starting with `prefix`.
    pub(crate) fn present_with_prefix(&self, prefix: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(account, entry)| entry.present && account.starts_with(prefix))
            .map(|(account, _)| account.clone())
            .collect()
    }
}

fn index_path(paths: &Paths) -> PathBuf {
    paths.data_dir.join(INDEX_FILE)
}

/// The index of secrets written through an [`IndexedSecretStore`] on this
/// data dir (empty when none was written). Blocking.
pub(crate) fn load_index(paths: &Paths) -> Result<SecretIndex> {
    load_index_at(&index_path(paths))
}

fn load_index_at(path: &std::path::Path) -> Result<SecretIndex> {
    crate::store::load_toml_capped(path, MAX_INDEX_BYTES)
}

/// The engine's secret store with an index of accounts (see the module
/// doc). The index entry is written **before** `set` (a crash in between
/// leaves an entry that names nothing, harmless) and marked absent
/// **after** `delete`. Read-modify-write under the index's own lock (the
/// TUI and the GUI share a data dir).
#[derive(Debug)]
pub struct IndexedSecretStore {
    inner: Arc<dyn SecretStore>,
    path: PathBuf,
}

impl IndexedSecretStore {
    /// `inner` wrapped (once: an indexed store is returned as is).
    pub fn wrap(paths: &Paths, inner: Arc<dyn SecretStore>) -> Arc<dyn SecretStore> {
        if inner.is_indexed() {
            return inner;
        }
        Arc::new(Self {
            inner,
            path: index_path(paths),
        })
    }

    fn record(&self, account: String, present: bool) -> Result<()> {
        let _lock = crate::store::lock_exclusive(&crate::store::lock_path(&self.path))?;
        let mut index = load_index_at(&self.path)?;
        index.version = INDEX_VERSION;
        index.counter = index
            .counter
            .checked_add(1)
            .ok_or_else(|| Error::Secrets("the secret index counter is exhausted".to_owned()))?;
        let generation = index.counter;
        index.entries.insert(
            account,
            IndexEntry {
                generation,
                present,
            },
        );
        let absent: Vec<(u64, String)> = index
            .entries
            .iter()
            .filter(|(_, entry)| !entry.present)
            .map(|(account, entry)| (entry.generation, account.clone()))
            .collect();
        if absent.len() > MAX_ABSENT_ENTRIES {
            let mut oldest = absent;
            oldest.sort_unstable();
            let excess = oldest.len().saturating_sub(MAX_ABSENT_ENTRIES);
            for (_, account) in oldest.into_iter().take(excess) {
                index.entries.remove(&account);
            }
        }
        let text = toml::to_string_pretty(&index).map_err(|e| Error::Serde {
            what: "toml",
            message: e.to_string(),
        })?;
        crate::store::write_atomic_capped(&self.path, text.as_bytes(), MAX_INDEX_BYTES)
    }
}

impl SecretStore for IndexedSecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>> {
        self.inner.get(key)
    }

    fn contains(&self, key: &SecretKey) -> Result<bool> {
        self.inner.contains(key)
    }

    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()> {
        // Indexed first: a secret is never stored without an entry.
        self.record(key.account(), true)?;
        self.inner.set(key, secret)
    }

    fn delete(&self, key: &SecretKey) -> Result<()> {
        self.inner.delete(key)?;
        // Best effort: an entry left "present" only makes a later cleanup
        // delete again (deleting is idempotent).
        let _ = self.record(key.account(), false);
        Ok(())
    }

    fn vault(&self) -> Option<&VaultStore> {
        self.inner.vault()
    }

    fn is_indexed(&self) -> bool {
        true
    }
}

/// The platform secret store: the system keyring when it initialized,
/// otherwise the encrypted vault at [`Paths::vault`] (locked until the user
/// unlocks or creates it). Blocking (may probe the keyring service).
pub fn default_store(paths: &Paths) -> Arc<dyn SecretStore> {
    if KeyringStore::is_available() {
        Arc::new(KeyringStore)
    } else {
        Arc::new(VaultStore::open(paths.vault()))
    }
}

/// System keyring backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    /// `true` if the platform credential store initialized correctly.
    pub fn is_available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    #[cfg(not(windows))]
    fn entry(key: &SecretKey) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &key.account()).map_err(keyring_err)
    }

    /// Windows: the Credential Manager entry with persistence `Local`
    /// (`CRED_PERSIST_LOCAL_MACHINE`): never copied to a roaming domain
    /// profile, unlike the crate's default (`Enterprise`). The persistence
    /// applies when an entry is written.
    #[cfg(windows)]
    fn entry(key: &SecretKey) -> Result<keyring::Entry> {
        // Initialises the default (Windows) store, as `Entry::new` does.
        if keyring::Entry::store_status().is_err() {
            return Err(keyring_err(keyring::Error::NoDefaultStore));
        }
        let modifiers = std::collections::HashMap::from([("persistence", "Local")]);
        keyring_core::Entry::new_with_modifiers(SERVICE, &key.account(), &modifiers)
            .map(|inner| keyring::Entry { inner })
            .map_err(keyring_err)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>> {
        match Self::entry(key)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::from(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keyring_err(e)),
        }
    }

    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()> {
        Self::entry(key)?
            .set_password(secret.expose_secret())
            .map_err(keyring_err)
    }

    fn delete(&self, key: &SecretKey) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keyring_err(e)),
        }
    }
}

/// In-memory store: tests, and sessions where no keyring is available
/// (secrets then live only for the process lifetime).
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: std::sync::Mutex<std::collections::HashMap<String, SecretString>>,
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key.account())
            .cloned())
    }

    fn contains(&self, key: &SecretKey) -> Result<bool> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&key.account()))
    }

    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.account(), secret.clone());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key.account());
        Ok(())
    }
}

// keyring errors never contain the secret itself, only platform diagnostics.
#[allow(clippy::needless_pass_by_value)]
fn keyring_err(e: keyring::Error) -> Error {
    Error::Secrets(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Credential Manager entries are written `Local` (never roaming).
    /// Wine's Credential Manager is not Windows': skipped there.
    #[cfg(windows)]
    #[test]
    fn keyring_entries_are_local() {
        if crate::winfs::tests::under_wine() {
            return;
        }
        let key = SecretKey::KeyPassphrase {
            fingerprint: format!("SHA256:portico-test-{:016x}", rand::random::<u64>()),
        };
        let store = KeyringStore;
        store
            .set(&key, &SecretString::from("s3cret".to_owned()))
            .unwrap();
        let attributes = KeyringStore::entry(&key)
            .unwrap()
            .inner
            .get_attributes()
            .unwrap();
        store.delete(&key).unwrap();
        assert_eq!(
            attributes.get("persistence").map(String::as_str),
            Some("Local"),
            "{attributes:?}"
        );
    }

    #[test]
    fn account_names_are_stable() {
        let k = SecretKey::HostPassword {
            user: "root".into(),
            host: "db".into(),
            port: 22,
            host_key: "SHA256:abc".into(),
        };
        assert_eq!(k.account(), "password:root@db:22#SHA256:abc");
        let k = SecretKey::UnboundHostPassword {
            user: "root".into(),
            host: "db".into(),
            port: 2222,
        };
        assert_eq!(k.account(), "unbound-password:root@db:2222");
        let k = SecretKey::SyncDataKey {
            bundle: "00ff".into(),
            epoch: 1,
        };
        assert_eq!(k.account(), "sync-key:00ff:1");
    }

    #[test]
    fn sync_key_accounts_parse_strictly() {
        let bundle = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            SecretKey::parse_sync_key(&format!("sync-key:{bundle}:7")),
            Some(SecretKey::SyncDataKey {
                bundle: bundle.into(),
                epoch: 7
            })
        );
        for refused in [
            "password:root@db:22#SHA256:abc".to_owned(),
            "passphrase:SHA256:abc".to_owned(),
            "unbound-password:root@db:22".to_owned(),
            format!("sync-key:{bundle}:007"),
            format!("sync-key:{bundle}:"),
            format!("sync-key:{bundle}:4294967296"),
            format!("sync-key:{}:1", bundle.to_uppercase()),
            "sync-key:00ff:1".to_owned(),
            format!("sync-key:{bundle}:1:2"),
        ] {
            assert_eq!(SecretKey::parse_sync_key(&refused), None, "{refused}");
        }
    }

    fn indexed() -> (
        tempfile::TempDir,
        Paths,
        Arc<MemoryStore>,
        Arc<dyn SecretStore>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        let inner = Arc::new(MemoryStore::default());
        let store = IndexedSecretStore::wrap(&paths, Arc::clone(&inner) as Arc<dyn SecretStore>);
        (dir, paths, inner, store)
    }

    #[test]
    fn index_records_before_set_with_a_global_counter() {
        let (_dir, paths, inner, store) = indexed();
        assert!(store.is_indexed());
        assert!(
            IndexedSecretStore::wrap(&paths, Arc::clone(&store)).is_indexed(),
            "never wrapped twice"
        );
        let a = SecretKey::KeyPassphrase {
            fingerprint: "SHA256:a".into(),
        };
        let b = SecretKey::SyncDataKey {
            bundle: "00".repeat(16),
            epoch: 2,
        };
        store
            .set(&a, &SecretString::from("hunter2".to_owned()))
            .expect("set");
        store
            .set(&b, &SecretString::from("x".to_owned()))
            .expect("set");
        store.delete(&a).expect("delete");
        store
            .set(&a, &SecretString::from("hunter2".to_owned()))
            .expect("set");
        let index = load_index(&paths).expect("index");
        assert_eq!(index.counter, 4, "one global counter");
        assert_eq!(
            index.entries.get(&a.account()),
            Some(&IndexEntry {
                generation: 4,
                present: true
            }),
            "set, delete, set of the same value is seen (R9)"
        );
        store.delete(&b).expect("delete");
        let index = load_index(&paths).expect("index");
        assert_eq!(
            index.entries.get(&b.account()),
            Some(&IndexEntry {
                generation: 5,
                present: false
            }),
            "kept, marked absent"
        );
        assert!(inner.get(&b).expect("get").is_none());
        let text = std::fs::read_to_string(paths.data_dir.join(INDEX_FILE)).expect("read");
        assert!(!text.contains("hunter2"), "accounts only");
        assert_eq!(index.present_with_prefix("passphrase:"), vec![a.account()]);
    }

    #[test]
    fn a_crash_between_index_and_set_leaves_a_harmless_entry() {
        // The index is written first: simulate the crash by recording
        // without storing.
        let (_dir, paths, inner, store) = indexed();
        let key = SecretKey::SyncDataKey {
            bundle: "11".repeat(16),
            epoch: 3,
        };
        IndexedSecretStore {
            inner: Arc::clone(&inner) as Arc<dyn SecretStore>,
            path: index_path(&paths),
        }
        .record(key.account(), true)
        .expect("record");
        assert!(store.get(&key).expect("get").is_none(), "nothing stored");
        assert_eq!(
            load_index(&paths)
                .expect("index")
                .present_with_prefix("sync-key:"),
            vec![key.account()],
            "the entry names it: a cleanup deletes it (idempotent)"
        );
        store.delete(&key).expect("delete is idempotent");
        assert!(
            load_index(&paths)
                .expect("index")
                .present_with_prefix("sync-key:")
                .is_empty()
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let s = SecretString::from("hunter2".to_owned());
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
