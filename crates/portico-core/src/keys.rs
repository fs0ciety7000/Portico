//! Local SSH keys, OpenSSH certificates, generation and `ssh-copy-id`.
//!
//! All key parsing goes through `russh::keys::ssh_key` (re-exported by russh,
//! pinned to the exact version russh uses) so there is a single key model in
//! the process.
//!
//! - Discovery: implemented (this module).
//! - Generation (`ed25519` default, `rsa` ≥ 3072, `ecdsa` P-256/384/521),
//!   encrypted with a passphrase (bcrypt-pbkdf, OpenSSH format): Phase 3.
//! - Copy-id (append to remote `authorized_keys` atomically, idempotent,
//!   correct permissions): Phase 3.

use std::path::{Path, PathBuf};

use russh::keys::ssh_key::{Certificate, PublicKey};
use russh::keys::{EcdsaCurve, HashAlg};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Algorithm choice offered by the key generation form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyAlgorithm {
    /// Recommended default.
    #[default]
    Ed25519,
    /// `bits` must be ≥ 3072 (enforced at generation time).
    Rsa {
        bits: u32,
    },
    Ecdsa {
        curve: Curve,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    P256,
    P384,
    P521,
}

impl From<Curve> for EcdsaCurve {
    fn from(c: Curve) -> Self {
        match c {
            Curve::P256 => Self::NistP256,
            Curve::P384 => Self::NistP384,
            Curve::P521 => Self::NistP521,
        }
    }
}

/// Minimum RSA modulus accepted by the generator.
pub const MIN_RSA_BITS: u32 = 3072;

/// Summary of an OpenSSH certificate attached to a key (`*-cert.pub`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateInfo {
    pub path: PathBuf,
    pub key_id: String,
    pub principals: Vec<String>,
    /// Unix seconds.
    pub valid_after: u64,
    /// Unix seconds (`u64::MAX` = forever).
    pub valid_before: u64,
    pub ca_fingerprint: String,
}

impl CertificateInfo {
    pub fn is_valid_at(&self, unix_now: u64) -> bool {
        self.valid_after <= unix_now && unix_now < self.valid_before
    }
}

/// A key pair found on disk (never holds private material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalKey {
    /// Path of the private key (may not exist if only the `.pub` is present).
    pub private_path: PathBuf,
    pub public_path: PathBuf,
    pub algorithm: String,
    /// `SHA256:…`, as printed by `ssh-keygen -l`.
    pub fingerprint: String,
    pub comment: String,
    pub has_private: bool,
    pub certificate: Option<CertificateInfo>,
}

/// Lists key pairs in `ssh_dir` by scanning `*.pub` files.
///
/// Unreadable or malformed files are skipped (they are reported through
/// tracing, never fatal), so a single odd file cannot break the key view.
pub fn discover(ssh_dir: &Path) -> Result<Vec<LocalKey>> {
    let entries = match std::fs::read_dir(ssh_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(ssh_dir, e)),
    };

    let mut keys = Vec::new();
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // OpenSSH file names are case-sensitive: `.PUB` is not a public key.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        if !name.ends_with(".pub") || name.ends_with("-cert.pub") {
            continue;
        }
        match read_key(&path) {
            Ok(key) => keys.push(key),
            Err(e) => tracing::debug!(path = %path.display(), error = %e, "skipping public key"),
        }
    }
    keys.sort_by(|a, b| a.public_path.cmp(&b.public_path));
    Ok(keys)
}

fn read_key(public_path: &Path) -> Result<LocalKey> {
    let text = std::fs::read_to_string(public_path).map_err(|e| Error::io(public_path, e))?;
    let public = PublicKey::from_openssh(text.trim()).map_err(|e| Error::Serde {
        what: "openssh public key",
        message: e.to_string(),
    })?;
    let stem = public_path.to_string_lossy();
    let private_path = PathBuf::from(stem.trim_end_matches(".pub"));
    let cert_path = PathBuf::from(format!("{}-cert.pub", stem.trim_end_matches(".pub")));

    Ok(LocalKey {
        has_private: private_path.is_file(),
        private_path,
        public_path: public_path.to_path_buf(),
        algorithm: public.algorithm().to_string(),
        fingerprint: public.fingerprint(HashAlg::Sha256).to_string(),
        comment: public.comment().to_string(),
        certificate: read_certificate(&cert_path),
    })
}

fn read_certificate(path: &Path) -> Option<CertificateInfo> {
    let text = std::fs::read_to_string(path).ok()?;
    let cert = Certificate::from_openssh(text.trim()).ok()?;
    let ca = PublicKey::from(cert.signature_key().clone());
    Some(CertificateInfo {
        path: path.to_path_buf(),
        key_id: cert.key_id().to_owned(),
        principals: cert.valid_principals().to_vec(),
        valid_after: cert.valid_after(),
        valid_before: cert.valid_before(),
        ca_fingerprint: ca.fingerprint(HashAlg::Sha256).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Throwaway test key (public half only).
    const ED25519_PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFPhl4hO0pS1bxgFwR0cIGijellPogqzFcdSn+al5ZRJ user@example.com";

    #[test]
    fn discovers_public_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("id_test.pub"), ED25519_PUB).expect("write");
        std::fs::write(dir.path().join("id_test"), "private").expect("write");
        std::fs::write(dir.path().join("garbage.pub"), "not a key").expect("write");
        std::fs::write(dir.path().join("config"), "Host x").expect("write");

        let keys = discover(dir.path()).expect("discover");
        assert_eq!(keys.len(), 1);
        let key = &keys[0];
        assert_eq!(key.algorithm, "ssh-ed25519");
        assert_eq!(key.comment, "user@example.com");
        assert!(key.fingerprint.starts_with("SHA256:"));
        assert!(key.has_private);
        assert!(key.certificate.is_none());
    }

    #[test]
    fn missing_dir_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(discover(&dir.path().join("nope")).expect("ok").is_empty());
    }
}
