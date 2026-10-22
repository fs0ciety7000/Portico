//! Encrypted fallback secret store (Phase 3).
//!
//! Used when no system keyring is available (headless Linux, SSH sessions):
//! remembered passwords and key passphrases live in `vault.toml` (data dir),
//! encrypted under a key derived from a master passphrase.
//!
//! ## Format (version 2)
//!
//! A readable, versioned TOML file whose binary fields are standard base64:
//!
//! ```toml
//! format = "portico-vault"
//! version = 2
//! [kdf]            # Argon2id v0x13: memory_kib, iterations, parallelism, salt
//! [cipher]         # xchacha20poly1305: check_nonce, check, nonce, ciphertext
//! ```
//!
//! - **KDF**: Argon2id produces 64 bytes from the passphrase and a random
//!   16-byte salt: an encryption key and an independent *check* key. The
//!   parameters are stored in the file and must lie inside [`KdfBounds`]
//!   **before** any derivation runs: a hostile file can neither weaken the
//!   KDF below the defaults (floor) nor freeze the machine (ceiling).
//! - **Content**: one JSON object
//!   `{"generation": n, "entries": {account: base64(secret)}}`. Secrets are
//!   base64-encoded so that the JSON never needs escaping: neither encoding
//!   nor decoding goes through an intermediate (non-zeroized) copy. The
//!   object is padded with spaces to a multiple of 256 bytes (the size leaks
//!   only a coarse bucket) and encrypted with XChaCha20-Poly1305 under a
//!   fresh random 24-byte nonce at every write.
//! - **Authenticated header**: the AAD is a canonical rendering of the format,
//!   version, KDF parameters, salt, cipher and check nonce. Changing any of
//!   them invalidates both tags.
//! - **Check tag**: the AEAD tag of an empty message under the check key
//!   (AAD = header). It tells a wrong passphrase ([`VaultError::WrongPassphrase`])
//!   apart from a damaged ciphertext ([`VaultError::Corrupt`]); it gives an
//!   offline attacker nothing the ciphertext did not already give.
//! - Version 1 (development builds only, never released) is refused with
//!   [`VaultError::UnsupportedVersion`]: the vault has to be recreated.
//!
//! ## Invariants
//!
//! - Keys and decrypted content live in [`Zeroizing`] buffers and
//!   [`SecretString`]s; nothing secret reaches `Debug`, logs or errors.
//! - Every write is atomic (`store::write_atomic`, `0600`); creation never
//!   replaces an existing file (`store::write_new`).
//! - **Cross-process lock**: every read-modify-write (create, write of an
//!   entry, change of passphrase) holds an exclusive advisory lock on
//!   `vault.toml.lock` (`0600`, next to the vault, left in place) from the
//!   read to the rename. Writers in other processes (or other handles) are
//!   serialized: a re-key can never be reverted by a writer that read the
//!   file before it. Unlocking only reads one atomically renamed file and
//!   takes no lock. Lock order: in-memory state, then the file lock.
//! - Writes re-read the file under the lock: entries written by another
//!   process are kept, and a vault re-keyed by another process locks this
//!   handle ([`VaultError::ChangedOnDisk`]).
//! - **Rollback**: the content carries a `generation` counter incremented at
//!   every write and kept in memory while unlocked. A write that reads an
//!   older generation (an older copy of the file put back, by a restore or an
//!   attacker) is refused and locks the vault ([`VaultError::Rollback`]).
//!   The protection lasts for the lifetime of the unlocked handle only: an
//!   older file put back while Portico is not running unlocks normally.
//! - All operations are **blocking** (Argon2 takes ~0.5 s by design, plus file
//!   I/O and the lock): async callers go through
//!   `tokio::task::spawn_blocking`, as the [`crate::Engine`] `vault_*`
//!   methods do.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use argon2::{Algorithm, Argon2, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{AeadInOut, Generate, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::Result;
use crate::secrets::{SecretKey, SecretStore};
use crate::store::{self, WriteNewError};

/// Value of the `format` field.
pub const VAULT_FORMAT: &str = "portico-vault";
/// Current (and only readable) format version.
pub const VAULT_VERSION: u32 = 2;
/// Minimum length of a new master passphrase, in characters.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

const KDF_ALGORITHM: &str = "argon2id";
/// Argon2 version 0x13 (19).
const ARGON2_VERSION: u32 = 0x13;
const CIPHER_ALGORITHM: &str = "xchacha20poly1305";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Salt generated for new vaults.
const SALT_LEN: usize = 16;
/// Salts accepted when reading.
const MIN_SALT_LEN: usize = 16;
const MAX_SALT_LEN: usize = 64;
/// Larger files are refused before parsing (a vault holds a few KiB).
const MAX_VAULT_FILE_LEN: u64 = 8 * 1024 * 1024;
/// Plaintext is padded to a multiple of this many bytes.
const PADDING_BLOCK: usize = 256;

const FILE_COMMENT: &str = "# Portico encrypted vault (Argon2id + XChaCha20-Poly1305).\n\
# Do not edit: any change makes it unreadable. Back it up with your passphrase in mind.\n\n";

/// Argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory, in KiB.
    pub memory_kib: u32,
    /// Passes over memory.
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u32,
}

impl KdfParams {
    /// Parameters of new vaults: 64 MiB, 3 passes, 1 lane (~0.3-0.6 s on a
    /// laptop).
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };
}

/// Accepted range of [`KdfParams`], checked before any derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfBounds {
    pub min: KdfParams,
    pub max: KdfParams,
}

impl KdfBounds {
    /// Floor: the defaults (64 MiB, 3 passes): a file cannot ask for a
    /// weaker derivation than the one Portico writes. Ceiling: 256 MiB,
    /// 8 passes, 4 lanes (a few seconds at most, no swap storm).
    pub const PRODUCTION: Self = Self {
        min: KdfParams::DEFAULT,
        max: KdfParams {
            memory_kib: 256 * 1024,
            iterations: 8,
            parallelism: 4,
        },
    };

    fn check(&self, params: &KdfParams) -> std::result::Result<(), VaultError> {
        let fields = [
            (
                "memory_kib",
                params.memory_kib,
                self.min.memory_kib,
                self.max.memory_kib,
            ),
            (
                "iterations",
                params.iterations,
                self.min.iterations,
                self.max.iterations,
            ),
            (
                "parallelism",
                params.parallelism,
                self.min.parallelism,
                self.max.parallelism,
            ),
        ];
        for (param, value, min, max) in fields {
            if !(min..=max).contains(&value) {
                return Err(VaultError::KdfOutOfBounds {
                    param,
                    value,
                    min,
                    max,
                });
            }
        }
        Ok(())
    }
}

