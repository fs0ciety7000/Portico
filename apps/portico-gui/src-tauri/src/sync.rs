//! Encrypted folder sync (phase 11, S1c): IPC around `Engine::sync_*`
//! (`docs/SYNC.md` §6 and §10 "Ce que S1c attend de S1b").
//!
//! Invariants:
//! - The sync folder is chosen with the **native picker, in Rust**: the
//!   webview never names a local path (same rule as uploads and imports).
//!   The picked folder stays here; create and join use it, and the webview
//!   only sees it for display (`~/…`).
//! - Passphrases arrive as `SecretString` (deserialized directly, redacted
//!   `Debug`, never serialized back), except a **generated** one: the core
//!   hands it out once ([`GeneratedPassphrase`], not `Clone`, not
//!   constructible from text) and only that value gets the diceware credit.
//!   It stays here under a single-use token; its text is sent to the webview
//!   once, to be shown and written down, and create / change / reset name it
//!   by token. A reloading page drops it and the picked folder.
//! - Statuses, devices, conflicts, issues and pending confirmations are the
//!   core's own serializable types (`portico_core::sync::api`): none of them
//!   holds a secret. Confirmations go back with the hash that was shown
//!   (`destination_hash`, snippet `hash`); the core refuses a stale one
//!   (`sync_not_pending`).

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use portico_core::sync::{
    DisableOptions, FolderProbe, JoinBulkChoice, JoinPreview, NewPassphrase, PassphraseStrength,
    PendingHostChange, SyncConflict, SyncCreate, SyncDevice, SyncIssue, SyncJoin, SyncOptions,
    SyncResolution, SyncStatus, UnconfirmedSnippet, assess_passphrase,
};
use portico_core::{Engine, ErrorSummary};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, Serializer};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

use crate::dto::display_path;

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

/// What the sync dialogs keep on the Rust side between two calls.
#[derive(Debug, Default)]
pub(crate) struct SyncUi {
    /// The folder picked for create / join.
    folder: Mutex<Option<PathBuf>>,
    /// The latest generated passphrase, under its token (single use).
    generated: Mutex<Option<(u64, NewPassphrase)>>,
    next: AtomicU64,
}

impl SyncUi {
    /// A reloading page: forget the picked folder and the generated
    /// passphrase (zeroized on drop).
    pub(crate) fn clear(&self) {
        *self.folder.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self
            .generated
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn picked(&self) -> Result<PathBuf, ErrorSummary> {
        self.folder
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(no_folder)
    }

    /// The passphrase a request names: typed, or the generated one by
    /// token (taken: it serves once).
    fn take(&self, passphrase: NewPassphraseDto) -> Result<NewPassphrase, ErrorSummary> {
        match passphrase {
            NewPassphraseDto::Typed { value } => Ok(NewPassphrase::Typed(value)),
            NewPassphraseDto::Generated { token } => {
                let mut slot = self
                    .generated
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match slot.take() {
                    Some((held, generated)) if held == token => Ok(generated),
                    other => {
                        *slot = other;
                        Err(generated_gone())
                    }
                }
            }
        }
    }
}

fn no_folder() -> ErrorSummary {
    ErrorSummary {
        code: "sync_no_folder".into(),
        message: "no sync folder chosen".into(),
        hint: Some("Choose the folder first.".into()),
    }
}

fn generated_gone() -> ErrorSummary {
    ErrorSummary {
        code: "sync_generated_used".into(),
        message: "this generated passphrase is no longer available".into(),
        hint: Some("Generate a new one, or type your own.".into()),
    }
}

/// A new passphrase as the webview names it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum NewPassphraseDto {
    /// Deserialized straight into `SecretString`.
    Typed { value: SecretString },
    /// The one [`sync_generate_passphrase`] returned.
    Generated { token: u64 },
}

/// `PassphraseStrength`, for the meter.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StrengthDto {
    pub estimated_bits: u32,
    pub diceware_words: usize,
    pub acceptable: bool,
}

impl From<PassphraseStrength> for StrengthDto {
    fn from(s: PassphraseStrength) -> Self {
        Self {
            estimated_bits: s.estimated_bits,
            diceware_words: s.diceware_words,
            acceptable: s.acceptable,
        }
    }
}

