//! Private keys found in an export (Termix): parsing, local copies, and
//! writing the keys the user selected.
//!
//! ## Formats read
//!
//! Parsing goes through `russh::keys` (the `ssh_key` version russh pins, a
//! single key model in the process):
//!
//! | text | read with | encrypted |
//! |---|---|---|
//! | `-----BEGIN OPENSSH PRIVATE KEY-----` | `ssh_key::PrivateKey::from_openssh` | the public half is in clear: listed, written as it is |
//! | `-----BEGIN RSA PRIVATE KEY-----` (PKCS#1) | `russh::keys::decode_secret_key` | legacy `Proc-Type: 4,ENCRYPTED` (PKCS#5): read with the export's `keyPassword` only |
//! | `-----BEGIN EC PRIVATE KEY-----` (SEC1) | same | same |
//! | `-----BEGIN PRIVATE KEY-----` (PKCS#8: RSA, ECDSA, Ed25519) | same | `-----BEGIN ENCRYPTED PRIVATE KEY-----`: with `keyPassword` only |
//!
//! Anything else (`PuTTY` `.ppk`, DSA, garbage, an encrypted PEM key without
//! a working `keyPassword`) is refused with a reason that never quotes the
//! key. `keyPassword` is used, in memory, only to compute the fingerprint
//! of an encrypted PEM key, then wiped: it is never kept nor written.
//!
//! ## Writing
//!
//! A key that is not encrypted in the export is written in OpenSSH format
//! (re-encoded from the parsed key), encrypted with the user's passphrase
//! when one is given (bcrypt-pbkdf + aes256-ctr, like key generation). A key
//! encrypted in the export is written as it is (its text, line endings
//! normalized): its passphrase does not change. The file goes to
//! `~/.ssh/portico/<alias>` (`0600`, `create_new`: never a symlink, never
//! an overwrite; `-2`, `-3`… on a name clash), with its `.pub`; the folder
//! is created `0700` and refused when it is a symlink or writable by others.

use std::fmt;
use std::path::PathBuf;

use russh::keys::HashAlg;
use russh::keys::ssh_key::{LineEnding, PrivateKey, PublicKey};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use zeroize::Zeroizing;

use super::KeyId;
use crate::error::{Error, Result};
use crate::keys::{KeyError, LocalKey};
use crate::store::Paths;

/// Folder of `~/.ssh` where imported keys are written.
pub const IMPORTED_KEYS_DIR: &str = "portico";
/// Longest key text read, in bytes (an RSA 16384 PEM is ~13 KiB).
pub(crate) const MAX_KEY_TEXT_BYTES: usize = 64 * 1024;
/// Most `-n` suffixes tried for a key file name.
const MAX_NAME_ATTEMPTS: usize = 100;

/// How a key is encoded in the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyFormat {
    /// `-----BEGIN OPENSSH PRIVATE KEY-----`.
    OpenSsh,
    /// PKCS#1 RSA, SEC1 EC or PKCS#8 PEM.
    Pem,
}

/// A private key of an export, kept for applying the import. The text
/// is wiped on drop; `Debug` shows the fingerprint only.
pub(crate) struct KeySecret {
    /// The key as found (`\r\n` normalized to `\n`, trailing newline).
    text: Zeroizing<String>,
    pub(crate) format: KeyFormat,
    pub(crate) encrypted: bool,
    pub(crate) public: PublicKey,
}

impl fmt::Debug for KeySecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeySecret")
            .field("fingerprint", &self.id())
            .field("format", &self.format)
            .field("encrypted", &self.encrypted)
            .finish_non_exhaustive()
    }
}

impl KeySecret {
    pub(crate) fn id(&self) -> KeyId {
        KeyId(fingerprint(&self.public))
    }
}

pub(crate) fn fingerprint(public: &PublicKey) -> String {
    public.fingerprint(HashAlg::Sha256).to_string()
}

/// Why a key cannot be imported. Never quotes the key.
pub(crate) type Unreadable = &'static str;

