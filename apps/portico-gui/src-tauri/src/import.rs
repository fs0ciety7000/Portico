//! "Import hosts…" (phase 5; Termix and private keys: ADR "Import Termix
//! et imports vers les hôtes Portico").
//!
//! The export is chosen with the **native picker, in Rust**: the webview
//! never names a local path (same rule as uploads, see `files`). The format
//! (Termius or Termix) is detected from the content by the engine. The plan
//! stays here, under a single-use token, **with its key material** (opaque,
//! never serialized); the webview gets a preview (file name only, never the
//! source path; key paths shown `~/…`) and answers with the token: apply or
//! cancel. A reloading page drops the pending plan.
//!
//! Applying names the private keys to import by id (their fingerprint),
//! checked here against the plan's own keys, and an optional passphrase,
//! deserialized straight into `SecretString` (redacted `Debug`, never
//! serialized). Plans and reports never hold a secret.
//!
//! A re-import that points an earlier imported alias at another server
//! (`PlannedHost::previous`, revue sécurité) is applied only for the
//! aliases the user ticked: they come back as `redirect`, are checked here
//! against the plan's own destination changes, then fill
//! `ImportOptions::allow_redirect`. Nothing ticked: skipped by the engine
//! (`destination_changed`).

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use portico_core::import::{
    IMPORTED_KEYS_DIR, IgnoredSecrets, ImportNotice, ImportOptions, ImportPlan, ImportReport,
    KeyFormat, KeyId, KeySelection, PlannedHost, PlannedKey,
};
use portico_core::{Engine, ErrorSummary};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

use crate::dto::display_path;

/// The plan waiting for the user's answer (one at a time).
#[derive(Debug, Default)]
pub(crate) struct Imports {
    pending: Mutex<Option<Pending>>,
    next_token: Mutex<u64>,
}

#[derive(Debug)]
struct Pending {
    token: u64,
    source: PathBuf,
    plan: ImportPlan,
}

impl Imports {
    /// Forgets the pending plan (page reload, cancel): its keys are wiped.
    pub(crate) fn clear(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn keep(&self, source: PathBuf, plan: ImportPlan) -> u64 {
        let mut next = self
            .next_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *next = next.saturating_add(1);
        let token = *next;
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
            token,
            source,
            plan,
        });
        token
    }

    /// Takes the plan of `token` once `check` accepts it; a refused
    /// request leaves the plan pending (the user can fix the choice).
    fn take_if<T>(
        &self,
        token: u64,
        check: impl FnOnce(&ImportPlan) -> Result<T, ErrorSummary>,
    ) -> Result<(Pending, T), ErrorSummary> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let checked = match pending.as_ref() {
            Some(p) if p.token == token => check(&p.plan)?,
            _ => return Err(expired()),
        };
        pending.take().map(|p| (p, checked)).ok_or_else(expired)
    }

    fn source(&self, token: u64) -> Option<PathBuf> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|p| p.token == token)
            .map(|p| p.source.clone())
    }
}

/// Where an earlier imported host points now (`PlannedHost::previous`):
/// the destination only, its key shown `~/…` (a redirect never keeps it).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DestinationDto {
    hostname: String,
    user: Option<String>,
    /// `None` for 22.
    port: Option<u16>,
    identity_file: Option<String>,
}

/// One host of the preview: `PlannedHost` with `previous` reduced to a
/// [`DestinationDto`] (no absolute path reaches the webview).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlannedHostDto {
    alias: String,
    label: String,
    hostname: String,
    user: Option<String>,
    port: Option<u16>,
    group: Option<String>,
    tags: Vec<String>,
    notes: Option<String>,
    record: usize,
    favorite: bool,
    key: Option<KeyId>,
    update: bool,
    /// Set when this update changes the destination: applied only if the
    /// user accepts it ([`ImportApplyDto::redirect`]).
    previous: Option<DestinationDto>,
    source_id: String,
}

