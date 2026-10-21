//! Local SSH keys, OpenSSH certificates, generation and `ssh-copy-id`.
//!
//! All key parsing goes through `russh::keys::ssh_key` (re-exported by russh,
//! pinned to the exact version russh uses) so there is a single key model in
//! the process.
//!
//! - Discovery of key pairs and their certificates.
//! - Generation ([`generate`]): `ed25519` (default), `rsa` ≥ 3072, `ecdsa`
//!   P-256/384/521, optionally encrypted with a passphrase (OpenSSH format,
//!   bcrypt-pbkdf + aes256-ctr, like `ssh-keygen`). The private key is
//!   created exclusively in `0600`, the `.pub` in `0644`: an existing file is
//!   never overwritten, and a half-written pair is removed.
//! - Copy-id: see `Engine::copy_id`.
//!
//! Generation is CPU-bound (RSA can take seconds) and does blocking I/O:
//! async callers go through `spawn_blocking`.

use std::io::Write;
use std::path::{Path, PathBuf};

use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
use russh::keys::ssh_key::{Algorithm, Certificate, LineEnding, PrivateKey, PublicKey};
use russh::keys::{EcdsaCurve, HashAlg};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Key generation and copy-id failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    #[error("{} already exists", path.display())]
    Exists { path: PathBuf },
    #[error("invalid key file name {name:?}: {reason}")]
    InvalidName { name: String, reason: &'static str },
    #[error("RSA keys must be {MIN_RSA_BITS} to {MAX_RSA_BITS} bits (got {bits})")]
    RsaSize { bits: u32 },
    #[error("invalid key comment: {reason}")]
    InvalidComment { reason: &'static str },
    #[error("key generation failed: {0}")]
    Generate(String),
    #[error("{} is not a usable public key: {reason}", path.display())]
    BadPublicKey { path: PathBuf, reason: String },
    #[error("installing the key on {host} failed: {reason}")]
    CopyFailed { host: String, reason: String },
    #[error("the key was installed on {host} but logging in with it failed")]
    NotAccepted { host: String },
    #[error("{host} ran the command but did not report a result")]
    CopyUnexpectedOutput { host: String },
    /// The verification login failed for another reason (host key
    /// changed, unreachable…): reported with that error's own code.
    #[error("{}", summary.message)]
    ProbeFailed { summary: crate::error::ErrorSummary },
    #[error("{} can be written by other accounts", path.display())]
    UnsafeDir { path: PathBuf },
    #[error("no ssh-agent is reachable{}", socket.as_ref().map(|s| format!(" at {}", s.display())).unwrap_or_default())]
    AgentUnavailable { socket: Option<PathBuf> },
    #[error("ssh-agent refused the request: {0}")]
    AgentRefused(String),
    #[error("{} is encrypted", path.display())]
    NeedsPassphrase { path: PathBuf },
    #[error("wrong passphrase for {}", path.display())]
    WrongPassphrase { path: PathBuf },
    #[error("cannot read the private key {}: {reason}", path.display())]
    BadPrivateKey { path: PathBuf, reason: String },
    #[error("no identity {fingerprint} in ssh-agent")]
    NotInAgent { fingerprint: String },
}

impl KeyError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Exists { .. } => (
                "key_exists",
                Some("Pick another name: Portico never overwrites a key."),
            ),
            Self::InvalidName { .. } => (
                "key_name",
                Some("Use letters, digits, dot, dash or underscore, like id_ed25519_work."),
            ),
            Self::RsaSize { .. } => ("key_rsa_size", Some("Use 3072 or 4096 bits, or Ed25519.")),
            Self::InvalidComment { .. } => (
                "key_comment",
                Some("Use a short single-line comment, like you@laptop."),
            ),
            Self::Generate(_) => ("key_generate", None),
            Self::BadPublicKey { .. } => {
                ("key_public", Some("Choose the .pub file of a key pair."))
            }
            Self::CopyFailed { .. } => (
                "copy_id_failed",
                Some("Check that the account has a writable home and a POSIX shell."),
            ),
            Self::UnsafeDir { .. } => (
                "key_unsafe_dir",
                Some("Make it yours and private: chmod 700 ~/.ssh."),
            ),
            Self::AgentUnavailable { .. } => (
                "agent_unavailable",
                Some("Start ssh-agent (eval $(ssh-agent)) or set IdentityAgent / SSH_AUTH_SOCK."),
            ),
            Self::AgentRefused(_) => (
                "agent_refused",
                Some("The agent may be locked (ssh-add -X) or not accept this key type."),
            ),
            Self::NeedsPassphrase { .. } => ("key_needs_passphrase", None),
            Self::WrongPassphrase { .. } => ("key_wrong_passphrase", Some("Try again.")),
            Self::BadPrivateKey { .. } => (
                "key_private",
                Some("Choose a private key in OpenSSH format (the file without .pub)."),
            ),
            Self::NotInAgent { .. } => ("agent_no_identity", Some("Refresh the list.")),
            Self::ProbeFailed { .. } => ("copy_id_probe", None),
            Self::CopyUnexpectedOutput { .. } => (
                "copy_id_unexpected_output",
                Some(
                    "The account may be restricted (forced command, SFTP only) or not run a POSIX shell.",
                ),
            ),
            Self::NotAccepted { .. } => (
                "copy_id_not_accepted",
                Some(
                    "If you dismissed the passphrase, verify again. Otherwise the server may ignore ~/.ssh/authorized_keys (AuthorizedKeysFile, StrictModes or ownership).",
                ),
            ),
        }
    }
}

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
/// Maximum RSA modulus (larger keys take minutes and bring nothing).
pub const MAX_RSA_BITS: u32 = 8192;
/// Longest key file name.
const MAX_NAME_LEN: usize = 64;
/// Longest key comment.
const MAX_COMMENT_LEN: usize = 256;