/// Parses the key text of an export. `key_password` (the export's
/// `keyPassword`) is only used to read the public half of an encrypted PEM
/// key. Blocking (decryption runs a KDF).
pub(crate) fn parse(
    raw: &str,
    key_password: Option<&str>,
) -> std::result::Result<KeySecret, Unreadable> {
    if raw.len() > MAX_KEY_TEXT_BYTES {
        return Err("the key is larger than 64 KiB");
    }
    let text = normalized(raw);
    let body = text.trim_start();
    if body.starts_with("PuTTY-User-Key-File-") {
        return Err("a PuTTY key: convert it to OpenSSH with puttygen first");
    }
    if body.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
        let key = PrivateKey::from_openssh(body.as_bytes())
            .map_err(|_| "not an OpenSSH private key Portico can read")?;
        let encrypted = key.is_encrypted();
        return Ok(KeySecret {
            public: key.public_key().clone(),
            text,
            format: KeyFormat::OpenSsh,
            encrypted,
        });
    }
    let first = body.lines().next().unwrap_or_default();
    let pem = matches!(
        first.trim_end(),
        "-----BEGIN RSA PRIVATE KEY-----"
            | "-----BEGIN EC PRIVATE KEY-----"
            | "-----BEGIN PRIVATE KEY-----"
            | "-----BEGIN ENCRYPTED PRIVATE KEY-----"
    );
    if !pem {
        return Err("not a private key Portico can read (OpenSSH or PEM)");
    }
    let encrypted = first.contains("ENCRYPTED") || body.contains("Proc-Type: 4,ENCRYPTED");
    if !encrypted {
        let key = russh::keys::decode_secret_key(body, None)
            .map_err(|_| "not a PEM private key Portico can read")?;
        return Ok(KeySecret {
            public: key.public_key().clone(),
            text,
            format: KeyFormat::Pem,
            encrypted: false,
        });
    }
    let Some(password) = key_password.filter(|p| !p.is_empty()) else {
        return Err("an encrypted PEM key without its passphrase in the export");
    };
    let key = russh::keys::decode_secret_key(body, Some(password))
        .map_err(|_| "an encrypted PEM key Portico cannot read with the export's passphrase")?;
    Ok(KeySecret {
        public: key.public_key().clone(),
        text,
        format: KeyFormat::Pem,
        encrypted: true,
    })
}

/// `raw` with `\r\n` turned into `\n`, trimmed, ending with one `\n`, in a
/// buffer sized up front (no reallocation leaves an unwiped copy).
fn normalized(raw: &str) -> Zeroizing<String> {
    let trimmed = raw.trim();
    let mut out = Zeroizing::new(String::with_capacity(trimmed.len().saturating_add(1)));
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' && chars.peek() == Some(&'\n') {
            continue;
        }
        out.push(c);
    }
    out.push('\n');
    out
}

/// The local key pairs Portico can reuse: `~/.ssh/*.pub` and
/// `~/.ssh/portico/*.pub` whose private half exists. Blocking.
pub(crate) fn local_keys(paths: &Paths) -> Vec<LocalKey> {
    let mut keys = Vec::new();
    for dir in [paths.ssh_dir.clone(), paths.ssh_dir.join(IMPORTED_KEYS_DIR)] {
        if let Ok(found) = crate::keys::discover(&dir) {
            keys.extend(found.into_iter().filter(|k| k.has_private));
        }
    }
    keys
}

/// The private key of `local` with the fingerprint `id`, when it is one
/// Portico may write as an `IdentityFile`.
pub(crate) fn local_copy(paths: &Paths, local: &[LocalKey], id: &KeyId) -> Option<PathBuf> {
    local
        .iter()
        .filter(|k| k.fingerprint == id.as_str())
        .find_map(|k| crate::managed_hosts::check_identity_file(&paths.home, &k.private_path).ok())
}

/// `~/.ssh/portico`, created `0700` when missing; refused when it is a
/// symlink, not a directory, not ours or writable by others. Blocking.
fn imported_keys_dir(paths: &Paths) -> Result<PathBuf> {
    crate::keys::create_private_dir(&paths.ssh_dir)?;
    let dir = paths.ssh_dir.join(IMPORTED_KEYS_DIR);
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(KeyError::UnsafeDir { path: dir }.into());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(&dir, e)),
    }
    crate::keys::create_private_dir(&dir)?;
    Ok(dir)
}

/// The private and public texts to write for `secret`. Blocking (the
/// passphrase KDF).
fn texts(
    id: &KeyId,
    secret: &KeySecret,
    passphrase: Option<&SecretString>,
) -> Result<(Zeroizing<String>, String)> {
    let bad = |reason: &str| KeyError::BadPrivateKey {
        path: PathBuf::from(id.as_str()),
        reason: reason.to_owned(),
    };
    let public = secret
        .public
        .to_openssh()
        .map_err(|_| bad("cannot encode the public key"))?;
    if secret.id() != *id {
        return Err(bad("the key changed since the import was planned").into());
    }
    if secret.encrypted {
        // Written as it is: the passphrase of the export still opens it.
        return Ok((secret.text.clone(), public));
    }
    let mut key = match secret.format {
        KeyFormat::OpenSsh => PrivateKey::from_openssh(secret.text.as_bytes())
            .map_err(|_| bad("not an OpenSSH private key"))?,
        KeyFormat::Pem => russh::keys::decode_secret_key(&secret.text, None)
            .map_err(|_| bad("not a PEM private key"))?,
    };
    if fingerprint(key.public_key()) != id.as_str() {
        return Err(bad("the key does not match its fingerprint").into());
    }
    if let Some(passphrase) = passphrase.filter(|p| !p.expose_secret().is_empty()) {
        let mut rng = crate::keys::system_rng()?;
        key = key
            .encrypt(&mut rng, passphrase.expose_secret().as_bytes())
            .map_err(|_| bad("cannot encrypt the key"))?;
    }
    let private = key
        .to_openssh(LineEnding::LF)
        .map_err(|_| bad("cannot encode the private key"))?;
    Ok((private, public))
}

