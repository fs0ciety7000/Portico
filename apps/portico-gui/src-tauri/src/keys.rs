//! Keys, ssh-agent, copy-id and vault commands (phase 3).
//!
//! The webview never names a local path. A local key is designated by its
//! file name (`id_ed25519`), which is looked up in a fresh
//! `Engine::local_keys()` listing of `~/.ssh`: only a key Portico itself
//! discovered can be added to the agent, copied or read. A new key gets a
//! *name* only, validated by `portico-core` (no separator, no `..`).
//!
//! Secrets (key and vault passphrases) are deserialized straight into
//! `SecretString` and handed to the engine; no DTO holding one prints it.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use portico_core::AgentKey;
use portico_core::ids::SessionId;
use portico_core::keys::{CertValidity, KeyAlgorithm, KeyGenRequest, LocalKey};
use portico_core::sftp::display_safe;
use portico_core::vault::VaultStatus;
use portico_core::{CopyIdOutcome, Engine, ErrorSummary};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::dto::display_path;

/// Longest comment, key id or principal shown in the UI.
const DISPLAY_TEXT: usize = 256;
/// Most principals shown for a certificate.
const MAX_PRINCIPALS: usize = 16;
/// Longest agent lifetime offered (a week); longer means "until removed".
const MAX_LIFETIME_SECS: u64 = 7 * 24 * 3600;
/// Largest public key file read for the clipboard (RSA 8192 is ~1.4 KiB).
const MAX_PUBLIC_KEY_BYTES: u64 = 16 * 1024;

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ── DTOs ────────────────────────────────────────────────────────────────

/// An OpenSSH certificate attached to a key, with its standing now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CertificateDto {
    pub key_id: String,
    pub principals: Vec<String>,
    /// Unix seconds.
    pub valid_after: u64,
    /// Unix seconds (`u64::MAX` = forever, sent as `null`).
    pub valid_before: Option<u64>,
    pub validity: CertValidity,
}

impl CertificateDto {
    fn new(
        key_id: &str,
        principals: &[String],
        valid_after: u64,
        valid_before: u64,
        validity: CertValidity,
    ) -> Self {
        Self {
            key_id: display_safe(key_id, DISPLAY_TEXT).into_owned(),
            principals: principals
                .iter()
                .take(MAX_PRINCIPALS)
                .map(|p| display_safe(p, DISPLAY_TEXT).into_owned())
                .collect(),
            valid_after,
            valid_before: (valid_before != u64::MAX).then_some(valid_before),
            validity,
        }
    }
}

/// A key pair of `~/.ssh` (`id` designates it in later commands).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalKeyDto {
    /// File name of the private key (`id_ed25519`), unique in `~/.ssh`.
    pub id: String,
    /// `~/.ssh/id_ed25519` (for `IdentityFile` suggestions).
    pub private_display: String,
    pub public_display: String,
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    pub has_private: bool,
    pub certificate: Option<CertificateDto>,
}

pub(crate) fn key_id(key: &LocalKey) -> Option<String> {
    key.private_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
}

impl LocalKeyDto {
    fn new(key: &LocalKey, now: u64) -> Option<Self> {
        Some(Self {
            id: key_id(key)?,
            private_display: display_path(&key.private_path),
            public_display: display_path(&key.public_path),
            algorithm: key.algorithm.clone(),
            fingerprint: key.fingerprint.clone(),
            comment: display_safe(&key.comment, DISPLAY_TEXT).into_owned(),
            has_private: key.has_private,
            certificate: key.certificate.as_ref().map(|c| {
                CertificateDto::new(
                    &c.key_id,
                    &c.principals,
                    c.valid_after,
                    c.valid_before,
                    c.validity_at(now),
                )
            }),
        })
    }
}

/// An identity held by ssh-agent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentKeyDto {
    pub algorithm: String,
    pub fingerprint: String,
    pub comment: String,
    pub certificate: Option<CertificateDto>,
}

impl AgentKeyDto {
    fn new(key: &AgentKey, now: u64) -> Self {
        Self {
            algorithm: key.algorithm.clone(),
            fingerprint: key.fingerprint.clone(),
            comment: display_safe(&key.comment, DISPLAY_TEXT).into_owned(),
            certificate: key.certificate.as_ref().map(|c| {
                CertificateDto::new(
                    &c.key_id,
                    &c.principals,
                    c.valid_after,
                    c.valid_before,
                    c.validity_at(now),
                )
            }),
        }
    }
}