impl From<PlannedHost> for PlannedHostDto {
    fn from(h: PlannedHost) -> Self {
        Self {
            previous: h.previous.map(|p| DestinationDto {
                hostname: p.hostname,
                user: p.user,
                port: p.port,
                identity_file: p.identity_file.as_deref().map(display_path),
            }),
            alias: h.alias,
            label: h.label,
            hostname: h.hostname,
            user: h.user,
            port: h.port,
            group: h.group,
            tags: h.tags,
            notes: h.notes,
            record: h.record,
            favorite: h.favorite,
            key: h.key,
            update: h.update,
            source_id: h.source_id,
        }
    }
}

/// A private key of the export, as the preview lists it (no material).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlannedKeyDto {
    /// Sent back to select the key (its `SHA256:…` fingerprint).
    id: String,
    algorithm: String,
    fingerprint: String,
    format: KeyFormat,
    encrypted: bool,
    /// `false`: the fingerprint is only claimed by the export (encrypted
    /// OpenSSH key), never matched with a local key.
    fingerprint_verified: bool,
    aliases: Vec<String>,
    /// The local copy reused instead (`~/…`), if any.
    local: Option<String>,
}

impl From<PlannedKey> for PlannedKeyDto {
    fn from(k: PlannedKey) -> Self {
        Self {
            id: k.id.as_str().to_owned(),
            algorithm: k.algorithm,
            fingerprint: k.fingerprint,
            format: k.format,
            encrypted: k.encrypted,
            fingerprint_verified: k.fingerprint_verified,
            aliases: k.aliases,
            local: k.local.as_deref().map(display_path),
        }
    }
}

/// What the import dialog shows before writing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportPreviewDto {
    token: u64,
    /// The export's file name (never its path).
    file_name: String,
    /// The tool the export comes from (`Termius`, `Termix`).
    source: &'static str,
    /// Human name of the detected format.
    format: &'static str,
    /// The alias prefix of the source (`termius-`, `termix-`).
    prefix_text: &'static str,
    records: usize,
    prefix: bool,
    hosts: Vec<PlannedHostDto>,
    skipped: Vec<ImportNotice>,
    warnings: Vec<ImportNotice>,
    secrets: IgnoredSecrets,
    secrets_advice: Option<String>,
    identity_files: Vec<String>,
    /// Where the hosts will be written (`hosts.conf`, Portico's own folder).
    target: String,
    include_line: String,
    /// Hosts of an earlier import that this plan updates.
    replaces: Option<usize>,
    /// Private keys found (Termix); none is selected by default.
    keys: Vec<PlannedKeyDto>,
    /// Where selected keys are written (`~/.ssh/portico/`).
    keys_dir: String,
}

impl ImportPreviewDto {
    fn new(token: u64, plan: ImportPlan, keys_dir: String) -> Self {
        let file_name = plan
            .source
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect()
            })
            .unwrap_or_default();
        let source = plan.source_kind();
        Self {
            token,
            file_name,
            source: source.label(),
            format: plan.format.label(),
            prefix_text: source.prefix(),
            records: plan.records,
            prefix: plan.options.prefix,
            secrets_advice: plan.secrets.advice(),
            secrets: plan.secrets,
            target: display_path(&plan.target),
            include_line: plan.include_line,
            replaces: plan.replaces,
            hosts: plan.hosts.into_iter().map(PlannedHostDto::from).collect(),
            skipped: plan.skipped,
            warnings: plan.warnings,
            identity_files: plan.identity_files,
            keys: plan.keys.into_iter().map(PlannedKeyDto::from).collect(),
            keys_dir,
        }
    }
}

/// What happened to the keys (paths `~/…`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeyOutcomeDto {
    imported: Vec<String>,
    reused: Vec<String>,
    skipped: usize,
}

/// What the import did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReportDto {
    format_label: &'static str,
    source: &'static str,
    target: String,
    unchanged: bool,
    imported: Vec<String>,
    updated: Vec<String>,
    skipped: Vec<ImportNotice>,
    warnings: Vec<ImportNotice>,
    secrets: IgnoredSecrets,
    secrets_advice: Option<String>,
    identity_files: Vec<String>,
    meta_updated: usize,
    include_line: String,
    include_active: bool,
    keys: KeyOutcomeDto,
}