/// Writes the key `id` as `~/.ssh/portico/<name>` (or `<name>-2`…), with
/// its `.pub`, and returns the private key's path. Never overwrites a file
/// nor follows a symlink: a taken name moves on to the next suffix.
/// Blocking.
pub(crate) fn write(
    paths: &Paths,
    name: &str,
    id: &KeyId,
    secret: &KeySecret,
    passphrase: Option<&SecretString>,
) -> Result<PathBuf> {
    let (private, public) = texts(id, secret, passphrase)?;
    let dir = imported_keys_dir(paths)?;
    for attempt in 1..=MAX_NAME_ATTEMPTS {
        let file = if attempt == 1 {
            name.to_owned()
        } else {
            format!("{name}-{attempt}")
        };
        let private_path = dir.join(&file);
        let public_path = dir.join(format!("{file}.pub"));
        // A dangling symlink counts as taken.
        if private_path.symlink_metadata().is_ok() || public_path.symlink_metadata().is_ok() {
            continue;
        }
        match crate::keys::write_new(&private_path, private.as_bytes(), 0o600) {
            Ok(()) => {}
            Err(Error::Key(KeyError::Exists { .. })) => continue,
            Err(e) => return Err(e),
        }
        let public_text = format!("{public}\n");
        if let Err(e) = crate::keys::write_new(&public_path, public_text.as_bytes(), 0o644) {
            // Never leave a private key without its public half.
            let _ = std::fs::remove_file(&private_path);
            if matches!(e, Error::Key(KeyError::Exists { .. })) {
                continue;
            }
            return Err(e);
        }
        return Ok(private_path);
    }
    Err(KeyError::Exists {
        path: dir.join(name),
    }
    .into())
}

/// Removes key pairs written by an import that then failed (they would be
/// referenced by nothing).
pub(crate) fn remove_written(written: &[PathBuf]) {
    for path in written {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}.pub", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed25519() -> PrivateKey {
        let mut rng = crate::keys::system_rng().expect("rng");
        PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).expect("key")
    }

    #[test]
    fn openssh_keys_plain_and_encrypted() {
        let key = ed25519();
        let text = key.to_openssh(LineEnding::CRLF).expect("pem");
        let parsed = parse(&text, None).expect("plain");
        assert_eq!(parsed.format, KeyFormat::OpenSsh);
        assert!(!parsed.encrypted);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));
        assert!(!parsed.text.contains('\r'));
        assert!(!format!("{parsed:?}").contains("OPENSSH"));

        let mut rng = crate::keys::system_rng().expect("rng");
        let locked = key.encrypt(&mut rng, b"pw").expect("encrypt");
        let text = locked.to_openssh(LineEnding::LF).expect("pem");
        let parsed = parse(&text, None).expect("encrypted");
        assert!(parsed.encrypted);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));
    }

    #[test]
    fn pem_keys() {
        let key = ed25519();
        let mut pem = Vec::new();
        russh::keys::encode_pkcs8_pem(&key, &mut pem).expect("pkcs8");
        let pem = String::from_utf8(pem).expect("utf8");
        let parsed = parse(&pem, None).expect("pkcs8");
        assert_eq!(parsed.format, KeyFormat::Pem);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));

        let mut encrypted = Vec::new();
        russh::keys::encode_pkcs8_pem_encrypted(&key, b"pw", 16, &mut encrypted).expect("enc");
        let encrypted = String::from_utf8(encrypted).expect("utf8");
        assert!(parse(&encrypted, None).is_err(), "no passphrase");
        assert!(parse(&encrypted, Some("nope")).is_err(), "wrong passphrase");
        let parsed = parse(&encrypted, Some("pw")).expect("with keyPassword");
        assert!(parsed.encrypted);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));
    }

    #[test]
    fn garbage_is_refused_without_quoting() {
        for bad in [
            "",
            "hello",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAgarbage\n-----END OPENSSH PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nAAAAgarbage\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN DSA PRIVATE KEY-----\nAAAA\n-----END DSA PRIVATE KEY-----",
            "PuTTY-User-Key-File-3: ssh-ed25519\n",
        ] {
            let reason = parse(bad, None).expect_err("refused");
            assert!(!reason.contains("AAAA"), "{reason}");
        }
    }
}