fn expose<S: Serializer>(secret: &SecretString, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose_secret())
}

/// A generated passphrase, sent once to be shown.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeneratedDto {
    pub token: u64,
    /// Shown once (and copyable); the webview drops it with the dialog.
    #[serde(serialize_with = "expose")]
    pub text: SecretString,
    pub strength: StrengthDto,
}

/// `SyncOverview`, the folder shown `~/…`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OverviewDto {
    pub status: SyncStatus,
    pub folder: Option<String>,
    pub device: Option<String>,
    pub remember_key: bool,
}

/// The folder picked for create / join, and what it holds.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PickedDto {
    pub display: String,
    pub probe: FolderProbe,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateDto {
    pub passphrase: NewPassphraseDto,
    pub device_name: String,
    pub remember_key: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinDto {
    pub passphrase: SecretString,
    pub device_name: String,
    pub remember_key: bool,
}

/// A received host as it was shown: `(alias, destination_hash)`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostDecisionDto {
    pub alias: String,
    pub destination_hash: String,
}

fn decisions(list: Vec<HostDecisionDto>) -> Vec<(String, String)> {
    list.into_iter()
        .map(|d| (d.alias, d.destination_hash))
        .collect()
}

// ── Status and lists ─────────────────────────────────────────────────────

/// Starts the background sync task (immediate pass, then every 30 s and
/// after local changes). Idempotent; nothing runs while sync is off.
#[tauri::command]
pub(crate) async fn sync_start(engine: State<'_, Engine>) -> Result<(), ErrorSummary> {
    engine.start_sync();
    Ok(())
}

#[tauri::command]
pub(crate) async fn sync_status(engine: State<'_, Engine>) -> Result<OverviewDto, ErrorSummary> {
    let overview = engine.sync_status().await.map_err(summary)?;
    Ok(OverviewDto {
        status: overview.status,
        folder: overview.folder.as_deref().map(display_path),
        device: overview.device,
        remember_key: overview.remember_key,
    })
}