/// Lock state of the vault, as shown by the UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultStatus {
    /// The system keyring is the secret store: the vault is not used.
    NotInUse,
    /// No vault file yet.
    Absent,
    /// The file exists; secrets are unreadable until it is unlocked.
    Locked,
    /// The key is in memory: secrets can be read and remembered.
    Unlocked,
}

/// Vault failures. Messages never contain secret material.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VaultError {
    #[error("the vault is locked")]
    Locked,
    #[error("wrong vault passphrase")]
    WrongPassphrase,
    #[error("vault {} is corrupt: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: &'static str },
    #[error("a vault already exists at {}", path.display())]
    Exists { path: PathBuf },
    #[error("no vault at {}", path.display())]
    Missing { path: PathBuf },
    #[error(
        "vault format version {found} is not supported (this Portico reads version {VAULT_VERSION})"
    )]
    UnsupportedVersion { found: u32 },
    #[error("vault key derivation {param} = {value} is outside the accepted range {min}..={max}")]
    KdfOutOfBounds {
        param: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("the vault passphrase must have at least {min} characters")]
    PassphraseTooShort { min: usize },
    #[error("vault {} was re-keyed by another process", path.display())]
    ChangedOnDisk { path: PathBuf },
    #[error("vault {} was replaced by an older copy", path.display())]
    Rollback { path: PathBuf },
    #[error("the system keyring is the secret store: the vault is not in use")]
    NotInUse,
    #[error("vault cryptography failed: {0}")]
    Crypto(&'static str),
}

impl VaultError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Locked => (
                "vault_locked",
                Some(
                    "Unlock the Portico vault with its master passphrase to use remembered secrets.",
                ),
            ),
            Self::WrongPassphrase => (
                "vault_wrong_passphrase",
                Some("Check the master passphrase (keyboard layout, Caps Lock)."),
            ),
            Self::Corrupt { .. } => (
                "vault_corrupt",
                Some(
                    "The vault file is damaged or was modified: restore it from a backup, or move it away to start a new one.",
                ),
            ),
            Self::Exists { .. } => (
                "vault_exists",
                Some("A vault already exists: unlock it instead of creating a new one."),
            ),
            Self::Missing { .. } => ("vault_missing", Some("Create a vault first.")),
            Self::UnsupportedVersion { found } if *found < VAULT_VERSION => (
                "vault_version",
                Some(
                    "This vault was written by an older development build of Portico: move it away and recreate the vault.",
                ),
            ),
            Self::UnsupportedVersion { .. } => (
                "vault_version",
                Some("This vault was written by a newer Portico: update Portico."),
            ),
            Self::KdfOutOfBounds { .. } => (
                "vault_kdf_out_of_bounds",
                Some(
                    "The vault's key derivation settings are outside safe limits: the file may have been tampered with.",
                ),
            ),
            Self::PassphraseTooShort { .. } => (
                "vault_passphrase_too_short",
                Some("Choose a longer master passphrase (several words are easy to remember)."),
            ),
            Self::ChangedOnDisk { .. } => (
                "vault_changed",
                Some("Another Portico changed the vault passphrase: unlock it again."),
            ),
            Self::Rollback { .. } => (
                "vault_rollback",
                Some(
                    "The vault file was replaced by an older version (restored backup or tampering): check it, then unlock it again.",
                ),
            ),
            Self::NotInUse => (
                "vault_not_in_use",
                Some("Secrets are kept in the system keyring; the vault is only a fallback."),
            ),
            Self::Crypto(_) => ("vault_crypto", None),
        }
    }
}

// ── On-disk representation ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultFile {
    format: String,
    version: u32,
    kdf: KdfSection,
    cipher: CipherSection,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KdfSection {
    algorithm: String,
    version: u32,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    salt: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CipherSection {
    algorithm: String,
    check_nonce: String,
    check: String,
    nonce: String,
    ciphertext: String,
}

/// Validated header: everything the AAD covers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Header {
    params: KdfParams,
    salt: Vec<u8>,
    check_nonce: [u8; NONCE_LEN],
}

impl Header {
    fn generate(params: KdfParams) -> std::result::Result<Self, VaultError> {
        Ok(Self {
            params,
            salt: random::<SALT_LEN>()?.to_vec(),
            check_nonce: random::<NONCE_LEN>()?,
        })
    }

    /// Canonical, unambiguous rendering (independent of TOML formatting).
    fn aad(&self) -> Vec<u8> {
        format!(
            "{VAULT_FORMAT}\nversion={VAULT_VERSION}\nkdf={KDF_ALGORITHM}\nkdf_version={ARGON2_VERSION}\n\
             memory_kib={}\niterations={}\nparallelism={}\nsalt={}\ncipher={CIPHER_ALGORITHM}\ncheck_nonce={}\n",
            self.params.memory_kib,
            self.params.iterations,
            self.params.parallelism,
            B64.encode(&self.salt),
            B64.encode(self.check_nonce),
        )
        .into_bytes()
    }
}

/// A parsed, structurally valid vault file.
#[derive(Debug)]
struct Sealed {
    header: Header,
    check_tag: [u8; TAG_LEN],
    nonce: [u8; NONCE_LEN],
    /// Ciphertext followed by its tag.
    ciphertext: Vec<u8>,
}

/// Keys derived from the passphrase.
struct DerivedKeys {
    encryption: Zeroizing<[u8; KEY_LEN]>,
    check: Zeroizing<[u8; KEY_LEN]>,
}

/// Decrypted content (see the module docs).
struct Content {
    generation: u64,
    entries: BTreeMap<String, SecretString>,
}

/// Borrowed view of the decrypted JSON: values are base64, so they never
/// need unescaping (no scratch copy).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentView<'a> {
    generation: u64,
    #[serde(borrow)]
    entries: BTreeMap<Cow<'a, str>, &'a str>,
}

/// In-memory state of an unlocked vault.
struct Unlocked {
    key: Zeroizing<[u8; KEY_LEN]>,
    header: Header,
    check_tag: [u8; TAG_LEN],
    /// Generation of the content last read or written: never goes back.
    generation: u64,
    entries: BTreeMap<String, SecretString>,
}

impl std::fmt::Debug for Unlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unlocked")
            .field("generation", &self.generation)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

/// Encrypted file-backed [`SecretStore`]. Starts locked; see the module docs.
pub struct VaultStore {
    path: PathBuf,
    bounds: KdfBounds,
    /// Parameters used when (re)keying.
    new_params: KdfParams,
    state: Mutex<Option<Unlocked>>,
}