/// The key generation form. The passphrase is a `SecretString` from the
/// start (empty or absent: no passphrase).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyGenDto {
    pub name: String,
    pub algorithm: KeyAlgorithm,
    pub comment: String,
    pub passphrase: Option<SecretString>,
}

// Manual impl: the derived one would print the passphrase.
impl std::fmt::Debug for KeyGenDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyGenDto")
            .field("name", &self.name)
            .field("algorithm", &self.algorithm)
            .field("comment", &self.comment)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl From<KeyGenDto> for KeyGenRequest {
    fn from(dto: KeyGenDto) -> Self {
        Self {
            name: dto.name,
            algorithm: dto.algorithm,
            comment: dto.comment,
            passphrase: dto.passphrase.filter(|p| !p.expose_secret().is_empty()),
        }
    }
}

/// Defaults of the key generation form.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyDefaultsDto {
    /// `user@host`, like `ssh-keygen`.
    pub comment: String,
}

// ── Lookup ──────────────────────────────────────────────────────────────

/// The key named `id` in a fresh listing of `~/.ssh`.
async fn find_key(engine: &Engine, id: &str) -> Result<LocalKey, ErrorSummary> {
    let keys = engine.local_keys().await.map_err(summary)?;
    keys.into_iter()
        .find(|k| key_id(k).as_deref() == Some(id))
        .ok_or_else(|| unknown_key(id))
}

/// The private key file of the key named `id` in `keys` (a listing made by
/// the engine): the only way a webview choice becomes a path. An id that
/// is not a discovered key's file name (a path, `..`, a public-only key)
/// is refused.
pub(crate) fn private_key_path(
    keys: &[LocalKey],
    id: &str,
) -> Result<std::path::PathBuf, ErrorSummary> {
    let key = keys
        .iter()
        .find(|k| key_id(k).as_deref() == Some(id))
        .ok_or_else(|| unknown_key(id))?;
    private_path_of(key, id)
}

fn unknown_key(id: &str) -> ErrorSummary {
    ErrorSummary {
        code: "key_unknown".into(),
        message: format!("no key named {} in ~/.ssh", display_safe(id, 64)),
        hint: Some("Refresh the list: the key may have been moved or deleted.".into()),
    }
}

fn public_only(id: &str) -> ErrorSummary {
    ErrorSummary {
        code: "key_public_only".into(),
        message: format!("{} has no private key here", display_safe(id, 64)),
        hint: Some(
            "Only the .pub file is in ~/.ssh: this key cannot be used from this machine.".into(),
        ),
    }
}

fn private_path_of(key: &LocalKey, id: &str) -> Result<std::path::PathBuf, ErrorSummary> {
    if key.has_private {
        Ok(key.private_path.clone())
    } else {
        Err(public_only(id))
    }
}

// ── Keys ────────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn list_keys(engine: State<'_, Engine>) -> Result<Vec<LocalKeyDto>, ErrorSummary> {
    let now = unix_now();
    let keys = engine.local_keys().await.map_err(summary)?;
    Ok(keys
        .iter()
        .filter_map(|k| LocalKeyDto::new(k, now))
        .collect())
}

/// Async (and off the main thread): the host name may come from
/// `/etc/hostname`, read in `spawn_blocking`.
#[tauri::command]
pub(crate) async fn key_defaults() -> Result<KeyDefaultsDto, ErrorSummary> {
    tauri::async_runtime::spawn_blocking(key_defaults_blocking)
        .await
        .map_err(|_| ErrorSummary {
            code: "key_defaults".into(),
            message: "cannot read the default key comment".into(),
            hint: None,
        })
}

fn key_defaults_blocking() -> KeyDefaultsDto {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    let host = local_hostname();
    let comment = match (user.is_empty(), host) {
        (false, Some(host)) => format!("{user}@{host}"),
        (false, None) => user,
        (true, Some(host)) => host,
        (true, None) => String::new(),
    };
    KeyDefaultsDto {
        comment: display_safe(&comment, 128).replace(char::is_whitespace, ""),
    }
}

/// Short host name, when the system tells it without running a program.
fn local_hostname() -> Option<String> {
    let raw = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())?;
    let short = raw.trim().split('.').next()?.to_owned();
    (!short.is_empty()).then_some(short)
}