#[tauri::command]
pub(crate) async fn sync_now(engine: State<'_, Engine>) -> Result<SyncStatus, ErrorSummary> {
    engine.sync_now().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_devices(
    engine: State<'_, Engine>,
) -> Result<Vec<SyncDevice>, ErrorSummary> {
    engine.sync_devices().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_conflicts(
    engine: State<'_, Engine>,
) -> Result<Vec<SyncConflict>, ErrorSummary> {
    engine.sync_conflicts().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_issues(engine: State<'_, Engine>) -> Result<Vec<SyncIssue>, ErrorSummary> {
    engine.sync_issues().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_pending_hosts(
    engine: State<'_, Engine>,
) -> Result<Vec<PendingHostChange>, ErrorSummary> {
    engine.sync_pending_hosts().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn synced_snippets_unconfirmed(
    engine: State<'_, Engine>,
) -> Result<Vec<UnconfirmedSnippet>, ErrorSummary> {
    engine.synced_snippets_unconfirmed().await.map_err(summary)
}

// ── Wizard ───────────────────────────────────────────────────────────────

/// This machine's short host name, the default device name.
#[tauri::command]
pub(crate) async fn sync_default_device_name() -> Result<String, ErrorSummary> {
    tauri::async_runtime::spawn_blocking(crate::keys::local_hostname)
        .await
        .map(Option::unwrap_or_default)
        .map_err(|_| ErrorSummary {
            code: "sync_device_name".into(),
            message: "cannot read this machine's name".into(),
            hint: None,
        })
}

/// Native folder picker, then what the folder holds. `None`: cancelled.
#[tauri::command]
pub(crate) async fn sync_pick_folder<R: Runtime>(
    app: AppHandle<R>,
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
) -> Result<Option<PickedDto>, ErrorSummary> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title("Sync folder");
    if let Some(home) = crate::dto::home_dir() {
        dialog = dialog.set_directory(home);
    }
    dialog.pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let Some(folder) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let display = display_path(&folder);
    *ui.folder.lock().unwrap_or_else(PoisonError::into_inner) = Some(folder.clone());
    let probe = engine.sync_probe_folder(folder).await.map_err(summary)?;
    Ok(Some(PickedDto { display, probe }))
}

/// Probes the picked folder again ("Check again" after a download).
#[tauri::command]
pub(crate) async fn sync_probe_again(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
) -> Result<FolderProbe, ErrorSummary> {
    let folder = ui.picked()?;
    engine.sync_probe_folder(folder).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_generate_passphrase(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
) -> Result<GeneratedDto, ErrorSummary> {
    let generated = engine.sync_generate_passphrase().await.map_err(summary)?;
    let text = generated.as_secret().clone();
    let passphrase = NewPassphrase::Generated(generated);
    let strength = passphrase.strength().into();
    let token = ui.next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    *ui.generated.lock().unwrap_or_else(PoisonError::into_inner) = Some((token, passphrase));
    Ok(GeneratedDto {
        token,
        text,
        strength,
    })
}

/// The meter while typing (what the core will enforce for a typed one).
#[tauri::command]
pub(crate) async fn sync_assess_passphrase(
    passphrase: SecretString,
) -> Result<StrengthDto, ErrorSummary> {
    tauri::async_runtime::spawn_blocking(move || StrengthDto::from(assess_passphrase(&passphrase)))
        .await
        .map_err(|_| ErrorSummary {
            code: "sync_assess".into(),
            message: "cannot estimate this passphrase".into(),
            hint: None,
        })
}

#[tauri::command]
pub(crate) async fn sync_create(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    request: CreateDto,
) -> Result<SyncStatus, ErrorSummary> {
    let folder = ui.picked()?;
    let passphrase = ui.take(request.passphrase)?;
    let status = engine
        .sync_create(SyncCreate {
            folder,
            passphrase,
            device_name: request.device_name,
            remember_key: request.remember_key,
        })
        .await
        .map_err(summary)?;
    ui.clear();
    Ok(status)
}

#[tauri::command]
pub(crate) async fn sync_join_preview(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    request: JoinDto,
) -> Result<JoinPreview, ErrorSummary> {
    let folder = ui.picked()?;
    engine
        .sync_join_preview(SyncJoin {
            folder,
            passphrase: request.passphrase,
            device_name: request.device_name,
            remember_key: request.remember_key,
        })
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_join_commit(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    token: String,
    bulk: JoinBulkChoice,
) -> Result<SyncStatus, ErrorSummary> {
    let status = engine
        .sync_join_commit(token, bulk)
        .await
        .map_err(summary)?;
    ui.clear();
    Ok(status)
}

// ── Panel ────────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn sync_unlock(
    engine: State<'_, Engine>,
    passphrase: SecretString,
) -> Result<SyncStatus, ErrorSummary> {
    engine.sync_unlock(passphrase).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_resolve(
    engine: State<'_, Engine>,
    id: String,
    resolution: SyncResolution,
    sides_hash: String,
) -> Result<SyncStatus, ErrorSummary> {
    engine
        .sync_resolve(id, resolution, sides_hash)
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_skip_on_this_device(
    engine: State<'_, Engine>,
    key: String,
) -> Result<SyncStatus, ErrorSummary> {
    engine.sync_skip_on_this_device(key).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_confirm_hosts(
    engine: State<'_, Engine>,
    hosts: Vec<HostDecisionDto>,
) -> Result<SyncStatus, ErrorSummary> {
    engine
        .sync_confirm_hosts(decisions(hosts))
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_reject_hosts(
    engine: State<'_, Engine>,
    hosts: Vec<HostDecisionDto>,
) -> Result<SyncStatus, ErrorSummary> {
    engine
        .sync_reject_hosts(decisions(hosts))
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn confirm_synced_snippet(
    engine: State<'_, Engine>,
    id: String,
    hash: String,
) -> Result<(), ErrorSummary> {
    engine
        .confirm_synced_snippet(id, hash)
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_acknowledge_alerts(
    engine: State<'_, Engine>,
) -> Result<SyncStatus, ErrorSummary> {
    engine.sync_acknowledge_alerts().await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_change_passphrase(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    old: SecretString,
    new: NewPassphraseDto,
) -> Result<SyncStatus, ErrorSummary> {
    let new = ui.take(new)?;
    engine
        .sync_change_passphrase(old, new)
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_reset_passphrase(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    new: NewPassphraseDto,
) -> Result<SyncStatus, ErrorSummary> {
    let new = ui.take(new)?;
    engine.sync_reset_passphrase(new).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_rename_device(
    engine: State<'_, Engine>,
    name: String,
) -> Result<SyncStatus, ErrorSummary> {
    engine.sync_rename_device(name).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_set_options(
    engine: State<'_, Engine>,
    remember_key: bool,
) -> Result<SyncStatus, ErrorSummary> {
    engine
        .sync_set_options(SyncOptions { remember_key })
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sync_disable(
    engine: State<'_, Engine>,
    ui: State<'_, SyncUi>,
    forget_key: bool,
    remove_my_file: bool,
) -> Result<SyncStatus, ErrorSummary> {
    let status = engine
        .sync_disable(DisableOptions {
            forget_key,
            remove_my_file,
        })
        .await
        .map_err(summary)?;
    ui.clear();
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_passphrases_are_secrets() {
        let dto: CreateDto = serde_json::from_str(
            r#"{"passphrase":{"kind":"typed","value":"correct horse battery"},"deviceName":"laptop","rememberKey":true}"#,
        )
        .unwrap();
        assert!(!format!("{dto:?}").contains("correct horse"));
        let ui = SyncUi::default();
        let NewPassphrase::Typed(secret) = ui.take(dto.passphrase).unwrap() else {
            panic!("typed");
        };
        assert_eq!(secret.expose_secret(), "correct horse battery");
    }

    #[test]
    fn a_generated_passphrase_serves_once_and_only_by_its_token() {
        let ui = SyncUi::default();
        let generated = portico_core::sync::generate_passphrase().unwrap();
        *ui.generated.lock().unwrap() = Some((7, NewPassphrase::Generated(generated)));
        // Another token: refused, and the passphrase stays.
        let wrong = ui
            .take(NewPassphraseDto::Generated { token: 8 })
            .unwrap_err();
        assert_eq!(wrong.code, "sync_generated_used");
        assert!(matches!(
            ui.take(NewPassphraseDto::Generated { token: 7 }).unwrap(),
            NewPassphrase::Generated(_)
        ));
        // Used: gone.
        assert_eq!(
            ui.take(NewPassphraseDto::Generated { token: 7 })
                .unwrap_err()
                .code,
            "sync_generated_used"
        );
    }

    #[test]
    fn a_generated_passphrase_is_sent_once_as_text() {
        let dto = GeneratedDto {
            token: 1,
            text: SecretString::from("alpha bravo"),
            strength: StrengthDto {
                estimated_bits: 77,
                diceware_words: 6,
                acceptable: true,
            },
        };
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["text"], "alpha bravo");
        assert_eq!(json["strength"]["estimatedBits"], 77);
        assert!(!format!("{dto:?}").contains("alpha"));
    }

    #[test]
    fn create_and_join_need_a_picked_folder() {
        let ui = SyncUi::default();
        assert_eq!(ui.picked().unwrap_err().code, "sync_no_folder");
        *ui.folder.lock().unwrap() = Some(PathBuf::from("/tmp/x"));
        assert!(ui.picked().is_ok());
        ui.clear();
        assert!(ui.picked().is_err());
    }

    #[test]
    fn host_decisions_carry_the_hash_shown() {
        let list: Vec<HostDecisionDto> =
            serde_json::from_str(r#"[{"alias":"web","destinationHash":"ab12"}]"#).unwrap();
        assert_eq!(decisions(list), vec![("web".to_owned(), "ab12".to_owned())]);
    }

    #[test]
    fn statuses_and_resolutions_keep_the_core_shape() {
        let status = serde_json::to_value(SyncStatus::Idle {
            last_pass: 5,
            devices: 2,
            conflicts: 1,
            issues: 0,
            pending_confirmations: 3,
        })
        .unwrap();
        assert_eq!(status["state"], "idle");
        assert_eq!(status["pending_confirmations"], 3);
        let r: SyncResolution = serde_json::from_str(r#"{"type":"keep_both","keep":1}"#).unwrap();
        assert_eq!(r, SyncResolution::KeepBoth { keep: 1 });
        let b: JoinBulkChoice = serde_json::from_str(r#""prefer_remote""#).unwrap();
        assert_eq!(b, JoinBulkChoice::PreferRemote);
    }
}
