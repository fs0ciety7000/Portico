//! "Import from Termius…" (phase 5).
//!
//! The export is chosen with the **native picker, in Rust**: the webview
//! never names a local path (same rule as uploads, see `files`). The plan
//! stays here, under a token; the webview gets a preview (file name only,
//! never the source path) and answers with the token: apply or cancel. A
//! reloading page drops the pending plan.
//!
//! Plans and reports come from `portico_core::import`: they never hold a
//! secret (counts only), and nothing is written before `import_apply`.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use portico_core::import::{
    IgnoredSecrets, ImportNotice, ImportOptions, ImportPlan, ImportReport, PlannedHost,
};
use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

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
    /// Forgets the pending plan (page reload, cancel).
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

    fn take(&self, token: u64) -> Option<Pending> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.as_ref().is_some_and(|p| p.token == token) {
            pending.take()
        } else {
            None
        }
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

/// What the import dialog shows before writing.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportPreviewDto {
    token: u64,
    /// The export's file name (never its path).
    file_name: String,
    /// Human name of the detected format.
    format: &'static str,
    records: usize,
    prefix: bool,
    hosts: Vec<PlannedHost>,
    skipped: Vec<ImportNotice>,
    warnings: Vec<ImportNotice>,
    secrets: IgnoredSecrets,
    secrets_advice: Option<String>,
    identity_files: Vec<String>,
    /// Where the hosts will be written (Portico's own folder).
    target: String,
    include_line: String,
    /// `Host` blocks already in the target, replaced by this import.
    replaces: Option<usize>,
}

impl ImportPreviewDto {
    fn new(token: u64, plan: ImportPlan) -> Self {
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
        Self {
            token,
            file_name,
            format: plan.format.label(),
            records: plan.records,
            prefix: plan.options.prefix,
            secrets_advice: plan.secrets.advice(),
            secrets: plan.secrets,
            target: plan.target.display().to_string(),
            include_line: plan.include_line,
            replaces: plan.replaces,
            hosts: plan.hosts,
            skipped: plan.skipped,
            warnings: plan.warnings,
            identity_files: plan.identity_files,
        }
    }
}

/// What the import did.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReportDto {
    #[serde(flatten)]
    report: ImportReport,
    format_label: &'static str,
    secrets_advice: Option<String>,
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
pub(crate) async fn import_termius_pick<R: Runtime>(
    app: AppHandle<R>,
    engine: State<'_, Engine>,
    imports: State<'_, Imports>,
    prefix: bool,
) -> Result<Option<ImportPreviewDto>, ErrorSummary> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Import from Termius")
        .pick_file(move |picked| {
            let _ = tx.send(picked);
        });
    let Some(source) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    plan(&engine, &imports, source, prefix).await.map(Some)
}

/// The same export again, with or without the `termius-` prefix.
#[tauri::command]
pub(crate) async fn import_termius_replan(
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
        .plan_termius_import(source.clone(), ImportOptions { prefix })
        .await
        .map_err(|e| e.summary())?;
    let token = imports.keep(source, plan.clone());
    Ok(ImportPreviewDto::new(token, plan))
}

/// Writes the previewed plan (consumed).
#[tauri::command]
pub(crate) async fn import_apply(
    engine: State<'_, Engine>,
    imports: State<'_, Imports>,
    token: u64,
) -> Result<ImportReportDto, ErrorSummary> {
    let pending = imports.take(token).ok_or_else(expired)?;
    let report = engine
        .apply_import(pending.plan)
        .await
        .map_err(|e| e.summary())?;
    Ok(ImportReportDto {
        format_label: report.format.label(),
        secrets_advice: report.secrets.advice(),
        report,
    })
}

/// The dialog was closed without importing.
#[tauri::command]
pub(crate) fn import_cancel(imports: State<'_, Imports>) {
    imports.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_single_use_and_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = portico_core::store::Paths::rooted(dir.path());
        let plan = ImportPlan {
            source: dir.path().join("secret folder/export.csv"),
            format: portico_core::import::TermiusFormat::TemplateCsv,
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
        };
        let imports = Imports::default();
        let first = imports.keep("a".into(), plan.clone());
        let second = imports.keep("b".into(), plan.clone());
        assert_ne!(first, second);
        assert!(imports.take(first).is_none(), "replaced by the second");
        assert_eq!(imports.source(second), Some(PathBuf::from("b")));
        assert!(imports.take(second).is_some());
        assert!(imports.take(second).is_none(), "single use");

        let preview = serde_json::to_string(&ImportPreviewDto::new(3, plan)).expect("json");
        assert!(preview.contains("\"fileName\":\"export.csv\""), "{preview}");
        assert!(!preview.contains("secret folder"), "{preview}");
    }
}