impl std::fmt::Debug for VaultStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultStore")
            .field("path", &self.path)
            .field("unlocked", &self.is_unlocked())
            .finish_non_exhaustive()
    }
}

impl VaultStore {
    /// A locked handle on `path` (no I/O; the file may not exist yet).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self::with_policy(path.into(), KdfBounds::PRODUCTION, KdfParams::DEFAULT)
    }

    fn with_policy(path: PathBuf, bounds: KdfBounds, new_params: KdfParams) -> Self {
        Self {
            path,
            bounds,
            new_params,
            state: Mutex::new(None),
        }
    }

    /// Cheap KDF for tests. Production bounds still apply to [`Self::open`].
    #[cfg(test)]
    pub(crate) fn open_for_tests(path: impl Into<PathBuf>) -> Self {
        let params = KdfParams {
            memory_kib: 64,
            iterations: 1,
            parallelism: 1,
        };
        Self::with_policy(
            path.into(),
            KdfBounds {
                min: params,
                max: KdfBounds::PRODUCTION.max,
            },
            params,
        )
    }

    /// Creates a new vault at `path`, unlocked. Refuses to replace a file.
    pub fn create(path: impl Into<PathBuf>, passphrase: &SecretString) -> Result<Self> {
        let vault = Self::open(path);
        vault.create_in_place(passphrase)?;
        Ok(vault)
    }

    /// Opens and unlocks the vault at `path`.
    pub fn unlock(path: impl Into<PathBuf>, passphrase: &SecretString) -> Result<Self> {
        let vault = Self::open(path);
        vault.unlock_in_place(passphrase)?;
        Ok(vault)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_unlocked(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// [`VaultStatus::Absent`], `Locked` or `Unlocked` (stats the file).
    pub fn status(&self) -> VaultStatus {
        if self.is_unlocked() {
            VaultStatus::Unlocked
        } else if self.path.try_exists().unwrap_or(true) {
            // An unreadable directory counts as "there is something": the
            // user is asked to unlock, which reports the real error.
            VaultStatus::Locked
        } else {
            VaultStatus::Absent
        }
    }

    /// Creates the vault file (empty) and keeps it unlocked.
    pub fn create_in_place(&self, passphrase: &SecretString) -> Result<()> {
        check_new_passphrase(passphrase)?;
        self.bounds.check(&self.new_params)?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _file_lock = self.lock_file()?;
        if self.path.try_exists().unwrap_or(false) {
            return Err(self.exists().into());
        }
        let header = Header::generate(self.new_params)?;
        let keys = derive(passphrase, &header)?;
        let check_tag = check_tag(&keys, &header)?;
        let unlocked = Unlocked {
            key: keys.encryption,
            header,
            check_tag,
            generation: 0,
            entries: BTreeMap::new(),
        };
        let text = seal(&unlocked)?;
        match store::write_new(&self.path, text.as_bytes()) {
            Ok(()) => {}
            Err(WriteNewError::Exists) => return Err(self.exists().into()),
            Err(WriteNewError::Other(e)) => return Err(e),
        }
        *state = Some(unlocked);
        Ok(())
    }

    /// Derives the key and decrypts the vault. The KDF runs without holding
    /// the state lock.
    pub fn unlock_in_place(&self, passphrase: &SecretString) -> Result<()> {
        let sealed = self.read_sealed()?;
        let keys = derive(passphrase, &sealed.header)?;
        verify_check(&keys, &sealed)?;
        let content = self.open_content(&keys.encryption, &sealed)?;
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = Some(Unlocked {
            key: keys.encryption,
            header: sealed.header,
            check_tag: sealed.check_tag,
            generation: content.generation,
            entries: content.entries,
        });
        Ok(())
    }

    /// Forgets the key and every decrypted secret (zeroized on drop).
    pub fn lock(&self) {
        let previous = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(previous);
    }

    /// Re-keys the vault: verifies `old`, then re-encrypts every secret under
    /// a key derived from `new` with a fresh salt and the current default
    /// parameters. Leaves the vault unlocked.
    pub fn change_passphrase(&self, old: &SecretString, new: &SecretString) -> Result<()> {
        check_new_passphrase(new)?;
        // Both held throughout so that no concurrent write (this process or
        // another one) is lost or reverts the new key.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _file_lock = self.lock_file()?;
        let sealed = self.read_sealed()?;
        let old_keys = derive(old, &sealed.header)?;
        verify_check(&old_keys, &sealed)?;
        let content = self.open_content(&old_keys.encryption, &sealed)?;
        drop(old_keys);
        if let Some(unlocked) = state.as_ref()
            && unlocked.header == sealed.header
            && content.generation < unlocked.generation
        {
            *state = None;
            return Err(self.rollback().into());
        }
        let generation = next_generation(content.generation)?;

        self.bounds.check(&self.new_params)?;
        let header = Header::generate(self.new_params)?;
        let keys = derive(new, &header)?;
        let check_tag = check_tag(&keys, &header)?;
        let unlocked = Unlocked {
            key: keys.encryption,
            header,
            check_tag,
            generation,
            entries: content.entries,
        };
        store::write_atomic(&self.path, seal(&unlocked)?.as_bytes())?;
        *state = Some(unlocked);
        Ok(())
    }

    /// Read-modify-write of the entries, under the state lock and the file
    /// lock. The file is re-read so that entries written by another process
    /// are kept; an older generation than the one in memory is a rollback.
    fn mutate(&self, change: impl FnOnce(&mut BTreeMap<String, SecretString>)) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.is_none() {
            return Err(VaultError::Locked.into());
        }
        let _file_lock = self.lock_file()?;
        let Some(unlocked) = state.as_mut() else {
            return Err(VaultError::Locked.into());
        };
        let sealed = match self.read_sealed() {
            Ok(sealed) => sealed,
            Err(e) => {
                if matches!(
                    e,
                    crate::Error::Vault(
                        VaultError::Missing { .. } | VaultError::UnsupportedVersion { .. }
                    )
                ) {
                    *state = None;
                }
                return Err(e);
            }
        };
        if sealed.header != unlocked.header || sealed.check_tag != unlocked.check_tag {
            *state = None;
            return Err(VaultError::ChangedOnDisk {
                path: self.path.clone(),
            }
            .into());
        }
        let Content {
            generation,
            mut entries,
        } = self.open_content(&unlocked.key, &sealed)?;
        if generation < unlocked.generation {
            *state = None;
            return Err(self.rollback().into());
        }
        let next = next_generation(generation)?;
        change(&mut entries);
        let previous_entries = std::mem::replace(&mut unlocked.entries, entries);
        let previous_generation = std::mem::replace(&mut unlocked.generation, next);
        match seal(unlocked).and_then(|text| store::write_atomic(&self.path, text.as_bytes())) {
            Ok(()) => Ok(()),
            Err(e) => {
                unlocked.entries = previous_entries;
                unlocked.generation = previous_generation;
                Err(e)
            }
        }
    }

    /// `vault.toml.lock`, next to the vault.
    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Exclusive cross-process lock, released when the file is dropped.
    fn lock_file(&self) -> Result<std::fs::File> {
        store::lock_exclusive(&self.lock_path())
    }

    fn read_sealed(&self) -> Result<Sealed> {
        use std::io::Read;

        let file = std::fs::File::open(&self.path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                VaultError::Missing {
                    path: self.path.clone(),
                }
                .into()
            } else {
                crate::Error::io(&self.path, e)
            }
        })?;
        let mut text = String::new();
        file.take(MAX_VAULT_FILE_LEN.saturating_add(1))
            .read_to_string(&mut text)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    self.corrupt("not UTF-8 text").into()
                } else {
                    crate::Error::io(&self.path, e)
                }
            })?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_VAULT_FILE_LEN {
            return Err(self.corrupt("file too large").into());
        }
        Ok(self.parse(&text)?)
    }

    fn parse(&self, text: &str) -> std::result::Result<Sealed, VaultError> {
        let file: VaultFile = toml::from_str(text).map_err(|_| self.corrupt("not a vault file"))?;
        if file.format != VAULT_FORMAT {
            return Err(self.corrupt("not a vault file"));
        }
        if file.version != VAULT_VERSION {
            return Err(VaultError::UnsupportedVersion {
                found: file.version,
            });
        }
        if file.kdf.algorithm != KDF_ALGORITHM || file.kdf.version != ARGON2_VERSION {
            return Err(self.corrupt("unknown key derivation"));
        }
        if file.cipher.algorithm != CIPHER_ALGORITHM {
            return Err(self.corrupt("unknown cipher"));
        }
        let params = KdfParams {
            memory_kib: file.kdf.memory_kib,
            iterations: file.kdf.iterations,
            parallelism: file.kdf.parallelism,
        };
        self.bounds.check(&params)?;
        let salt = self.decode(&file.kdf.salt, "invalid salt")?;
        if !(MIN_SALT_LEN..=MAX_SALT_LEN).contains(&salt.len()) {
            return Err(self.corrupt("invalid salt"));
        }
        let check_nonce = self.fixed::<NONCE_LEN>(&file.cipher.check_nonce, "invalid nonce")?;
        let check_tag = self.fixed::<TAG_LEN>(&file.cipher.check, "invalid check tag")?;
        let nonce = self.fixed::<NONCE_LEN>(&file.cipher.nonce, "invalid nonce")?;
        let ciphertext = self.decode(&file.cipher.ciphertext, "invalid ciphertext")?;
        if ciphertext.len() < TAG_LEN {
            return Err(self.corrupt("invalid ciphertext"));
        }
        Ok(Sealed {
            header: Header {
                params,
                salt,
                check_nonce,
            },
            check_tag,
            nonce,
            ciphertext,
        })
    }

    fn decode(
        &self,
        field: &str,
        reason: &'static str,
    ) -> std::result::Result<Vec<u8>, VaultError> {
        B64.decode(field.trim()).map_err(|_| self.corrupt(reason))
    }

    fn fixed<const N: usize>(
        &self,
        field: &str,
        reason: &'static str,
    ) -> std::result::Result<[u8; N], VaultError> {
        <[u8; N]>::try_from(self.decode(field, reason)?).map_err(|_| self.corrupt(reason))
    }

    /// Decrypts and decodes the content (the check tag already proved the
    /// key, so a failure here means a damaged or altered ciphertext).
    fn open_content(
        &self,
        key: &[u8; KEY_LEN],
        sealed: &Sealed,
    ) -> std::result::Result<Content, VaultError> {
        let mut buffer = Zeroizing::new(sealed.ciphertext.clone());
        cipher(key)
            .decrypt_in_place(
                &XNonce::from(sealed.nonce),
                &sealed.header.aad(),
                &mut *buffer,
            )
            .map_err(|_| self.corrupt("ciphertext authentication failed"))?;
        let view: ContentView<'_> =
            serde_json::from_slice(&buffer).map_err(|_| self.corrupt("invalid content"))?;
        let mut entries = BTreeMap::new();
        for (account, encoded) in view.entries {
            let secret = decode_secret(encoded).ok_or_else(|| self.corrupt("invalid content"))?;
            entries.insert(account.into_owned(), secret);
        }
        Ok(Content {
            generation: view.generation,
            entries,
        })
    }

    fn corrupt(&self, reason: &'static str) -> VaultError {
        VaultError::Corrupt {
            path: self.path.clone(),
            reason,
        }
    }

    fn rollback(&self) -> VaultError {
        VaultError::Rollback {
            path: self.path.clone(),
        }
    }

    fn exists(&self) -> VaultError {
        VaultError::Exists {
            path: self.path.clone(),
        }
    }
}

