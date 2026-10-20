//! Secret storage. **No secret is ever written in clear text.**
//!
//! Backends, in order of preference:
//! 1. [`KeyringStore`]: macOS Keychain / Linux Secret Service (GNOME Keyring,
//!    `KWallet` via Secret Service) through the `keyring` crate.
//! 2. `VaultStore` (Phase 3): local file encrypted with XChaCha20-Poly1305,
//!    key derived from a master passphrase with Argon2id. Used when no system
//!    keyring is available (headless Linux, SSH sessions).
//!
//! Secrets are passed around as [`SecretString`] (redacted `Debug`, zeroized
//! on drop) and must never reach logs, events or error messages.

use secrecy::{ExposeSecret, SecretString};

use crate::error::{Error, Result};

/// Keyring "service" name for every Portico entry.
pub const SERVICE: &str = "dev.portico";

/// What a secret unlocks. Rendered as the keyring account name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretKey {
    /// Password for `user@host:port`.
    HostPassword {
        user: String,
        host: String,
        port: u16,
    },
    /// Passphrase of a private key file.
    KeyPassphrase { fingerprint: String },
}

impl SecretKey {
    pub fn account(&self) -> String {
        match self {
            Self::HostPassword { user, host, port } => format!("password:{user}@{host}:{port}"),
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
        };
        assert_eq!(k.account(), "password:root@db:22");
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let s = SecretString::from("hunter2".to_owned());
        assert!(!format!("{s:?}").contains("hunter2"));
    }
}
