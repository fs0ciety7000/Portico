//! OpenSSH `known_hosts` verification.
//!
//! Supported: plain and wildcard host patterns with negation, `[host]:port`
//! entries, hashed entries (`|1|salt|hmac-sha1`), `@cert-authority` (host
//! certificates) and `@revoked`. Several files are consulted in order (user
//! files first, then global ones).
//!
//! Security invariants:
//! - a revoked key is always refused;
//! - a host known with a *different key of the same algorithm* is
//!   [`HostKeyVerdict::Changed`]: callers must fail hard, never offer to accept;
//! - writing only ever **appends** one line (no rewrite of existing entries).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::ssh_key::certificate::CertType;
use russh::keys::{Certificate, HashAlg, PublicKey};
use sha1::Sha1;

use crate::error::{Error, Result};
use crate::ssh_config::HostPattern;
use crate::ssh_config::pattern::match_pattern_list;

/// Line marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    None,
    CertAuthority,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostField {
    Patterns(Vec<HostPattern>),
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
}

/// One parsed `known_hosts` line.
#[derive(Debug, Clone)]
pub struct Entry {
    pub marker: Marker,
    hosts: HostField,
    pub key: PublicKey,
    pub file: PathBuf,
    pub line: usize,
}

/// Outcome of a host key check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// The exact key is recorded for this host.
    Trusted { file: PathBuf, line: usize },
    /// A host certificate signed by a trusted `@cert-authority`.
    TrustedCertificate { ca_fingerprint: String },
    /// Nothing recorded for this algorithm. `other_algorithms` lists key
    /// types already known for the host (shown as a warning in the prompt).
    Unknown { other_algorithms: Vec<String> },
    /// A different key of the same algorithm is recorded: possible MITM.
    Changed {
        file: PathBuf,
        line: usize,
        expected_fingerprint: String,
    },
    /// The key (or its CA) is marked `@revoked`.
    Revoked { file: PathBuf, line: usize },
    /// Signed by a trusted CA but invalid (expired, wrong principal, type…).
    InvalidCertificate { reason: String },
}

/// All entries from a set of files.
#[derive(Debug, Clone, Default)]
pub struct KnownHosts {
    entries: Vec<Entry>,
    /// Malformed lines, as `file:line: message` (never fatal).
    pub warnings: Vec<String>,
}

/// The name OpenSSH matches entries against: `host` or `[host]:port`.
pub fn host_field(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_owned()
    } else {
        format!("[{host}]:{port}")
    }
}