impl SecretStore for VaultStore {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let unlocked = state.as_ref().ok_or(VaultError::Locked)?;
        Ok(unlocked.entries.get(&key.account()).cloned())
    }

    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()> {
        let account = key.account();
        self.mutate(|entries| {
            entries.insert(account, secret.clone());
        })
    }

    fn delete(&self, key: &SecretKey) -> Result<()> {
        let account = key.account();
        self.mutate(|entries| {
            entries.remove(&account);
        })
    }

    fn vault(&self) -> Option<&VaultStore> {
        Some(self)
    }
}

// ── Crypto helpers ──────────────────────────────────────────────────────

fn check_new_passphrase(passphrase: &SecretString) -> std::result::Result<(), VaultError> {
    if passphrase.expose_secret().chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(VaultError::PassphraseTooShort {
            min: MIN_PASSPHRASE_CHARS,
        });
    }
    Ok(())
}

fn next_generation(generation: u64) -> std::result::Result<u64, VaultError> {
    generation
        .checked_add(1)
        .ok_or(VaultError::Crypto("the generation counter overflowed"))
}

/// Base64 → secret, through zeroized buffers only. The final `String` has
/// exactly the needed capacity, so turning it into a `SecretString` does not
/// reallocate (no stray copy).
fn decode_secret(encoded: &str) -> Option<SecretString> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(base64::decoded_len_estimate(
        encoded.len(),
    )));
    B64.decode_vec(encoded, &mut bytes).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut secret = String::with_capacity(text.len());
    secret.push_str(text);
    Some(SecretString::from(secret))
}