/// What the key generation form asks for. The passphrase stays a
/// [`SecretString`]: empty means "no passphrase".
#[derive(Debug)]
pub struct KeyGenRequest {
    /// File name in `~/.ssh` (`id_ed25519_work`); the `.pub` is added.
    pub name: String,
    pub algorithm: KeyAlgorithm,
    pub comment: String,
    pub passphrase: Option<SecretString>,
}

/// Generates a key pair in `ssh_dir` (created `0700` if missing). Blocking.
pub fn generate(ssh_dir: &Path, request: &KeyGenRequest) -> Result<LocalKey> {
    validate_name(&request.name)?;
    validate_comment(&request.comment)?;
    if let KeyAlgorithm::Rsa { bits } = request.algorithm
        && !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&bits)
    {
        return Err(KeyError::RsaSize { bits }.into());
    }
    create_private_dir(ssh_dir)?;
    let private_path = ssh_dir.join(&request.name);
    let public_path = ssh_dir.join(format!("{}.pub", request.name));
    for path in [&private_path, &public_path] {
        // symlink_metadata: a dangling symlink counts as taken.
        if path.symlink_metadata().is_ok() {
            return Err(KeyError::Exists { path: path.clone() }.into());
        }
    }

    // The OS generator itself (getrandom), as the project requires. Probed
    // first so a broken entropy source is an error, not a panic inside
    // `UnwrapErr`.
    {
        use rand::TryRng;
        rand::rngs::SysRng
            .try_next_u32()
            .map_err(|e| KeyError::Generate(format!("no system randomness: {e}")))?;
    }
    let mut rng = rand::rand_core::UnwrapErr(rand::rngs::SysRng);
    let gen_err = |e: russh::keys::ssh_key::Error| KeyError::Generate(e.to_string());
    let mut key = match request.algorithm {
        KeyAlgorithm::Ed25519 => PrivateKey::random(&mut rng, Algorithm::Ed25519),
        KeyAlgorithm::Ecdsa { curve } => PrivateKey::random(
            &mut rng,
            Algorithm::Ecdsa {
                curve: curve.into(),
            },
        ),
        KeyAlgorithm::Rsa { bits } => usize::try_from(bits)
            .map_err(|_| russh::keys::ssh_key::Error::Crypto)
            .and_then(|bits| RsaKeypair::random(&mut rng, bits))
            .and_then(|pair| PrivateKey::new(KeypairData::from(pair), "")),
    }
    .map_err(gen_err)?;
    key.set_comment(request.comment.as_str());
    let passphrase = request
        .passphrase
        .as_ref()
        .filter(|p| !p.expose_secret().is_empty());
    if let Some(passphrase) = passphrase {
        key = key
            .encrypt(&mut rng, passphrase.expose_secret().as_bytes())
            .map_err(gen_err)?;
    }
    let private_text = key.to_openssh(LineEnding::LF).map_err(gen_err)?;
    let public_text = key.public_key().to_openssh().map_err(gen_err)?;

    write_new(&private_path, private_text.as_bytes(), 0o600)?;
    if let Err(e) = write_new(&public_path, format!("{public_text}\n").as_bytes(), 0o644) {
        // Never leave a private key without its public half.
        let _ = std::fs::remove_file(&private_path);
        return Err(e);
    }
    read_key(&public_path)
}

