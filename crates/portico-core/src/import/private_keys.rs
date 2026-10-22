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
//! ## Bounded decryption
//!
//! An export is untrusted: before an encrypted PEM key is decrypted, the
//! exact DER russh would decrypt is decoded as a PKCS#8
//! `EncryptedPrivateKeyInfo` and refused when its KDF is too costly
//! ([`check_kdf`]: PBKDF2 above 10 000 000 iterations, scrypt above
//! `log2 N` 20, `r` or `p` 16, or 1 GiB; PBES1 or any other KDF); a key
//! with two `BEGIN` lines is refused; one plan decrypts at most
//! [`MAX_DECRYPTIONS_PER_PLAN`] keys ([`DecryptBudget`]).
//!
//! ## Fingerprints
//!
//! The fingerprint of an encrypted OpenSSH key comes from its public half,
//! in clear: it is only **claimed** ([`KeySecret::fingerprint_verified`]),
//! and such a key never selects a local key. A local key is reused only
//! when its **private** file, read without a passphrase, has the same
//! fingerprint ([`local_copy`]).
//!
//! ## Writing
//!
//! A key that is not encrypted in the export is written in OpenSSH format
//! (re-encoded from the parsed key), encrypted with the user's passphrase
//! when one is given (bcrypt-pbkdf + aes256-ctr, like key generation). A key
//! encrypted in the export is written as it is (its text, line endings
//! normalized): its passphrase does not change. The file goes to
//! `~/.ssh/portico/<alias>` (named by [`key_file_name`]: never ending like
//! `.pub`, `-cert` or `-cert.pub`; `0600`, `create_new`: never a symlink, never
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

impl KeySecret {
    /// `true` when the fingerprint was derived from the private key itself:
    /// a clear key, or a PEM key decrypted with the export's passphrase. An
    /// encrypted OpenSSH key's public half is in clear and only **claims**
    /// to match the encrypted private half.
    pub(crate) fn fingerprint_verified(&self) -> bool {
        !self.encrypted || self.format == KeyFormat::Pem
    }
}

/// Why a key cannot be imported. Never quotes the key.
pub(crate) type Unreadable = &'static str;

/// Most encrypted PEM keys decrypted (a KDF each) while planning one
/// import; the others are skipped (`key_unreadable`).
pub(crate) const MAX_DECRYPTIONS_PER_PLAN: usize = 32;
/// Highest PBKDF2 iteration count accepted in an encrypted PKCS#8 key
/// (OpenSSL writes 2048, OWASP recommends 600 000).
pub(crate) const MAX_PBKDF2_ITERATIONS: u32 = 10_000_000;
/// Highest scrypt `log2(N)` accepted (OpenSSL writes 14).
pub(crate) const MAX_SCRYPT_LOG_N: u32 = 20;
/// Highest scrypt `r` and `p` accepted (OpenSSL writes 8 and 1).
pub(crate) const MAX_SCRYPT_R_P: u16 = 16;
/// Most memory scrypt may need (`128 · r · N` bytes).
pub(crate) const MAX_SCRYPT_MEMORY: u64 = 1024 * 1024 * 1024;

/// How many encrypted PEM keys one plan may still decrypt.
#[derive(Debug)]
pub(crate) struct DecryptBudget {
    remaining: usize,
}

impl Default for DecryptBudget {
    fn default() -> Self {
        Self {
            remaining: MAX_DECRYPTIONS_PER_PLAN,
        }
    }
}