impl From<ImportReport> for ImportReportDto {
    fn from(r: ImportReport) -> Self {
        let shown = |paths: Vec<PathBuf>| paths.iter().map(|p| display_path(p)).collect();
        Self {
            format_label: r.format.label(),
            source: r.format.source().label(),
            target: display_path(&r.target),
            unchanged: r.unchanged,
            imported: r.imported,
            updated: r.updated,
            skipped: r.skipped,
            warnings: r.warnings,
            secrets_advice: r.secrets.advice(),
            secrets: r.secrets,
            identity_files: r.identity_files,
            meta_updated: r.meta_updated,
            include_line: r.include_line,
            include_active: r.include_active,
            keys: KeyOutcomeDto {
                imported: shown(r.keys.imported),
                reused: shown(r.keys.reused),
                skipped: r.keys.skipped,
            },
        }
    }
}

/// The user's answer to a preview.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImportApplyDto {
    token: u64,
    /// The prefix choice the preview showed (must match the plan).
    prefix: bool,
    /// Ids of the keys to import ([`PlannedKeyDto::id`]).
    keys: Vec<String>,
    /// Aliases whose destination change the user accepted (only aliases
    /// of the plan with `previous`).
    redirect: Vec<String>,
    // Deserialized straight into `SecretString`: no intermediate `String`.
    passphrase: Option<SecretString>,
}

// Manual impl: the derived one would print the passphrase.
impl std::fmt::Debug for ImportApplyDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportApplyDto")
            .field("token", &self.token)
            .field("prefix", &self.prefix)
            .field("keys", &self.keys)
            .field("redirect", &self.redirect)
            .field(
                "passphrase",
                &self.passphrase.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl ImportApplyDto {
    /// The engine's options, once the request matches `plan`: the prefix
    /// it was planned with, only key ids it holds, and only aliases whose
    /// destination it changes (an empty passphrase is none).
    fn options_for(self, plan: &ImportPlan) -> Result<ImportOptions, ErrorSummary> {
        if self.prefix != plan.options.prefix {
            return Err(ErrorSummary {
                code: "import_stale".into(),
                message: "this preview does not match the prefix choice".into(),
                hint: Some("Toggle the prefix again, then import.".into()),
            });
        }
        let known: BTreeSet<&str> = plan.keys.iter().map(|k| k.id.as_str()).collect();
        let mut ids = BTreeSet::new();
        for id in self.keys {
            if !known.contains(id.as_str()) {
                return Err(ErrorSummary {
                    code: "import_key_unknown".into(),
                    message: "a selected key is not in this export".into(),
                    hint: Some("Choose the export file again.".into()),
                });
            }
            ids.insert(KeyId::new(id));
        }
        let changes: BTreeSet<&str> = plan
            .hosts
            .iter()
            .filter(|h| h.previous.is_some())
            .map(|h| h.alias.as_str())
            .collect();
        let mut allow_redirect = BTreeSet::new();
        for alias in self.redirect {
            if !changes.contains(alias.as_str()) {
                return Err(ErrorSummary {
                    code: "import_redirect_unknown".into(),
                    message: "an accepted destination change is not in this preview".into(),
                    hint: Some("Choose the export file again.".into()),
                });
            }
            allow_redirect.insert(alias);
        }
        let keys = if ids.is_empty() {
            KeySelection::None
        } else {
            KeySelection::Some(ids)
        };
        let key_passphrase = self
            .passphrase
            .filter(|p| !p.expose_secret().is_empty() && keys != KeySelection::None);
        Ok(ImportOptions {
            prefix: self.prefix,
            keys,
            key_passphrase,
            allow_redirect,
        })
    }
}

fn expired() -> ErrorSummary {
    ErrorSummary {
        code: "import_expired".into(),
        message: "this import preview is no longer available".into(),
        hint: Some("Choose the export file again.".into()),
    }
}

/// Native file picker, then the plan; `None` when the user cancelled.
#[tauri::command]
pub(crate) async fn import_pick<R: Runtime>(
    app: AppHandle<R>,
    engine: State<'_, Engine>,
    imports: State<'_, Imports>,
    prefix: bool,
) -> Result<Option<ImportPreviewDto>, ErrorSummary> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Import hosts (Termius or Termix export)")
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    let Some(source) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    plan(&engine, &imports, source, prefix).await.map(Some)
}