fn validate_name(name: &str) -> Result<()> {
    let reason = if name.is_empty() {
        Some("empty")
    } else if name.len() > MAX_NAME_LEN {
        Some("too long")
    } else if name.starts_with('.') || name.starts_with('-') {
        Some("must start with a letter or digit")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Some("only letters, digits, dot, dash and underscore")
    } else if [".pub", "-cert", "-cert.pub"]
        .iter()
        .any(|s| name.ends_with(s))
        || [
            "config",
            "known_hosts",
            "known_hosts.old",
            "authorized_keys",
            "environment",
            "rc",
        ]
        .contains(&name)
    {
        Some("reserved by OpenSSH")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(KeyError::InvalidName {
            name: name.to_owned(),
            reason,
        }
        .into()),
        None => Ok(()),
    }
}

fn validate_comment(comment: &str) -> Result<()> {
    let reason = if comment.chars().count() > MAX_COMMENT_LEN {
        Some("too long")
    } else if comment
        .chars()
        .any(|c| c.is_control() || is_bidi_control(c))
    {
        Some("control or bidirectional characters, or line breaks")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(KeyError::InvalidComment { reason }.into()),
        None => Ok(()),
    }
}

/// Bidirectional overrides and isolates: they can make a comment display
/// as something else (`evil\u{202E}moc.liame`).
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn create_private_dir(dir: &Path) -> Result<()> {
    // `metadata` follows a symlinked ~/.ssh: what matters is the directory.
    if let Ok(meta) = std::fs::metadata(dir) {
        if !meta.is_dir() {
            return Err(Error::io(
                dir,
                std::io::Error::from(std::io::ErrorKind::NotADirectory),
            ));
        }
        // Another account able to write here could swap the new pair.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if meta.uid() != current_uid()? || meta.permissions().mode() & 0o022 != 0 {
                return Err(KeyError::UnsafeDir {
                    path: dir.to_path_buf(),
                }
                .into());
            }
        }
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| Error::io(dir, e))
}

/// The uid of this process: the owner of a file it creates (no `unsafe`
/// libc call).
#[cfg(unix)]
fn current_uid() -> Result<u32> {
    use std::os::unix::fs::MetadataExt;
    let dir = std::env::temp_dir();
    // Random name: concurrent generations (threads, processes) never meet.
    let probe = dir.join(format!(
        ".portico-uid-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata());
    let _ = std::fs::remove_file(&probe);
    created.map(|m| m.uid()).map_err(|e| Error::io(&dir, e))
}

/// Creates `path` exclusively (never follows or replaces anything).
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, mode);
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = opts.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => KeyError::Exists {
            path: path.to_path_buf(),
        }
        .into(),
        _ => Error::io(path, e),
    })?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(e) = written {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Error::io(path, e));
    }
    Ok(())
}

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

/// A certificate expiring within this many seconds deserves a warning.
pub const CERT_EXPIRY_WARNING_SECS: u64 = 7 * 24 * 3600;

/// Where a certificate stands at a given time (for badges and warnings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CertValidity {
    NotYetValid { starts_in_secs: u64 },
    Valid { expires_in_secs: Option<u64> },
    ExpiresSoon { expires_in_secs: u64 },
    Expired { since_secs: u64 },
}

impl CertificateInfo {
    pub fn is_valid_at(&self, unix_now: u64) -> bool {
        self.valid_after <= unix_now && unix_now < self.valid_before
    }

    pub fn validity_at(&self, unix_now: u64) -> CertValidity {
        cert_validity(self.valid_after, self.valid_before, unix_now)
    }
}

