//! Secret storage. **No secret is ever written in clear text.**
//!
//! Backends, in order of preference:
//! 1. [`KeyringStore`]: macOS Keychain / Linux Secret Service (GNOME Keyring,
//!    `KWallet` via Secret Service) through the `keyring` crate.
//! 2. [`VaultStore`]: local file encrypted with XChaCha20-Poly1305, key
//!    derived from a master passphrase with Argon2id (see [`crate::vault`]).
//!    Used when no system keyring is available (headless Linux, SSH
//!    sessions). [`default_store`] picks the backend.
//!
//! Secrets are passed around as [`SecretString`] (redacted `Debug`, zeroized
//! on drop) and must never reach logs, events or error messages.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};

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
        }
    }
}

/// Backend-agnostic secret storage. Implementations are blocking: callers in
/// async contexts must use `tokio::task::spawn_blocking`.
pub trait SecretStore: Send + Sync + std::fmt::Debug {
    fn get(&self, key: &SecretKey) -> Result<Option<SecretString>>;
    fn set(&self, key: &SecretKey, secret: &SecretString) -> Result<()>;
    fn delete(&self, key: &SecretKey) -> Result<()>;

    /// The vault behind this store, when it is one (lock / unlock from the
    /// [`crate::Engine`]).
    fn vault(&self) -> Option<&VaultStore> {
        None
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

    fn entry(key: &SecretKey) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, &key.account()).map_err(keyring_err)
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

    #[test]
    fn account_names_are_stable() {
        let k = SecretKey::HostPassword {
            user: "root".into(),
            host: "db".into(),
            port: 22,
            host_key: "SHA256:abc".into(),
        };
        assert_eq!(k.account(), "password:root@db:22#SHA256:abc");
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let s = SecretString::from("hunter2".to_owned());
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