/// The same export again, with or without the source's prefix.
#[tauri::command]
pub(crate) async fn import_replan(
    engine: State<'_, Engine>,
    imports: State<'_, Imports>,
    token: u64,
    prefix: bool,
) -> Result<ImportPreviewDto, ErrorSummary> {
    let source = imports.source(token).ok_or_else(expired)?;
    plan(&engine, &imports, source, prefix).await
}

async fn plan(
    engine: &Engine,
    imports: &Imports,
    source: PathBuf,
    prefix: bool,
) -> Result<ImportPreviewDto, ErrorSummary> {
    imports.clear();
    let plan = engine
        .plan_import(
            source.clone(),
            ImportOptions {
                prefix,
                ..ImportOptions::default()
            },
        )
        .await
        .map_err(|e| e.summary())?;
    let keys_dir = format!(
        "{}/",
        display_path(&engine.config().paths.ssh_dir.join(IMPORTED_KEYS_DIR))
    );
    let token = imports.keep(source, plan.clone());
    Ok(ImportPreviewDto::new(token, plan, keys_dir))
}

/// Writes the previewed plan (consumed) with the keys chosen.
#[tauri::command]
pub(crate) async fn import_apply(
    engine: State<'_, Engine>,
    imports: State<'_, Imports>,
    request: ImportApplyDto,
) -> Result<ImportReportDto, ErrorSummary> {
    let token = request.token;
    let (pending, options) = imports.take_if(token, |plan| request.options_for(plan))?;
    let report = engine
        .apply_import(pending.plan, options)
        .await
        .map_err(|e| e.summary())?;
    Ok(report.into())
}