pub(crate) fn cert_validity(valid_after: u64, valid_before: u64, unix_now: u64) -> CertValidity {
    if unix_now < valid_after {
        return CertValidity::NotYetValid {
            starts_in_secs: valid_after - unix_now,
        };
    }
    if unix_now >= valid_before {
        return CertValidity::Expired {
            since_secs: unix_now - valid_before,
        };
    }
    if valid_before == u64::MAX {
        return CertValidity::Valid {
            expires_in_secs: None,
        };
    }
    let left = valid_before - unix_now;
    if left <= CERT_EXPIRY_WARNING_SECS {
        CertValidity::ExpiresSoon {
            expires_in_secs: left,
        }
    } else {
        CertValidity::Valid {
            expires_in_secs: Some(left),
        }
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

    fn request(name: &str, algorithm: KeyAlgorithm, passphrase: Option<&str>) -> KeyGenRequest {
        KeyGenRequest {
            name: name.into(),
            algorithm,
            comment: "me@laptop".into(),
            passphrase: passphrase.map(|p| SecretString::from(p.to_owned())),
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn generates_every_algorithm() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        for (name, algorithm, expected) in [
            ("id_ed25519", KeyAlgorithm::Ed25519, "ssh-ed25519"),
            (
                "id_p256",
                KeyAlgorithm::Ecdsa { curve: Curve::P256 },
                "ecdsa-sha2-nistp256",
            ),
            (
                "id_p384",
                KeyAlgorithm::Ecdsa { curve: Curve::P384 },
                "ecdsa-sha2-nistp384",
            ),
            (
                "id_p521",
                KeyAlgorithm::Ecdsa { curve: Curve::P521 },
                "ecdsa-sha2-nistp521",
            ),
            ("id_rsa", KeyAlgorithm::Rsa { bits: 3072 }, "ssh-rsa"),
        ] {
            let key = generate(&ssh, &request(name, algorithm, None)).unwrap();
            assert_eq!(key.algorithm, expected);
            assert_eq!(key.comment, "me@laptop");
            assert!(key.has_private);
            let private = PrivateKey::read_openssh_file(&key.private_path).unwrap();
            assert!(!private.is_encrypted());
            assert_eq!(
                private
                    .public_key()
                    .fingerprint(HashAlg::Sha256)
                    .to_string(),
                key.fingerprint
            );
            #[cfg(unix)]
            {
                assert_eq!(mode(&key.private_path), 0o600);
                // 0644 minus the process umask.
                assert_eq!(mode(&key.public_path) & 0o600, 0o600);
                assert_eq!(mode(&key.public_path) & 0o133, 0);
            }
        }
        #[cfg(unix)]
        assert_eq!(mode(&ssh), 0o700);
        assert_eq!(discover(&ssh).unwrap().len(), 5);
    }

    #[test]
    fn passphrase_encrypts_the_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = generate(
            dir.path(),
            &request("id_secret", KeyAlgorithm::Ed25519, Some("correct horse")),
        )
        .unwrap();
        let text = std::fs::read_to_string(&key.private_path).unwrap();
        assert!(text.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        let private = PrivateKey::from_openssh(&text).unwrap();
        assert!(private.is_encrypted());
        assert!(private.decrypt("wrong").is_err());
        let clear = private.decrypt("correct horse").unwrap();
        assert_eq!(
            clear.public_key().fingerprint(HashAlg::Sha256).to_string(),
            key.fingerprint
        );
        // An empty passphrase means none.
        let plain = generate(
            dir.path(),
            &request("id_plain", KeyAlgorithm::Ed25519, Some("")),
        )
        .unwrap();
        assert!(
            !PrivateKey::read_openssh_file(&plain.private_path)
                .unwrap()
                .is_encrypted()
        );
    }

    #[test]
    fn never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("id_taken"), "keep me").unwrap();
        std::fs::write(dir.path().join("id_half.pub"), "keep me").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/nonexistent", dir.path().join("id_link")).unwrap();
        for name in ["id_taken", "id_half", "id_link"] {
            let err =
                generate(dir.path(), &request(name, KeyAlgorithm::Ed25519, None)).unwrap_err();
            assert_eq!(err.summary().code, "key_exists", "{name}");
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("id_taken")).unwrap(),
            "keep me"
        );
        assert!(!dir.path().join("id_half").exists());
    }

    #[test]
    fn rejects_bad_requests() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "",
            "../evil",
            "a/b",
            ".hidden",
            "-x",
            "id.pub",
            "id-cert",
            "config",
            "authorized_keys",
            "na me",
            &"x".repeat(65),
        ] {
            let err =
                generate(dir.path(), &request(name, KeyAlgorithm::Ed25519, None)).unwrap_err();
            assert_eq!(err.summary().code, "key_name", "{name:?}");
        }
        for bits in [1024, 2048, 3071, 16384] {
            let err = generate(
                dir.path(),
                &request("id_rsa", KeyAlgorithm::Rsa { bits }, None),
            )
            .unwrap_err();
            assert_eq!(err.summary().code, "key_rsa_size");
        }
        let mut bad = request("id_c", KeyAlgorithm::Ed25519, None);
        bad.comment = "evil\u{202e}moc".into();
        assert_eq!(
            generate(dir.path(), &bad).unwrap_err().summary().code,
            "key_comment"
        );
        bad.comment = "a\nb".into();
        assert_eq!(
            generate(dir.path(), &bad).unwrap_err().summary().code,
            "key_comment"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_group_writable_ssh_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let err = generate(dir.path(), &request("id", KeyAlgorithm::Ed25519, None)).unwrap_err();
        assert_eq!(err.summary().code, "key_unsafe_dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(generate(dir.path(), &request("id", KeyAlgorithm::Ed25519, None)).is_ok());
    }

    #[test]
    fn passphrase_never_in_debug() {
        let r = request("id", KeyAlgorithm::Ed25519, Some("hunter2"));
        assert!(!format!("{r:?}").contains("hunter2"));
    }

    #[test]
    fn missing_dir_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(discover(&dir.path().join("nope")).expect("ok").is_empty());
    }
}