impl KnownHosts {
    /// Loads every existing file in `files` (missing files are skipped).
    pub fn load(files: &[PathBuf]) -> Result<Self> {
        let mut kh = Self::default();
        for file in files {
            match std::fs::read_to_string(file) {
                Ok(text) => kh.parse_into(&text, file),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(file, e)),
            }
        }
        Ok(kh)
    }

    /// Parses `text` as if read from `file`.
    pub fn parse(text: &str, file: &Path) -> Self {
        let mut kh = Self::default();
        kh.parse_into(text, file);
        kh
    }

    fn parse_into(&mut self, text: &str, file: &Path) {
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            match parse_line(trimmed) {
                Ok((marker, hosts, key)) => self.entries.push(Entry {
                    marker,
                    hosts,
                    key,
                    file: file.to_path_buf(),
                    line,
                }),
                Err(msg) => self
                    .warnings
                    .push(format!("{}:{line}: {msg}", file.display())),
            }
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    fn matching<'a>(
        &'a self,
        host: &'a str,
        port: u16,
        marker: Marker,
    ) -> impl Iterator<Item = &'a Entry> + 'a {
        // OpenSSH lowercases names before matching and hashing.
        let name = host_field(&host.to_ascii_lowercase(), port);
        self.entries
            .iter()
            .filter(move |e| e.marker == marker && e.hosts.matches(&name))
    }

    fn revoked(&self, key: &PublicKey) -> Option<&Entry> {
        // `@revoked` applies whatever the host pattern (OpenSSH semantics).
        self.entries
            .iter()
            .find(|e| e.marker == Marker::Revoked && e.key.key_data() == key.key_data())
    }

    /// Checks a plain host key.
    pub fn check_key(&self, host: &str, port: u16, key: &PublicKey) -> HostKeyVerdict {
        if let Some(e) = self.revoked(key) {
            return HostKeyVerdict::Revoked {
                file: e.file.clone(),
                line: e.line,
            };
        }
        let mut other_algorithms: Vec<String> = Vec::new();
        let mut changed: Option<&Entry> = None;
        for e in self.matching(host, port, Marker::None) {
            if e.key.key_data() == key.key_data() {
                return HostKeyVerdict::Trusted {
                    file: e.file.clone(),
                    line: e.line,
                };
            }
            if e.key.algorithm() == key.algorithm() {
                changed.get_or_insert(e);
            } else {
                let alg = e.key.algorithm().to_string();
                if !other_algorithms.contains(&alg) {
                    other_algorithms.push(alg);
                }
            }
        }
        match changed {
            Some(e) => HostKeyVerdict::Changed {
                file: e.file.clone(),
                line: e.line,
                expected_fingerprint: e.key.fingerprint(HashAlg::Sha256).to_string(),
            },
            None => HostKeyVerdict::Unknown { other_algorithms },
        }
    }

    /// Checks a host certificate at `unix_now`. Falls back to checking the
    /// certified key as a plain key when no trusted CA signed it.
    pub fn check_certificate(
        &self,
        host: &str,
        port: u16,
        cert: &Certificate,
        unix_now: u64,
    ) -> HostKeyVerdict {
        let certified = PublicKey::from(cert.public_key().clone());
        let ca = PublicKey::from(cert.signature_key().clone());
        for key in [&certified, &ca] {
            if let Some(e) = self.revoked(key) {
                return HostKeyVerdict::Revoked {
                    file: e.file.clone(),
                    line: e.line,
                };
            }
        }

        let trusted_ca = self
            .matching(host, port, Marker::CertAuthority)
            .any(|e| e.key.key_data() == ca.key_data());
        if !trusted_ca {
            return self.check_key(host, port, &certified);
        }

        let ca_fingerprint = ca.fingerprint(HashAlg::Sha256);
        if let Err(e) = cert.validate_at(unix_now, [&ca_fingerprint]) {
            return HostKeyVerdict::InvalidCertificate {
                reason: format!("certificate rejected: {e}"),
            };
        }
        if cert.cert_type() != CertType::Host {
            return HostKeyVerdict::InvalidCertificate {
                reason: "a user certificate was presented as host key".to_owned(),
            };
        }
        // Strict: an empty principal list is not accepted for host certificates.
        if !cert
            .valid_principals()
            .iter()
            .any(|p| p.eq_ignore_ascii_case(host))
        {
            return HostKeyVerdict::InvalidCertificate {
                reason: format!("certificate is not valid for host `{host}`"),
            };
        }
        HostKeyVerdict::TrustedCertificate {
            ca_fingerprint: ca_fingerprint.to_string(),
        }
    }

    /// Key algorithms recorded for this host, in file order (used to prefer
    /// an already-known algorithm during key exchange).
    pub fn known_algorithms(&self, host: &str, port: u16) -> Vec<russh::keys::Algorithm> {
        let mut out = Vec::new();
        for e in self.matching(host, port, Marker::None) {
            let alg = e.key.algorithm();
            if !out.contains(&alg) {
                out.push(alg);
            }
        }
        out
    }

    /// SHA256 fingerprints of the plain keys recorded for this host (what
    /// remembered passwords are bound to), without duplicates.
    pub(crate) fn fingerprints(&self, host: &str, port: u16) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for e in self.matching(host, port, Marker::None) {
            let fingerprint = e.key.fingerprint(HashAlg::Sha256).to_string();
            if !out.contains(&fingerprint) {
                out.push(fingerprint);
            }
        }
        out
    }

    /// `true` if an `@cert-authority` line covers this host.
    pub fn has_cert_authority(&self, host: &str, port: u16) -> bool {
        self.matching(host, port, Marker::CertAuthority)
            .next()
            .is_some()
    }
}