/// The dialog was closed without importing.
#[tauri::command]
pub(crate) fn import_cancel(imports: State<'_, Imports>) {
    imports.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use portico_core::managed_hosts::ManagedHost;

    fn plan_with_keys(dir: &std::path::Path, keys: &[&str]) -> ImportPlan {
        let paths = portico_core::store::Paths::rooted(dir);
        ImportPlan {
            source: dir.join("secret folder/export.json"),
            format: portico_core::import::ImportFormat::TermixJson,
            options: ImportOptions::default(),
            records: 0,
            hosts: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            secrets: IgnoredSecrets::default(),
            identity_files: Vec::new(),
            target: portico_core::import::termius_target(&paths),
            include_line: "Include x".into(),
            replaces: None,
            keys: keys
                .iter()
                .map(|id| PlannedKey {
                    id: KeyId::new(*id),
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: (*id).into(),
                    format: KeyFormat::OpenSsh,
                    encrypted: false,
                    aliases: vec!["web".into()],
                    local: Some(dir.join(".ssh/id_ed25519")),
                    fingerprint_verified: true,
                })
                .collect(),
            key_material: portico_core::import::KeyMaterial::default(),
        }
    }

    fn request(json: serde_json::Value) -> ImportApplyDto {
        serde_json::from_value(json).expect("request")
    }

    #[test]
    fn tokens_are_single_use_and_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan_with_keys(dir.path(), &[]);
        let imports = Imports::default();
        let first = imports.keep("a".into(), plan.clone());
        let second = imports.keep("b".into(), plan.clone());
        assert_ne!(first, second);
        let ok = |_: &ImportPlan| Ok(());
        assert!(
            imports.take_if(first, ok).is_err(),
            "replaced by the second"
        );
        assert_eq!(imports.source(second), Some(PathBuf::from("b")));
        assert!(imports.take_if(second, ok).is_ok());
        assert_eq!(
            imports.take_if(second, ok).unwrap_err().code,
            "import_expired",
            "single use"
        );

        let preview =
            serde_json::to_string(&ImportPreviewDto::new(3, plan, "~/x/".into())).expect("json");
        assert!(
            preview.contains("\"fileName\":\"export.json\""),
            "{preview}"
        );
        assert!(preview.contains("\"source\":\"Termix\""), "{preview}");
        assert!(preview.contains("\"prefixText\":\"termix-\""), "{preview}");
        assert!(!preview.contains("secret folder"), "{preview}");
    }

    #[test]
    fn key_ids_must_come_from_the_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan_with_keys(dir.path(), &["SHA256:aaa", "SHA256:bbb"]);
        let imports = Imports::default();
        let token = imports.keep("a".into(), plan);
        let answer = |keys: serde_json::Value| {
            request(serde_json::json!({
                "token": token, "prefix": false, "keys": keys, "redirect": [], "passphrase": null
            }))
        };

        // An id the plan does not hold (a path, another key): refused, and
        // the plan stays pending so the user can answer again.
        for bad in ["SHA256:zzz", "/home/u/.ssh/id_ed25519", "", "sha256:aaa"] {
            let req = answer(serde_json::json!(["SHA256:aaa", bad]));
            let err = imports
                .take_if(token, |plan| req.options_for(plan))
                .unwrap_err();
            assert_eq!(err.code, "import_key_unknown", "{bad}");
        }
        // A prefix the preview did not show: refused too.
        let mut req = answer(serde_json::json!([]));
        req.prefix = true;
        let err = imports
            .take_if(token, |plan| req.options_for(plan))
            .unwrap_err();
        assert_eq!(err.code, "import_stale");

        let req = answer(serde_json::json!(["SHA256:bbb", "SHA256:bbb"]));
        let (_, options) = imports
            .take_if(token, |plan| req.options_for(plan))
            .expect("known ids");
        assert!(options.keys.contains(&KeyId::new("SHA256:bbb")));
        assert!(!options.keys.contains(&KeyId::new("SHA256:aaa")));
        assert!(imports.source(token).is_none(), "consumed once accepted");

        // No key ticked: no selection.
        let plan = plan_with_keys(dir.path(), &["SHA256:aaa"]);
        let options = answer(serde_json::json!([])).options_for(&plan).unwrap();
        assert_eq!(options.keys, KeySelection::None);
    }

    fn planned_host(alias: &str, previous: Option<ManagedHost>) -> PlannedHost {
        PlannedHost {
            alias: alias.into(),
            label: alias.into(),
            hostname: "198.51.100.99".into(),
            user: Some("deploy".into()),
            port: None,
            group: None,
            tags: Vec::new(),
            notes: None,
            record: 1,
            favorite: false,
            key: None,
            update: true,
            previous,
            source_id: "1".into(),
        }
    }

    fn earlier(alias: &str, home: &std::path::Path) -> ManagedHost {
        ManagedHost {
            alias: alias.into(),
            hostname: "203.0.113.10".into(),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_file: Some(home.join(".ssh/portico/web")),
            proxy_jump: None,
        }
    }

    #[test]
    fn accepted_redirects_must_be_destination_changes_of_the_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut plan = plan_with_keys(dir.path(), &[]);
        plan.hosts = vec![
            planned_host("web", Some(earlier("web", dir.path()))),
            planned_host("db", None),
        ];
        let answer = |redirect: serde_json::Value| {
            request(serde_json::json!({
                "token": 1, "prefix": false, "keys": [], "redirect": redirect, "passphrase": null
            }))
        };

        // An alias without a destination change, absent from the plan, or
        // spelled otherwise: refused, nothing is redirected.
        for bad in ["db", "mail", "WEB", "", "web "] {
            let err = answer(serde_json::json!(["web", bad]))
                .options_for(&plan)
                .unwrap_err();
            assert_eq!(err.code, "import_redirect_unknown", "{bad:?}");
        }
        // Nothing ticked: no redirect (the engine skips the change).
        let options = answer(serde_json::json!([])).options_for(&plan).unwrap();
        assert!(options.allow_redirect.is_empty());

        let options = answer(serde_json::json!(["web", "web"]))
            .options_for(&plan)
            .expect("a change of the plan");
        assert_eq!(options.allow_redirect, BTreeSet::from(["web".to_owned()]));

        // The field is required: an older webview cannot skip the choice.
        let missing: Result<ImportApplyDto, _> = serde_json::from_value(serde_json::json!({
            "token": 1, "prefix": false, "keys": [], "passphrase": null
        }));
        assert!(missing.is_err());
    }

    #[test]
    fn preview_shows_destination_changes_and_unverified_fingerprints() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let mut plan = plan_with_keys(dir.path(), &["SHA256:aaa"]);
        if let Some(k) = plan.keys.first_mut() {
            k.encrypted = true;
            k.fingerprint_verified = false;
            k.local = None;
        }
        plan.hosts = vec![
            planned_host("web", Some(earlier("web", &home))),
            planned_host("db", None),
        ];
        let json =
            serde_json::to_value(ImportPreviewDto::new(1, plan, "~/x/".into())).expect("json");
        let web = &json["hosts"][0];
        assert_eq!(web["previous"]["hostname"], "203.0.113.10");
        assert_eq!(web["previous"]["user"], "deploy");
        assert_eq!(web["previous"]["port"], 2222);
        assert_eq!(web["previous"]["identityFile"], "~/.ssh/portico/web");
        assert!(web["previous"].get("alias").is_none(), "{web}");
        assert_eq!(web["hostname"], "198.51.100.99");
        assert_eq!(web["sourceId"], "1");
        assert!(json["hosts"][1]["previous"].is_null());
        assert_eq!(json["keys"][0]["fingerprintVerified"], false);
        let text = json.to_string();
        assert!(
            !text.contains(&home.display().to_string()),
            "no absolute path: {text}"
        );
    }

    #[test]
    fn host_key_prompt_says_a_remembered_password_is_sent() {
        let kind = portico_core::events::PromptKind::UnknownHostKey {
            host: "198.51.100.99".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:aaa".into(),
            other_algorithms: Vec::new(),
            sends_remembered_password: true,
        };
        let json = serde_json::to_value(&kind).expect("json");
        assert_eq!(json["kind"], "unknown_host_key", "{json}");
        assert_eq!(json["sends_remembered_password"], true, "{json}");
    }

    #[test]
    fn passphrase_is_a_secret_never_serialized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plan = plan_with_keys(dir.path(), &["SHA256:aaa"]);
        let req = request(serde_json::json!({
            "token": 1, "prefix": false, "keys": ["SHA256:aaa"], "redirect": [], "passphrase": "correct horse"
        }));
        assert!(!format!("{req:?}").contains("correct horse"));
        let options = req.options_for(&plan).unwrap();
        assert_eq!(
            options
                .key_passphrase
                .as_ref()
                .map(|p| p.expose_secret().to_owned())
                .as_deref(),
            Some("correct horse")
        );
        let json = serde_json::to_string(&options).expect("json");
        assert!(!json.contains("correct horse"), "{json}");
        assert!(!format!("{options:?}").contains("correct horse"));

        // Empty, or no key selected: no passphrase reaches the engine.
        for (keys, pass) in [
            (serde_json::json!(["SHA256:aaa"]), ""),
            (serde_json::json!([]), "x"),
        ] {
            let req = request(serde_json::json!({
                "token": 1, "prefix": false, "keys": keys, "redirect": [], "passphrase": pass
            }));
            assert!(req.options_for(&plan).unwrap().key_passphrase.is_none());
        }
        // No path or other field can ride along.
        let extra: Result<ImportApplyDto, _> = serde_json::from_value(serde_json::json!({
            "token": 1, "prefix": false, "keys": [], "redirect": [], "passphrase": null, "path": "/etc/shadow"
        }));
        assert!(extra.is_err());
    }

    #[test]
    fn report_paths_are_shown_from_home() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let report = ImportReport {
            format: portico_core::import::ImportFormat::TermixJson,
            target: home.join(".config/portico/hosts.conf"),
            unchanged: false,
            imported: vec!["web".into()],
            updated: vec!["web".into()],
            skipped: Vec::new(),
            warnings: Vec::new(),
            secrets: IgnoredSecrets::default(),
            identity_files: Vec::new(),
            meta_updated: 0,
            include_line: "Include x".into(),
            include_active: false,
            keys: portico_core::import::KeyOutcome {
                imported: vec![home.join(".ssh/portico/web")],
                reused: vec![home.join(".ssh/id_ed25519")],
                skipped: 1,
            },
        };
        let json = serde_json::to_value(ImportReportDto::from(report)).expect("json");
        assert_eq!(json["keys"]["imported"][0], "~/.ssh/portico/web");
        assert_eq!(json["keys"]["reused"][0], "~/.ssh/id_ed25519");
        assert_eq!(json["keys"]["skipped"], 1);
        assert_eq!(json["source"], "Termix");
        assert_eq!(json["updated"][0], "web");
    }
}