/// The DER bytes `russh::keys::decode_secret_key` would decode from `text`
/// (same line rules: base64 lines after a `-----BEGIN` line, until the
/// first `-----END` line), and the number of `-----BEGIN` lines. `None`
/// when the base64 does not decode.
fn pem_der(text: &str) -> Option<(Zeroizing<Vec<u8>>, usize)> {
    use base64::Engine as _;
    let mut started = false;
    let mut begins = 0usize;
    let mut body = Zeroizing::new(String::with_capacity(text.len()));
    for line in text.lines() {
        if started {
            if line.starts_with("-----END ") {
                break;
            }
            if line
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            {
                body.push_str(line);
            }
        }
        if line.starts_with("-----BEGIN ") {
            begins = begins.saturating_add(1);
            started = true;
        }
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .ok()?;
    Some((Zeroizing::new(der), begins))
}

/// Refuses an encrypted PKCS#8 key whose KDF would be too costly, before
/// anything is decrypted: PBKDF2 above [`MAX_PBKDF2_ITERATIONS`], scrypt
/// above [`MAX_SCRYPT_LOG_N`], [`MAX_SCRYPT_R_P`] or
/// [`MAX_SCRYPT_MEMORY`], and any other scheme (PBES1, unknown KDF).
/// `Ok(false)`: `der` is not an `EncryptedPrivateKeyInfo` (no PBES2 KDF
/// can run on it). Reasons never quote the key.
pub(crate) fn check_kdf(der: &[u8]) -> std::result::Result<bool, Unreadable> {
    use pkcs8::der::Decode as _;
    use pkcs8::pkcs5::{EncryptionScheme, pbes2::Kdf};
    let Ok(info) = pkcs8::EncryptedPrivateKeyInfoRef::from_der(der) else {
        return Ok(false);
    };
    let EncryptionScheme::Pbes2(params) = &info.encryption_algorithm else {
        return Err("an encrypted PEM key with a legacy scheme Portico does not decrypt");
    };
    match &params.kdf {
        Kdf::Pbkdf2(pbkdf2) if pbkdf2.iteration_count <= MAX_PBKDF2_ITERATIONS => Ok(true),
        Kdf::Pbkdf2(_) => {
            Err("an encrypted PEM key with too many PBKDF2 iterations: not decrypted")
        }
        Kdf::Scrypt(scrypt) => {
            let n = scrypt.cost_parameter;
            let log_n = n.checked_ilog2().unwrap_or(u32::MAX);
            let memory = 128u64
                .saturating_mul(u64::from(scrypt.block_size))
                .saturating_mul(n);
            if n.is_power_of_two()
                && log_n <= MAX_SCRYPT_LOG_N
                && scrypt.block_size <= MAX_SCRYPT_R_P
                && scrypt.parallelization <= MAX_SCRYPT_R_P
                && memory <= MAX_SCRYPT_MEMORY
            {
                Ok(true)
            } else {
                Err("an encrypted PEM key with scrypt parameters too costly: not decrypted")
            }
        }
        _ => Err("an encrypted PEM key with a key derivation Portico does not decrypt"),
    }
}

/// Parses the key text of an export. `key_password` (the export's
/// `keyPassword`) is only used to read the public half of an encrypted PEM
/// key, after its KDF parameters were checked ([`check_kdf`]) and while
/// `budget` allows. Blocking (decryption runs a KDF).
pub(crate) fn parse(
    raw: &str,
    key_password: Option<&str>,
    budget: &mut DecryptBudget,
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
    // What russh would decrypt, checked first: one PEM block only, and a
    // PKCS#8 KDF with bounded parameters.
    let Some((der, begins)) = pem_der(body) else {
        return Err("not a PEM private key Portico can read");
    };
    if begins != 1 {
        return Err("more than one PEM block in one key");
    }
    let pkcs8 = check_kdf(&der)?;
    drop(der);
    if !pkcs8 && first.contains("ENCRYPTED PRIVATE KEY") {
        return Err("not an encrypted PKCS#8 key Portico can read");
    }
    if budget.remaining == 0 {
        return Err("too many encrypted PEM keys in this export: not decrypted");
    }
    budget.remaining = budget.remaining.saturating_sub(1);
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
/// Portico may write as an `IdentityFile`. The fingerprint is read again
/// from the **private** file (a `.pub` next to another private key would
/// otherwise lend it its identity); an encrypted file, whose private half
/// cannot be read without its passphrase, is never matched. Only call it for a
/// verified fingerprint ([`KeySecret::fingerprint_verified`]). Blocking.
pub(crate) fn local_copy(paths: &Paths, local: &[LocalKey], id: &KeyId) -> Option<PathBuf> {
    local
        .iter()
        .filter(|k| k.fingerprint == id.as_str())
        .filter(|k| private_fingerprint(&k.private_path).as_deref() == Some(id.as_str()))
        .find_map(|k| crate::managed_hosts::check_identity_file(&paths.home, &k.private_path).ok())
}

/// The fingerprint of the private key file at `path`, read without any
/// passphrase (no KDF runs). `None` when it cannot be read that way.
/// Blocking.
fn private_fingerprint(path: &std::path::Path) -> Option<String> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > u64::try_from(MAX_KEY_TEXT_BYTES).unwrap_or(u64::MAX) {
        return None;
    }
    let text = Zeroizing::new(std::fs::read_to_string(path).ok()?);
    let body = text.trim_start();
    if body.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
        // An encrypted one only claims its public half (it may be a key an
        // earlier import wrote with an unverified fingerprint).
        return PrivateKey::from_openssh(body.as_bytes())
            .ok()
            .filter(|k| !k.is_encrypted())
            .map(|k| fingerprint(k.public_key()));
    }
    if body.starts_with("PuTTY-User-Key-File-") || body.contains("ENCRYPTED") {
        return None;
    }
    russh::keys::decode_secret_key(body, None)
        .ok()
        .map(|k| fingerprint(k.public_key()))
}