impl HostField {
    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Patterns(patterns) => match_pattern_list(patterns, name),
            Self::Hashed { salt, hash } => hashed_name(salt, name).is_some_and(|h| h == *hash),
        }
    }
}

fn hashed_name(salt: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(salt).ok()?;
    mac.update(name.as_bytes());
    Some(mac.finalize().into_bytes().to_vec())
}

fn parse_line(line: &str) -> std::result::Result<(Marker, HostField, PublicKey), String> {
    let mut tokens = line.split_whitespace();
    let mut first = tokens.next().ok_or("empty line")?;
    let marker = match first {
        "@cert-authority" => Marker::CertAuthority,
        "@revoked" => Marker::Revoked,
        m if m.starts_with('@') => return Err(format!("unknown marker `{m}`")),
        _ => Marker::None,
    };
    if marker != Marker::None {
        first = tokens.next().ok_or("missing host field")?;
    }
    let hosts = parse_host_field(first)?;
    let key_type = tokens.next().ok_or("missing key type")?;
    let blob = tokens.next().ok_or("missing key data")?;
    let key = PublicKey::from_openssh(&format!("{key_type} {blob}"))
        .map_err(|e| format!("invalid key: {e}"))?;
    Ok((marker, hosts, key))
}

fn parse_host_field(field: &str) -> std::result::Result<HostField, String> {
    if let Some(rest) = field.strip_prefix("|1|") {
        let (salt, hash) = rest.split_once('|').ok_or("malformed hashed host")?;
        let salt = B64.decode(salt).map_err(|_| "invalid hashed host salt")?;
        let hash = B64.decode(hash).map_err(|_| "invalid hashed host hash")?;
        return Ok(HostField::Hashed { salt, hash });
    }
    let patterns: Vec<HostPattern> = field
        .split(',')
        .filter(|p| !p.is_empty())
        .map(HostPattern::parse)
        .collect();
    if patterns.is_empty() {
        return Err("empty host field".to_owned());
    }
    Ok(HostField::Patterns(patterns))
}

/// Appends `host key` to `path`, creating `~/.ssh` (0700) and the file (0600)
/// if needed. A single `write` of a complete line keeps concurrent appends
/// from interleaving.
///
/// With `hashed` (`HashKnownHosts yes`) the name is stored as
/// `|1|salt|HMAC-SHA1(salt, name)` so the file does not reveal host names.
pub fn append(path: &Path, host: &str, port: u16, key: &PublicKey, hashed: bool) -> Result<()> {
    validate_host_name(host)?;
    let name = host_field(&host.to_ascii_lowercase(), port);
    let field = if hashed {
        let mut salt = [0u8; 20];
        russh::keys::ssh_key::rand_core::Rng::fill_bytes(
            &mut russh::keys::key::safe_rng(),
            &mut salt,
        );
        let hash = hashed_name(&salt, &name).ok_or_else(|| Error::Serde {
            what: "known_hosts hash",
            message: "invalid salt".to_owned(),
        })?;
        format!("|1|{}|{}", B64.encode(salt), B64.encode(hash))
    } else {
        name
    };
    if let Some(dir) = path.parent().filter(|d| !d.exists()) {
        create_private_dir(dir)?;
    }
    let blob = key.to_bytes().map_err(|e| Error::Serde {
        what: "public key",
        message: e.to_string(),
    })?;
    let needs_newline = std::fs::read(path)
        .ok()
        .and_then(|bytes| bytes.last().copied())
        .is_some_and(|last| last != b'\n');
    let line = format!(
        "{}{} {} {}\n",
        if needs_newline { "\n" } else { "" },
        field,
        key.algorithm().as_str(),
        B64.encode(blob)
    );

    let mut opts = std::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| Error::io(path, e))?;
    file.write_all(line.as_bytes())
        .map_err(|e| Error::io(path, e))?;
    file.sync_all().map_err(|e| Error::io(path, e))
}