#[tauri::command]
pub(crate) async fn generate_key(
    engine: State<'_, Engine>,
    request: KeyGenDto,
) -> Result<LocalKeyDto, ErrorSummary> {
    let key = engine.generate_key(request.into()).await.map_err(summary)?;
    LocalKeyDto::new(&key, unix_now()).ok_or_else(|| unknown_key(""))
}

/// The public key line, for the clipboard.
#[tauri::command]
pub(crate) async fn public_key_text(
    engine: State<'_, Engine>,
    key: String,
) -> Result<String, ErrorSummary> {
    let found = find_key(&engine, &key).await?;
    let path = found.public_path.clone();
    let text = tauri::async_runtime::spawn_blocking(move || read_public(&path))
        .await
        .map_err(|_| unknown_key(&key))??;
    Ok(text)
}

fn read_public(path: &Path) -> Result<String, ErrorSummary> {
    let unreadable = |reason: String| ErrorSummary {
        code: "key_public".into(),
        message: format!("cannot read {}: {reason}", display_path(path)),
        hint: None,
    };
    let meta = std::fs::metadata(path).map_err(|e| unreadable(e.to_string()))?;
    if meta.len() > MAX_PUBLIC_KEY_BYTES {
        return Err(unreadable("file too large".into()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| unreadable(e.to_string()))?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| unreadable("empty file".into()))?;
    // One printable line, as `ssh-keygen` writes it.
    Ok(line.chars().filter(|c| !c.is_control()).collect())
}

// ── Agent ───────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn list_agent_keys(
    engine: State<'_, Engine>,
) -> Result<Vec<AgentKeyDto>, ErrorSummary> {
    let now = unix_now();
    let keys = engine.agent_keys().await.map_err(summary)?;
    Ok(keys.iter().map(|k| AgentKeyDto::new(k, now)).collect())
}

/// `ssh-add [-t lifetime]`. `key_needs_passphrase` asks the view for one.
#[tauri::command]
pub(crate) async fn agent_add(
    engine: State<'_, Engine>,
    key: String,
    passphrase: Option<SecretString>,
    lifetime_secs: Option<u64>,
) -> Result<AgentKeyDto, ErrorSummary> {
    let found = find_key(&engine, &key).await?;
    let private = private_path_of(&found, &key)?;
    let lifetime = agent_lifetime(lifetime_secs)?;
    let passphrase = passphrase.filter(|p| !p.expose_secret().is_empty());
    let added = engine
        .agent_add(private, passphrase, lifetime)
        .await
        .map_err(summary)?;
    Ok(AgentKeyDto::new(&added, unix_now()))
}

/// `None` keeps the key until removed; a duration outside
/// `1..=MAX_LIFETIME_SECS` is refused (never silently made unlimited).
fn agent_lifetime(lifetime_secs: Option<u64>) -> Result<Option<Duration>, ErrorSummary> {
    match lifetime_secs {
        None => Ok(None),
        Some(secs) if (1..=MAX_LIFETIME_SECS).contains(&secs) => {
            Ok(Some(Duration::from_secs(secs)))
        }
        Some(secs) => Err(ErrorSummary {
            code: "agent_lifetime".into(),
            message: format!("an agent lifetime of {secs} s is outside 1..={MAX_LIFETIME_SECS} s"),
            hint: Some("Pick one of the offered durations, or until removed.".into()),
        }),
    }
}

#[tauri::command]
pub(crate) async fn agent_remove(
    engine: State<'_, Engine>,
    fingerprint: String,
) -> Result<(), ErrorSummary> {
    engine.agent_remove(&fingerprint).await.map_err(summary)
}

// ── Copy-id ─────────────────────────────────────────────────────────────

/// Installs the key's public half for the account of `session`.
#[tauri::command]
pub(crate) async fn copy_id(
    engine: State<'_, Engine>,
    session: u64,
    key: String,
) -> Result<CopyIdOutcome, ErrorSummary> {
    let found = find_key(&engine, &key).await?;
    engine
        .copy_id(SessionId::from_raw(session), found.public_path)
        .await
        .map_err(summary)
}

/// Logs in again with this key only (a probe session), then disconnects.
#[tauri::command]
pub(crate) async fn verify_key_login(
    engine: State<'_, Engine>,
    session: u64,
    key: String,
) -> Result<(), ErrorSummary> {
    let found = find_key(&engine, &key).await?;
    let private = private_path_of(&found, &key)?;
    engine
        .verify_key_login(SessionId::from_raw(session), private)
        .await
        .map_err(summary)
}