/// The file name of an imported key for the host `alias`: the rules of
/// key generation (`keys::validate_name`), so it never ends like an
/// OpenSSH companion file (`.pub`, `-cert`, `-cert.pub`: `x.pub` becomes
/// `x_pub`) nor takes a name OpenSSH reserves (`config`…).
pub(crate) fn key_file_name(alias: &str) -> String {
    // A `-n` suffix is added on a clash: keep room for it (and for `_key`).
    let mut name: String = alias
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(crate::keys::MAX_NAME_LEN.saturating_sub(12))
        .collect();
    loop {
        if let Some(stem) = name.strip_suffix(".pub") {
            name = format!("{stem}_pub");
        } else if let Some(stem) = name.strip_suffix("-cert") {
            name = format!("{stem}_cert");
        } else {
            break;
        }
    }
    if name.starts_with(['.', '-']) || name.is_empty() {
        name = format!("key_{name}");
    }
    if crate::keys::validate_name(&name).is_ok() {
        name
    } else {
        format!("{name}_key")
    }
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

/// Writes the key `id` as `~/.ssh/portico/<name>` (or `<name>-2`…; `name`
/// goes through [`key_file_name`] first), with
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
    let name = key_file_name(name);
    let name = name.as_str();
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
        let parsed = parse(&text, None, &mut DecryptBudget::default()).expect("plain");
        assert_eq!(parsed.format, KeyFormat::OpenSsh);
        assert!(!parsed.encrypted);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));
        assert!(!parsed.text.contains('\r'));
        assert!(!format!("{parsed:?}").contains("OPENSSH"));

        let mut rng = crate::keys::system_rng().expect("rng");
        let locked = key.encrypt(&mut rng, b"pw").expect("encrypt");
        let text = locked.to_openssh(LineEnding::LF).expect("pem");
        let parsed = parse(&text, None, &mut DecryptBudget::default()).expect("encrypted");
        assert!(parsed.encrypted);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));
    }

    #[test]
    fn pem_keys() {
        let key = ed25519();
        let mut pem = Vec::new();
        russh::keys::encode_pkcs8_pem(&key, &mut pem).expect("pkcs8");
        let pem = String::from_utf8(pem).expect("utf8");
        let parsed = parse(&pem, None, &mut DecryptBudget::default()).expect("pkcs8");
        assert_eq!(parsed.format, KeyFormat::Pem);
        assert_eq!(parsed.id().as_str(), fingerprint(key.public_key()));

        let mut encrypted = Vec::new();
        russh::keys::encode_pkcs8_pem_encrypted(&key, b"pw", 16, &mut encrypted).expect("enc");
        let encrypted = String::from_utf8(encrypted).expect("utf8");
        assert!(
            parse(&encrypted, None, &mut DecryptBudget::default()).is_err(),
            "no passphrase"
        );
        assert!(
            parse(&encrypted, Some("nope"), &mut DecryptBudget::default()).is_err(),
            "wrong passphrase"
        );
        let parsed =
            parse(&encrypted, Some("pw"), &mut DecryptBudget::default()).expect("with keyPassword");
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
            let reason = parse(bad, None, &mut DecryptBudget::default()).expect_err("refused");
            assert!(!reason.contains("AAAA"), "{reason}");
        }
    }

    /// An `ENCRYPTED PRIVATE KEY` PEM block around `kdf` (the encrypted
    /// data is junk: it must never be reached).
    fn hostile_pem(kdf: pkcs8::pkcs5::pbes2::Kdf) -> String {
        use base64::Engine as _;
        use pkcs8::der::Encode as _;
        use pkcs8::der::asn1::OctetStringRef;
        use pkcs8::pkcs5::{EncryptionScheme, pbes2};
        let params = pbes2::Parameters {
            kdf,
            encryption: pbes2::EncryptionScheme::Aes256Cbc { iv: [1; 16] },
        };
        let data = [0u8; 64];
        let info = pkcs8::EncryptedPrivateKeyInfoRef {
            encryption_algorithm: EncryptionScheme::Pbes2(params),
            encrypted_data: OctetStringRef::new(&data).expect("octets"),
        };
        let der = info.to_der().expect("der");
        format!(
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\n{}\n-----END ENCRYPTED PRIVATE KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        )
    }

    #[test]
    fn hostile_kdf_parameters_are_refused_quickly() {
        use pkcs8::pkcs5::pbes2::{Kdf, Pbkdf2Params, Pbkdf2Prf, Salt, ScryptParams};
        let salt = Salt::new([7u8; 16]).expect("salt");
        let hostile = [
            Kdf::Scrypt(ScryptParams {
                salt,
                cost_parameter: 1u64 << 40,
                block_size: 8,
                parallelization: 1,
                key_length: None,
            }),
            Kdf::Pbkdf2(Pbkdf2Params {
                salt,
                iteration_count: 4_000_000_000,
                key_length: None,
                prf: Pbkdf2Prf::HmacWithSha256,
            }),
            // Within each bound alone, too much memory together.
            Kdf::Scrypt(ScryptParams {
                salt,
                cost_parameter: 1u64 << 20,
                block_size: 16,
                parallelization: 1,
                key_length: None,
            }),
        ];
        for kdf in hostile {
            let pem = hostile_pem(kdf);
            let mut budget = DecryptBudget::default();
            let start = std::time::Instant::now();
            let reason = parse(&pem, Some("FAKEKEYPASS"), &mut budget).expect_err("refused");
            assert!(
                start.elapsed() < std::time::Duration::from_millis(100),
                "{reason}: took {:?}",
                start.elapsed()
            );
            assert!(reason.contains("not decrypted"), "{reason}");
            assert!(!reason.contains("FAKEKEYPASS") && !reason.contains("BEGIN"));
            assert_eq!(budget.remaining, MAX_DECRYPTIONS_PER_PLAN, "no KDF run");
        }
        // Ordinary parameters still pass the check (the data is junk, so
        // decryption then fails, after the budget was spent).
        let ordinary = Kdf::Pbkdf2(Pbkdf2Params::hmac_sha256(2048, &[7u8; 16]).expect("pbkdf2"));
        let pem = hostile_pem(ordinary);
        let mut budget = DecryptBudget::default();
        assert!(parse(&pem, Some("pw"), &mut budget).is_err());
        assert_eq!(budget.remaining, MAX_DECRYPTIONS_PER_PLAN - 1);
    }

    #[test]
    fn decryptions_are_capped_per_plan() {
        let key = ed25519();
        let mut encrypted = Vec::new();
        russh::keys::encode_pkcs8_pem_encrypted(&key, b"pw", 16, &mut encrypted).expect("enc");
        let encrypted = String::from_utf8(encrypted).expect("utf8");
        let mut budget = DecryptBudget { remaining: 1 };
        assert!(parse(&encrypted, Some("pw"), &mut budget).is_ok());
        let reason = parse(&encrypted, Some("pw"), &mut budget).expect_err("over the cap");
        assert!(reason.contains("too many"), "{reason}");
        // Two `BEGIN` lines before the `END` (russh would decode with the
        // format of the last one): refused.
        let doubled = format!("-----BEGIN ENCRYPTED PRIVATE KEY-----\n{encrypted}");
        let reason =
            parse(&doubled, Some("pw"), &mut DecryptBudget::default()).expect_err("two blocks");
        assert!(reason.contains("more than one"), "{reason}");
    }

    #[test]
    fn key_file_names_never_end_like_openssh_companions() {
        for (alias, expected) in [
            ("web", "web"),
            ("x.pub", "x_pub"),
            ("x-cert", "x_cert"),
            ("x-cert.pub", "x-cert_pub"),
            ("x.pub.pub", "x.pub_pub"),
            ("a-cert-cert", "a-cert_cert"),
            ("config", "config_key"),
            ("known_hosts", "known_hosts_key"),
            (".hidden", "key_.hidden"),
        ] {
            let name = key_file_name(alias);
            assert_eq!(name, expected, "{alias}");
            assert!(crate::keys::validate_name(&name).is_ok(), "{name}");
            for suffix in [".pub", "-cert", "-cert.pub"] {
                assert!(!name.ends_with(suffix), "{name}");
                assert!(!format!("{name}-2").ends_with(suffix), "{name}-2");
            }
        }
        let long = format!("{}.pub", "a".repeat(80));
        let name = key_file_name(&long);
        assert!(crate::keys::validate_name(&name).is_ok(), "{name}");
        assert!(crate::keys::validate_name(&format!("{name}-100")).is_ok());

        // Written under the cleaned name, with its `.pub` beside it.
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        let key = ed25519();
        let text = key.to_openssh(LineEnding::LF).expect("pem");
        let secret = parse(&text, None, &mut DecryptBudget::default()).expect("key");
        let written = write(&paths, "x.pub", &secret.id(), &secret, None).expect("write");
        assert_eq!(written, paths.ssh_dir.join(IMPORTED_KEYS_DIR).join("x_pub"));
        assert!(
            paths
                .ssh_dir
                .join(IMPORTED_KEYS_DIR)
                .join("x_pub.pub")
                .is_file()
        );
    }
}