/// Rejects names that would be read back as patterns or markers (`,`, `*`,
/// `!`, `|1|`…) or break the line format. IPv6 literals stay valid: brackets
/// are only added by [`host_field`].
pub fn validate_host_name(host: &str) -> Result<()> {
    let bad = host.is_empty()
        || host.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, ',' | '*' | '?' | '!' | '#' | '|' | '[' | ']' | '@')
        });
    if bad {
        return Err(Error::InvalidHostName {
            host: host.escape_debug().to_string(),
        });
    }
    Ok(())
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| Error::io(dir, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::certificate::Builder;
    use russh::keys::{Algorithm, PrivateKey};

    fn key() -> PrivateKey {
        PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap()
    }

    fn line(field: &str, k: &PublicKey) -> String {
        format!("{field} {}", k.to_openssh().unwrap())
    }

    fn kh(text: &str) -> KnownHosts {
        let kh = KnownHosts::parse(text, Path::new("known_hosts"));
        assert!(kh.warnings.is_empty(), "{:?}", kh.warnings);
        kh
    }

    #[test]
    fn trusted_unknown_and_changed() {
        let (a, b) = (key(), key());
        let kh = kh(&line("example.com,10.0.0.1", a.public_key()));
        assert!(matches!(
            kh.check_key("example.com", 22, a.public_key()),
            HostKeyVerdict::Trusted { line: 1, .. }
        ));
        assert!(matches!(
            kh.check_key("10.0.0.1", 22, a.public_key()),
            HostKeyVerdict::Trusted { .. }
        ));
        assert!(matches!(
            kh.check_key("example.com", 22, b.public_key()),
            HostKeyVerdict::Changed { line: 1, .. }
        ));
        assert_eq!(
            kh.check_key("other.com", 22, a.public_key()),
            HostKeyVerdict::Unknown {
                other_algorithms: vec![]
            }
        );
    }

    #[test]
    fn non_default_port_uses_brackets() {
        let a = key();
        let kh = kh(&line("[example.com]:2222", a.public_key()));
        assert!(matches!(
            kh.check_key("example.com", 2222, a.public_key()),
            HostKeyVerdict::Trusted { .. }
        ));
        assert!(matches!(
            kh.check_key("example.com", 22, a.public_key()),
            HostKeyVerdict::Unknown { .. }
        ));
    }

    #[test]
    fn wildcards_and_negation() {
        let a = key();
        let kh = kh(&line("*.corp,!evil.corp", a.public_key()));
        assert!(matches!(
            kh.check_key("db.corp", 22, a.public_key()),
            HostKeyVerdict::Trusted { .. }
        ));
        assert!(matches!(
            kh.check_key("evil.corp", 22, a.public_key()),
            HostKeyVerdict::Unknown { .. }
        ));
    }

    #[test]
    fn hashed_entries() {
        let a = key();
        let salt = [7u8; 20];
        let hash = hashed_name(&salt, "[db.example.com]:2200").unwrap();
        let field = format!("|1|{}|{}", B64.encode(salt), B64.encode(hash));
        let kh = kh(&line(&field, a.public_key()));
        assert!(matches!(
            kh.check_key("db.example.com", 2200, a.public_key()),
            HostKeyVerdict::Trusted { .. }
        ));
        assert!(matches!(
            kh.check_key("db.example.com", 22, a.public_key()),
            HostKeyVerdict::Unknown { .. }
        ));
    }

    #[test]
    fn hashed_entries_ignore_host_case() {
        let (a, b) = (key(), key());
        let salt = [3u8; 20];
        let hash = hashed_name(&salt, "db.example.com").unwrap();
        let field = format!("|1|{}|{}", B64.encode(salt), B64.encode(hash));
        let kh = kh(&line(&field, a.public_key()));
        assert!(matches!(
            kh.check_key("DB.Example.com", 22, a.public_key()),
            HostKeyVerdict::Trusted { .. }
        ));
        assert!(matches!(
            kh.check_key("DB.Example.com", 22, b.public_key()),
            HostKeyVerdict::Changed { .. }
        ));
    }

    #[test]
    fn append_rejects_pattern_metacharacters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        for bad in ["a,b", "*", "a b", "a\nb", "#x", "|1|x|y", "!a", ""] {
            assert!(
                append(&path, bad, 22, key().public_key(), false).is_err(),
                "{bad:?}"
            );
        }
        assert!(!path.exists());
        append(&path, "::1", 2222, key().public_key(), false).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("[::1]:2222 ")
        );
    }

    #[test]
    fn revoked_wins_over_trusted() {
        let a = key();
        let text = format!(
            "{}\n@revoked * {}",
            line("example.com", a.public_key()),
            a.public_key().to_openssh().unwrap()
        );
        assert!(matches!(
            kh(&text).check_key("example.com", 22, a.public_key()),
            HostKeyVerdict::Revoked { line: 2, .. }
        ));
    }

    #[test]
    fn other_algorithms_are_reported() {
        let ecdsa = PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256,
            },
        )
        .unwrap();
        let kh = kh(&line("example.com", ecdsa.public_key()));
        assert_eq!(
            kh.check_key("example.com", 22, key().public_key()),
            HostKeyVerdict::Unknown {
                other_algorithms: vec!["ecdsa-sha2-nistp256".into()]
            }
        );
        assert_eq!(kh.known_algorithms("example.com", 22).len(), 1);
    }

    #[test]
    fn malformed_lines_are_warnings() {
        let kh = KnownHosts::parse("garbage\n@weird host ssh-ed25519 AAAA\n", Path::new("kh"));
        assert_eq!(kh.warnings.len(), 2);
        assert!(kh.entries().is_empty());
    }

    fn host_cert(
        ca: &PrivateKey,
        host_key: &PrivateKey,
        principal: &str,
        ty: CertType,
    ) -> Certificate {
        let mut b = Builder::new_with_random_nonce(
            &mut russh::keys::key::safe_rng(),
            host_key.public_key().key_data().clone(),
            0,
            u64::MAX,
        )
        .unwrap();
        b.cert_type(ty).unwrap();
        b.key_id("test").unwrap();
        b.valid_principal(principal).unwrap();
        b.sign(ca).unwrap()
    }

    #[test]
    fn cert_authority() {
        let (ca, host_key) = (key(), key());
        let kh = kh(&format!(
            "@cert-authority *.example.com {}",
            ca.public_key().to_openssh().unwrap()
        ));
        assert!(kh.has_cert_authority("db.example.com", 22));
        let now = 1_700_000_000;

        let good = host_cert(&ca, &host_key, "db.example.com", CertType::Host);
        assert!(matches!(
            kh.check_certificate("db.example.com", 22, &good, now),
            HostKeyVerdict::TrustedCertificate { .. }
        ));
        let wrong_host = host_cert(&ca, &host_key, "web.example.com", CertType::Host);
        assert!(matches!(
            kh.check_certificate("db.example.com", 22, &wrong_host, now),
            HostKeyVerdict::InvalidCertificate { .. }
        ));
        let user_cert = host_cert(&ca, &host_key, "db.example.com", CertType::User);
        assert!(matches!(
            kh.check_certificate("db.example.com", 22, &user_cert, now),
            HostKeyVerdict::InvalidCertificate { .. }
        ));
        // Untrusted CA: falls back to the certified key, unknown here.
        let rogue = host_cert(&key(), &host_key, "db.example.com", CertType::Host);
        assert!(matches!(
            kh.check_certificate("db.example.com", 22, &rogue, now),
            HostKeyVerdict::Unknown { .. }
        ));
    }

    #[test]
    fn append_creates_private_file_and_is_readable_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ssh/known_hosts");
        let a = key();
        append(&path, "example.com", 2222, a.public_key(), false).unwrap();
        // A file without trailing newline must not get its last line merged.
        std::fs::write(&path, std::fs::read_to_string(&path).unwrap().trim_end()).unwrap();
        append(&path, "other.com", 22, a.public_key(), true).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.lines().nth(1).unwrap().starts_with("|1|"),
            "hashed entry"
        );
        assert!(!text.contains("other.com"));
        let kh = KnownHosts::load(&[path.clone(), dir.path().join("missing")]).unwrap();
        assert!(kh.warnings.is_empty());
        assert!(matches!(
            kh.check_key("example.com", 2222, a.public_key()),
            HostKeyVerdict::Trusted { line: 1, .. }
        ));
        assert!(matches!(
            kh.check_key("other.com", 22, a.public_key()),
            HostKeyVerdict::Trusted { line: 2, .. }
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dmode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
    }
}