// ── Vault ───────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn vault_status(engine: State<'_, Engine>) -> Result<VaultStatus, ErrorSummary> {
    engine.vault_status().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn vault_create(
    engine: State<'_, Engine>,
    passphrase: SecretString,
) -> Result<(), ErrorSummary> {
    engine.vault_create(passphrase).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn vault_unlock(
    engine: State<'_, Engine>,
    passphrase: SecretString,
) -> Result<(), ErrorSummary> {
    engine.vault_unlock(passphrase).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn vault_lock(engine: State<'_, Engine>) -> Result<(), ErrorSummary> {
    engine.vault_lock().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn vault_change_passphrase(
    engine: State<'_, Engine>,
    old: SecretString,
    new: SecretString,
) -> Result<(), ErrorSummary> {
    engine
        .vault_change_passphrase(old, new)
        .await
        .map_err(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_gen_dto_takes_the_passphrase_as_a_secret() {
        let dto: KeyGenDto = serde_json::from_str(
            r#"{"name":"id_ed25519_work","algorithm":{"kind":"ed25519"},"comment":"me@laptop","passphrase":"hunter2"}"#,
        )
        .unwrap();
        assert!(!format!("{dto:?}").contains("hunter2"));
        let request = KeyGenRequest::from(dto);
        assert_eq!(request.name, "id_ed25519_work");
        assert_eq!(request.algorithm, KeyAlgorithm::Ed25519);
        assert_eq!(request.passphrase.unwrap().expose_secret(), "hunter2");
    }

    #[test]
    fn an_empty_passphrase_means_none() {
        let dto: KeyGenDto = serde_json::from_str(
            r#"{"name":"k","algorithm":{"kind":"rsa","bits":4096},"comment":"","passphrase":""}"#,
        )
        .unwrap();
        let request = KeyGenRequest::from(dto);
        assert_eq!(request.algorithm, KeyAlgorithm::Rsa { bits: 4096 });
        assert!(request.passphrase.is_none());
    }

    #[test]
    fn local_keys_are_named_by_file_and_shown_safely() {
        let home = std::env::var("HOME").unwrap();
        let key = LocalKey {
            private_path: Path::new(&home).join(".ssh/id_ed25519"),
            public_path: Path::new(&home).join(".ssh/id_ed25519.pub"),
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            comment: "me\u{202e}@evil\u{1b}[2J".into(),
            has_private: true,
            certificate: None,
        };
        let dto = LocalKeyDto::new(&key, 0).unwrap();
        assert_eq!(dto.id, "id_ed25519");
        assert_eq!(dto.private_display, "~/.ssh/id_ed25519");
        assert!(!dto.comment.contains('\u{202e}'));
        assert!(!dto.comment.contains('\u{1b}'));
    }

    #[test]
    fn forever_certificates_have_no_end() {
        let dto = CertificateDto::new(
            "ci",
            &["deploy".into()],
            0,
            u64::MAX,
            CertValidity::Valid {
                expires_in_secs: None,
            },
        );
        assert_eq!(dto.valid_before, None);
        assert_eq!(dto.principals, vec!["deploy".to_owned()]);
    }

    #[test]
    fn public_key_files_give_one_clean_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k.pub");
        std::fs::write(&path, "\nssh-ed25519 AAAA me@laptop\r\n").unwrap();
        assert_eq!(read_public(&path).unwrap(), "ssh-ed25519 AAAA me@laptop");
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_public(&path).unwrap_err().code, "key_public");
    }

    #[test]
    fn out_of_range_lifetimes_are_refused() {
        assert_eq!(agent_lifetime(None).unwrap(), None);
        assert_eq!(
            agent_lifetime(Some(3600)).unwrap(),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(
            agent_lifetime(Some(MAX_LIFETIME_SECS)).unwrap(),
            Some(Duration::from_secs(MAX_LIFETIME_SECS))
        );
        for secs in [0, MAX_LIFETIME_SECS + 1, u64::MAX] {
            assert_eq!(
                agent_lifetime(Some(secs)).unwrap_err().code,
                "agent_lifetime",
                "{secs}"
            );
        }
    }

    #[test]
    fn missing_private_keys_are_reported() {
        let key = LocalKey {
            private_path: "/nowhere/k".into(),
            public_path: "/nowhere/k.pub".into(),
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            comment: String::new(),
            has_private: false,
            certificate: None,
        };
        assert_eq!(
            private_path_of(&key, "k").unwrap_err().code,
            "key_public_only"
        );
    }
}