fn random<const N: usize>() -> std::result::Result<[u8; N], VaultError> {
    <[u8; N]>::try_generate().map_err(|_| VaultError::Crypto("the system random generator failed"))
}

fn derive(
    passphrase: &SecretString,
    header: &Header,
) -> std::result::Result<DerivedKeys, VaultError> {
    let params = argon2::Params::new(
        header.params.memory_kib,
        header.params.iterations,
        header.params.parallelism,
        Some(2 * KEY_LEN),
    )
    // Inside the bounds but rejected by Argon2 (memory < 8 × lanes).
    .map_err(|_| VaultError::Crypto("invalid key derivation parameters"))?;
    let mut output = Zeroizing::new([0u8; 2 * KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(
            passphrase.expose_secret().as_bytes(),
            &header.salt,
            &mut *output,
        )
        .map_err(|_| VaultError::Crypto("key derivation failed"))?;
    let mut encryption = Zeroizing::new([0u8; KEY_LEN]);
    let mut check = Zeroizing::new([0u8; KEY_LEN]);
    let (first, second) = output.split_at(KEY_LEN);
    encryption.copy_from_slice(first);
    check.copy_from_slice(second);
    Ok(DerivedKeys { encryption, check })
}

fn cipher(key: &[u8; KEY_LEN]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}

/// Tag of the empty message under the check key, AAD = header.
fn check_tag(
    keys: &DerivedKeys,
    header: &Header,
) -> std::result::Result<[u8; TAG_LEN], VaultError> {
    let mut buffer: Vec<u8> = Vec::with_capacity(TAG_LEN);
    cipher(&keys.check)
        .encrypt_in_place(
            &XNonce::from(header.check_nonce),
            &header.aad(),
            &mut buffer,
        )
        .map_err(|_| VaultError::Crypto("encryption failed"))?;
    <[u8; TAG_LEN]>::try_from(buffer).map_err(|_| VaultError::Crypto("encryption failed"))
}

fn verify_check(keys: &DerivedKeys, sealed: &Sealed) -> std::result::Result<(), VaultError> {
    let mut buffer = sealed.check_tag.to_vec();
    cipher(&keys.check)
        .decrypt_in_place(
            &XNonce::from(sealed.header.check_nonce),
            &sealed.header.aad(),
            &mut buffer,
        )
        .map_err(|_| VaultError::WrongPassphrase)
}

/// Encrypts the entries under a fresh nonce and renders the file.
fn seal(unlocked: &Unlocked) -> Result<String> {
    let plaintext = encode_content(unlocked.generation, &unlocked.entries)?;
    let mut buffer = plaintext;
    let nonce = random::<NONCE_LEN>()?;
    cipher(&unlocked.key)
        .encrypt_in_place(&XNonce::from(nonce), &unlocked.header.aad(), &mut *buffer)
        .map_err(|_| VaultError::Crypto("encryption failed"))?;
    let header = &unlocked.header;
    let file = VaultFile {
        format: VAULT_FORMAT.to_owned(),
        version: VAULT_VERSION,
        kdf: KdfSection {
            algorithm: KDF_ALGORITHM.to_owned(),
            version: ARGON2_VERSION,
            memory_kib: header.params.memory_kib,
            iterations: header.params.iterations,
            parallelism: header.params.parallelism,
            salt: B64.encode(&header.salt),
        },
        cipher: CipherSection {
            algorithm: CIPHER_ALGORITHM.to_owned(),
            check_nonce: B64.encode(header.check_nonce),
            check: B64.encode(unlocked.check_tag),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(&*buffer),
        },
    };
    let body = toml::to_string_pretty(&file).map_err(|e| crate::Error::Serde {
        what: "vault",
        message: e.to_string(),
    })?;
    Ok(format!("{FILE_COMMENT}{body}"))
}

/// `{"generation": n, "entries": {account: base64(secret)}}`, padded with
/// spaces. Written by hand into one buffer allocated up front, large enough
/// for the content, padding and tag: no reallocation leaves a stray
/// plaintext copy behind, and base64 never needs escaping.
fn encode_content(
    generation: u64,
    entries: &BTreeMap<String, SecretString>,
) -> std::result::Result<Zeroizing<Vec<u8>>, VaultError> {
    // `{"generation":<20 digits>,"entries":{` + `}}`.
    const FRAME: usize = 64;
    let fail = || VaultError::Crypto("cannot encode the secrets");
    let mut capacity = FRAME;
    for (account, secret) in entries {
        let encoded = base64::encoded_len(secret.expose_secret().len(), true).ok_or_else(fail)?;
        capacity = capacity
            .saturating_add(account.len().saturating_mul(6))
            .saturating_add(encoded)
            .saturating_add(8);
    }
    let capacity = capacity
        .saturating_add(PADDING_BLOCK)
        .saturating_add(TAG_LEN);
    let mut text = Zeroizing::new(String::with_capacity(capacity));
    text.push_str("{\"generation\":");
    text.push_str(&generation.to_string());
    text.push_str(",\"entries\":{");
    for (index, (account, secret)) in entries.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        // Accounts are identifiers, not secrets: ordinary JSON escaping.
        text.push_str(&serde_json::to_string(account).map_err(|_| fail())?);
        text.push_str(":\"");
        B64.encode_string(secret.expose_secret().as_bytes(), &mut text);
        text.push('"');
    }
    text.push_str("}}");
    let padded = text
        .len()
        .div_ceil(PADDING_BLOCK)
        .max(1)
        .saturating_mul(PADDING_BLOCK);
    while text.len() < padded {
        text.push(' ');
    }
    // Moves the allocation (no copy).
    Ok(Zeroizing::new(std::mem::take(&mut *text).into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    fn host_key(user: &str) -> SecretKey {
        SecretKey::HostPassword {
            user: user.into(),
            host: "db".into(),
            port: 22,
            host_key: "SHA256:abc".into(),
        }
    }

    fn new_vault(dir: &Path) -> VaultStore {
        let vault = VaultStore::open_for_tests(dir.join("data/vault.toml"));
        vault
            .create_in_place(&pass("correct horse"))
            .expect("create");
        vault
    }

    fn reopen(vault: &VaultStore) -> VaultStore {
        VaultStore::open_for_tests(vault.path())
    }

    fn code(e: &crate::Error) -> String {
        e.summary().code
    }

    #[test]
    fn roundtrip_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        assert_eq!(vault.status(), VaultStatus::Unlocked);
        vault.set(&host_key("root"), &pass("hunter2")).expect("set");
        vault
            .set(
                &SecretKey::KeyPassphrase {
                    fingerprint: "SHA256:k".into(),
                },
                &pass("pässphrase \"quoted\"\n"),
            )
            .expect("set");

        let text = std::fs::read_to_string(vault.path()).expect("read");
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("root@db"));
        assert!(text.contains("format = \"portico-vault\""));
        assert!(text.contains("memory_kib = 64"));

        let other = reopen(&vault);
        assert_eq!(other.status(), VaultStatus::Locked);
        other
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        let got = other.get(&host_key("root")).expect("get").expect("some");
        assert_eq!(got.expose_secret(), "hunter2");
        let got = other
            .get(&SecretKey::KeyPassphrase {
                fingerprint: "SHA256:k".into(),
            })
            .expect("get")
            .expect("some");
        assert_eq!(got.expose_secret(), "pässphrase \"quoted\"\n");
        assert!(other.get(&host_key("nobody")).expect("get").is_none());

        other.delete(&host_key("root")).expect("delete");
        other
            .delete(&host_key("root"))
            .expect("delete is idempotent");
        let third = reopen(&vault);
        third
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        assert!(third.get(&host_key("root")).expect("get").is_none());
    }

    #[test]
    fn every_write_uses_a_fresh_nonce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        let nonce = |v: &VaultStore| {
            let text = std::fs::read_to_string(v.path()).expect("read");
            let file: VaultFile = toml::from_str(&text).expect("toml");
            file.cipher.nonce
        };
        let first = nonce(&vault);
        vault.set(&host_key("a"), &pass("x")).expect("set");
        let second = nonce(&vault);
        vault.set(&host_key("a"), &pass("x")).expect("set");
        assert_ne!(first, second);
        assert_ne!(second, nonce(&vault));
    }

    #[test]
    fn writes_keep_entries_from_another_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = new_vault(dir.path());
        let b = reopen(&a);
        b.unlock_in_place(&pass("correct horse")).expect("unlock");
        a.set(&host_key("a"), &pass("1")).expect("set");
        b.set(&host_key("b"), &pass("2")).expect("set");
        let c = reopen(&a);
        c.unlock_in_place(&pass("correct horse")).expect("unlock");
        assert!(c.get(&host_key("a")).expect("get").is_some());
        assert!(c.get(&host_key("b")).expect("get").is_some());
    }

    #[test]
    fn padding_hides_the_exact_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        let len = |v: &VaultStore| {
            let text = std::fs::read_to_string(v.path()).expect("read");
            let file: VaultFile = toml::from_str(&text).expect("toml");
            B64.decode(file.cipher.ciphertext).expect("b64").len()
        };
        vault.set(&host_key("a"), &pass("x")).expect("set");
        let short = len(&vault);
        vault.set(&host_key("a"), &pass("xxxxxxxxxx")).expect("set");
        assert_eq!(short, len(&vault));
        assert_eq!((short - TAG_LEN) % PADDING_BLOCK, 0);
    }

    #[test]
    fn wrong_passphrase_is_not_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        let other = reopen(&vault);
        let e = other
            .unlock_in_place(&pass("wrong horse"))
            .expect_err("must fail");
        assert_eq!(code(&e), "vault_wrong_passphrase");
        assert!(!other.is_unlocked());
    }

    /// Rewrites one field of the file.
    fn tamper(path: &Path, edit: impl FnOnce(&mut VaultFile)) {
        let text = std::fs::read_to_string(path).expect("read");
        let mut file: VaultFile = toml::from_str(&text).expect("toml");
        edit(&mut file);
        std::fs::write(path, toml::to_string(&file).expect("toml")).expect("write");
    }

    fn flip_b64(field: &mut String, index: usize) {
        let mut bytes = B64.decode(&*field).expect("b64");
        bytes[index] ^= 0x01;
        *field = B64.encode(bytes);
    }

    fn unlock_code(vault: &VaultStore) -> String {
        let other = reopen(vault);
        let e = other
            .unlock_in_place(&pass("correct horse"))
            .expect_err("must fail");
        assert!(!other.is_unlocked());
        code(&e)
    }

    #[test]
    fn altered_ciphertext_is_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault.set(&host_key("a"), &pass("x")).expect("set");
        tamper(vault.path(), |f| flip_b64(&mut f.cipher.ciphertext, 3));
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        // The tag itself.
        let vault = new_vault(&dir.path().join("2"));
        tamper(vault.path(), |f| {
            let n = B64.decode(&f.cipher.ciphertext).expect("b64").len();
            flip_b64(&mut f.cipher.ciphertext, n - 1);
        });
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        // The nonce.
        let vault = new_vault(&dir.path().join("3"));
        tamper(vault.path(), |f| flip_b64(&mut f.cipher.nonce, 0));
        assert_eq!(unlock_code(&vault), "vault_corrupt");
    }

    #[test]
    fn altered_header_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Salt, check nonce and check tag: the passphrase no longer verifies.
        let edits: [fn(&mut VaultFile); 3] = [
            |f| flip_b64(&mut f.kdf.salt, 0),
            |f| flip_b64(&mut f.cipher.check_nonce, 5),
            |f| flip_b64(&mut f.cipher.check, 0),
        ];
        for (i, edit) in edits.into_iter().enumerate() {
            let vault = new_vault(&dir.path().join(i.to_string()));
            tamper(vault.path(), edit);
            assert_eq!(unlock_code(&vault), "vault_wrong_passphrase", "edit {i}");
        }
        // KDF parameters inside the bounds: still authenticated.
        let vault = new_vault(&dir.path().join("m"));
        tamper(vault.path(), |f| f.kdf.memory_kib += 8);
        assert_eq!(unlock_code(&vault), "vault_wrong_passphrase");
        let vault = new_vault(&dir.path().join("t"));
        tamper(vault.path(), |f| f.kdf.iterations += 1);
        assert_eq!(unlock_code(&vault), "vault_wrong_passphrase");
        // Structure.
        let vault = new_vault(&dir.path().join("v"));
        tamper(vault.path(), |f| f.version = VAULT_VERSION + 1);
        assert_eq!(unlock_code(&vault), "vault_version");
        let vault = new_vault(&dir.path().join("a"));
        tamper(vault.path(), |f| f.cipher.algorithm = "aes-256-gcm".into());
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        let vault = new_vault(&dir.path().join("k"));
        tamper(vault.path(), |f| f.kdf.algorithm = "argon2i".into());
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        let vault = new_vault(&dir.path().join("s"));
        tamper(vault.path(), |f| f.kdf.salt = B64.encode([0u8; 8]));
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        let vault = new_vault(&dir.path().join("n"));
        tamper(vault.path(), |f| f.cipher.nonce = "not base64!".into());
        assert_eq!(unlock_code(&vault), "vault_corrupt");
        let vault = new_vault(&dir.path().join("g"));
        std::fs::write(vault.path(), "garbage = [").expect("write");
        assert_eq!(unlock_code(&vault), "vault_corrupt");
    }

    #[test]
    fn kdf_parameters_out_of_bounds_are_refused_before_deriving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        // The test vault (64 KiB, 1 pass) is below the production floor.
        let production = VaultStore::open(vault.path());
        let e = production
            .unlock_in_place(&pass("correct horse"))
            .expect_err("floor");
        assert_eq!(code(&e), "vault_kdf_out_of_bounds");
        assert!(matches!(
            e,
            crate::Error::Vault(VaultError::KdfOutOfBounds {
                param: "memory_kib",
                ..
            })
        ));
        // A hostile 8 KiB / huge-cost file, refused instantly (no freeze).
        let hostile: [fn(&mut VaultFile); 4] = [
            |f| f.kdf.memory_kib = 8,
            |f| f.kdf.memory_kib = u32::MAX,
            |f| f.kdf.iterations = 1_000_000,
            |f| f.kdf.parallelism = 1024,
        ];
        for (i, edit) in hostile.into_iter().enumerate() {
            tamper(vault.path(), edit);
            let started = std::time::Instant::now();
            for store in [VaultStore::open(vault.path()), reopen(&vault)] {
                let e = store
                    .unlock_in_place(&pass("correct horse"))
                    .expect_err("bounds");
                assert_eq!(code(&e), "vault_kdf_out_of_bounds", "edit {i}");
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            tamper(vault.path(), |f| {
                f.kdf.memory_kib = 64;
                f.kdf.iterations = 1;
                f.kdf.parallelism = 1;
            });
        }
        // Production defaults sit inside the production bounds, and are the
        // floor: a file cannot ask for a weaker derivation.
        KdfBounds::PRODUCTION
            .check(&KdfParams::DEFAULT)
            .expect("defaults in bounds");
        const {
            let b = KdfBounds::PRODUCTION;
            let d = KdfParams::DEFAULT;
            assert!(b.min.memory_kib == d.memory_kib && d.memory_kib == 64 * 1024);
            assert!(b.min.iterations == d.iterations && d.iterations == 3);
            assert!(b.min.parallelism == d.parallelism && d.parallelism == 1);
            assert!(b.max.memory_kib == 256 * 1024);
            assert!(b.max.iterations == 8);
            assert!(b.max.parallelism == 4);
        };
        for (param, below, above) in [
            ("memory_kib", 64 * 1024 - 1, 256 * 1024 + 1),
            ("iterations", 2, 9),
            ("parallelism", 0, 5),
        ] {
            for value in [below, above] {
                let mut params = KdfParams::DEFAULT;
                match param {
                    "memory_kib" => params.memory_kib = value,
                    "iterations" => params.iterations = value,
                    _ => params.parallelism = value,
                }
                assert!(
                    KdfBounds::PRODUCTION.check(&params).is_err(),
                    "{param} = {value}"
                );
            }
        }
    }

    #[test]
    fn create_refuses_an_existing_file_and_short_passphrases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        let again = reopen(&vault);
        let e = again
            .create_in_place(&pass("another one"))
            .expect_err("exists");
        assert_eq!(code(&e), "vault_exists");
        assert!(matches!(
            VaultStore::create(vault.path(), &pass("another one")),
            Err(crate::Error::Vault(VaultError::Exists { .. }))
        ));
        // The original is intact.
        reopen(&vault)
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");

        let fresh = VaultStore::open_for_tests(dir.path().join("other.toml"));
        let e = fresh.create_in_place(&pass("short")).expect_err("short");
        assert_eq!(code(&e), "vault_passphrase_too_short");
        assert_eq!(fresh.status(), VaultStatus::Absent);
    }

    #[test]
    fn missing_vault() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = VaultStore::open_for_tests(dir.path().join("vault.toml"));
        assert_eq!(vault.status(), VaultStatus::Absent);
        let e = vault
            .unlock_in_place(&pass("correct horse"))
            .expect_err("missing");
        assert_eq!(code(&e), "vault_missing");
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        let mode = |v: &VaultStore| {
            std::fs::metadata(v.path())
                .expect("meta")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&vault), 0o600);
        vault.set(&host_key("a"), &pass("x")).expect("set");
        assert_eq!(mode(&vault), 0o600);
        vault
            .change_passphrase(&pass("correct horse"), &pass("battery staple"))
            .expect("rekey");
        assert_eq!(mode(&vault), 0o600);
    }

    #[test]
    fn debug_and_errors_are_redacted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault.set(&host_key("root"), &pass("hunter2")).expect("set");
        let debug = format!("{vault:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains("correct horse"), "{debug}");
        assert!(debug.contains("unlocked: true"), "{debug}");
        let state = vault.state.lock().expect("lock");
        let inner = format!("{state:?}");
        assert!(
            !inner.contains("hunter2") && !inner.contains("root@db"),
            "{inner}"
        );
        drop(state);
        let e = reopen(&vault)
            .unlock_in_place(&pass("hunter2 wrong"))
            .expect_err("wrong");
        let summary = e.summary();
        assert!(!summary.message.contains("hunter2"));
        assert!(summary.hint.is_some());
    }

    #[test]
    fn lock_forgets_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault.set(&host_key("a"), &pass("x")).expect("set");
        vault.lock();
        assert_eq!(vault.status(), VaultStatus::Locked);
        for result in [
            vault.get(&host_key("a")).map(|_| ()),
            vault.set(&host_key("a"), &pass("y")),
            vault.delete(&host_key("a")),
        ] {
            let e = result.expect_err("locked");
            assert_eq!(code(&e), "vault_locked");
            assert!(e.summary().hint.is_some());
        }
        vault
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        assert_eq!(
            vault
                .get(&host_key("a"))
                .expect("get")
                .expect("some")
                .expose_secret(),
            "x"
        );
    }

    #[test]
    fn change_passphrase_rekeys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault.set(&host_key("a"), &pass("x")).expect("set");
        let salt = |v: &VaultStore| {
            let text = std::fs::read_to_string(v.path()).expect("read");
            toml::from_str::<VaultFile>(&text).expect("toml").kdf.salt
        };
        let before = salt(&vault);

        let e = vault
            .change_passphrase(&pass("not the one"), &pass("battery staple"))
            .expect_err("old must match");
        assert_eq!(code(&e), "vault_wrong_passphrase");
        let e = vault
            .change_passphrase(&pass("correct horse"), &pass("short"))
            .expect_err("too short");
        assert_eq!(code(&e), "vault_passphrase_too_short");

        vault
            .change_passphrase(&pass("correct horse"), &pass("battery staple"))
            .expect("rekey");
        assert_ne!(before, salt(&vault));
        assert!(vault.is_unlocked());
        vault
            .set(&host_key("b"), &pass("y"))
            .expect("set after rekey");

        assert_eq!(unlock_code(&vault), "vault_wrong_passphrase");
        let other = reopen(&vault);
        other
            .unlock_in_place(&pass("battery staple"))
            .expect("unlock");
        assert!(other.get(&host_key("a")).expect("get").is_some());
        assert!(other.get(&host_key("b")).expect("get").is_some());

        // A store still holding the old key notices and locks itself.
        let stale = reopen(&vault);
        stale
            .unlock_in_place(&pass("battery staple"))
            .expect("unlock");
        vault
            .change_passphrase(&pass("battery staple"), &pass("third passphrase"))
            .expect("rekey");
        let e = stale.set(&host_key("c"), &pass("z")).expect_err("stale");
        assert_eq!(code(&e), "vault_changed");
        assert!(!stale.is_unlocked());
    }

    #[test]
    fn rekey_is_not_reverted_by_a_concurrent_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rekeyer = new_vault(dir.path());
        rekeyer.set(&host_key("a"), &pass("x")).expect("set");
        let writer = reopen(&rekeyer);
        writer
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                start.wait();
                rekeyer
                    .change_passphrase(&pass("correct horse"), &pass("battery staple"))
                    .expect("rekey");
            });
            scope.spawn(|| {
                start.wait();
                for i in 0..200 {
                    if let Err(e) = writer.set(&host_key(&format!("w{i}")), &pass("y")) {
                        // The writer notices the new key and stops.
                        assert_eq!(code(&e), "vault_changed");
                        assert!(!writer.is_unlocked());
                        break;
                    }
                }
            });
        });
        let after = reopen(&rekeyer);
        after
            .unlock_in_place(&pass("battery staple"))
            .expect("the new passphrase still opens the vault");
        assert!(after.get(&host_key("a")).expect("get").is_some());
        let old = reopen(&rekeyer);
        let e = old
            .unlock_in_place(&pass("correct horse"))
            .expect_err("the old passphrase is gone");
        assert_eq!(code(&e), "vault_wrong_passphrase");
        // The lock file sits next to the vault, private.
        let lock = dir.path().join("data/vault.toml.lock");
        assert!(lock.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&lock).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn older_ciphertext_is_refused_while_unlocked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault.set(&host_key("a"), &pass("x")).expect("set");
        let older = std::fs::read(vault.path()).expect("read");
        vault.set(&host_key("b"), &pass("y")).expect("set");
        // Same key and header, authentic but older content put back.
        std::fs::write(vault.path(), &older).expect("restore");
        let e = vault.set(&host_key("c"), &pass("z")).expect_err("rollback");
        assert_eq!(code(&e), "vault_rollback");
        assert!(e.summary().hint.is_some());
        assert!(!vault.is_unlocked());
        // The file was not rewritten.
        assert_eq!(std::fs::read(vault.path()).expect("read"), older);
        // Known limit: a fresh unlock accepts the older file.
        vault
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        assert!(vault.get(&host_key("b")).expect("get").is_none());
        vault.set(&host_key("c"), &pass("z")).expect("set");
    }

    #[test]
    fn content_is_versioned_and_base64() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        vault
            .set(&host_key("root"), &pass("quote\" back\\ \u{1}"))
            .expect("set");
        vault.set(&host_key("other"), &pass("")).expect("set");
        let text = std::fs::read_to_string(vault.path()).expect("read");
        assert!(text.contains("version = 2"), "{text}");
        let sealed = vault.parse(&text).expect("parse");
        let state = vault.state.lock().expect("lock");
        let unlocked = state.as_ref().expect("unlocked");
        let mut buffer = sealed.ciphertext.clone();
        cipher(&unlocked.key)
            .decrypt_in_place(
                &XNonce::from(sealed.nonce),
                &sealed.header.aad(),
                &mut buffer,
            )
            .expect("decrypt");
        let json: serde_json::Value = serde_json::from_slice(&buffer).expect("json");
        assert_eq!(json["generation"], 2);
        assert_eq!(
            json["entries"]["password:root@db:22#SHA256:abc"],
            B64.encode("quote\" back\\ \u{1}")
        );
        // No escape sequence in the whole plaintext but the account names'.
        assert!(!buffer.windows(2).any(|w| w == b"\\u"));
        drop(state);
        let other = reopen(&vault);
        other
            .unlock_in_place(&pass("correct horse"))
            .expect("unlock");
        assert_eq!(
            other
                .get(&host_key("root"))
                .expect("get")
                .expect("some")
                .expose_secret(),
            "quote\" back\\ \u{1}"
        );
        assert_eq!(
            other
                .get(&host_key("other"))
                .expect("get")
                .expect("some")
                .expose_secret(),
            ""
        );
    }

    #[test]
    fn version_1_is_refused_with_a_recreate_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = new_vault(dir.path());
        tamper(vault.path(), |f| f.version = 1);
        let e = reopen(&vault)
            .unlock_in_place(&pass("correct horse"))
            .expect_err("v1");
        let summary = e.summary();
        assert_eq!(summary.code, "vault_version");
        assert!(summary.hint.expect("hint").contains("recreate the vault"));
        // A write through a handle unlocked before also locks it.
        let e = vault.set(&host_key("a"), &pass("x")).expect_err("v1");
        assert_eq!(code(&e), "vault_version");
        assert!(!vault.is_unlocked());
    }

    #[test]
    fn production_open_is_locked_and_cheap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault = VaultStore::open(dir.path().join("vault.toml"));
        assert_eq!(vault.status(), VaultStatus::Absent);
        assert!(vault.get(&host_key("a")).is_err());
        assert!(vault.vault().is_some());
    }

    /// The real parameters (64 MiB, 3 passes) through the public
    /// constructors: one derivation per call, dependencies are optimized in
    /// dev builds.
    #[test]
    fn production_parameters_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("vault.toml");
        let vault = VaultStore::create(&path, &pass("correct horse")).expect("create");
        vault.set(&host_key("a"), &pass("x")).expect("set");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("memory_kib = 65536"), "{text}");
        assert!(text.contains("iterations = 3"), "{text}");
        assert!(matches!(
            VaultStore::unlock(&path, &pass("wrong horse")),
            Err(crate::Error::Vault(VaultError::WrongPassphrase))
        ));
        let again = VaultStore::unlock(&path, &pass("correct horse")).expect("unlock");
        assert!(again.get(&host_key("a")).expect("get").is_some());
    }
}
