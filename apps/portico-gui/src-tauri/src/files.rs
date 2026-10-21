//! SFTP and tunnel commands.
//!
//! The webview never names a local path. Files to upload come from the
//! native picker or a drop on the window, both handled here in Rust, and the
//! webview only gets opaque tokens for them. Downloads go to one folder kept
//! here (the user's Downloads folder, or one chosen with the native picker),
//! and the file name is built by `portico_core::sftp::local_destination`.
//! A compromised page can therefore not read or write arbitrary local files.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use portico_core::forward::ForwardSpec;
use portico_core::ids::{SessionId, TransferId, TunnelId};
use portico_core::sftp::{
    Direction, EntryKind, OnConflict, RemoteEntry, TransferRequest, display_safe, join_remote,
    local_destination, remote_name,
};
use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_dialog::DialogExt;

/// Longest name shown in the UI (the raw name is kept for operations).
const DISPLAY_NAME: usize = 256;

/// Local files the user handed to Portico, by token.
#[derive(Debug, Default)]
pub(crate) struct Files {
    uploads: Mutex<HashMap<u64, PathBuf>>,
    next: AtomicU64,
    download_dir: Mutex<Option<PathBuf>>,
}

impl Files {
    /// Registers files picked or dropped by the user (folders are skipped:
    /// transfers are not recursive yet).
    pub(crate) fn register(&self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<UploadDto> {
        let mut uploads = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        paths
            .into_iter()
            .filter_map(|path| {
                let meta = std::fs::metadata(&path)
                    .ok()
                    .filter(std::fs::Metadata::is_file)?;
                let name = path.file_name()?.to_str()?.to_owned();
                let token = self.next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                uploads.insert(token, path);
                Some(UploadDto {
                    token,
                    display: display_safe(&name, DISPLAY_NAME).into_owned(),
                    size: meta.len(),
                })
            })
            .collect()
    }

    /// Forgets every token (the page that held them is gone).
    pub(crate) fn clear(&self) {
        self.uploads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn upload_path(&self, token: u64) -> Option<PathBuf> {
        self.uploads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token)
            .cloned()
    }

    fn download_dir(&self) -> PathBuf {
        self.download_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(default_download_dir)
    }
}

/// The XDG / macOS Downloads folder, else `~/Downloads` when it exists,
/// else the home folder.
fn default_download_dir() -> PathBuf {
    let dirs = directories::UserDirs::new();
    if let Some(dir) = dirs.as_ref().and_then(directories::UserDirs::download_dir) {
        return dir.to_path_buf();
    }
    let home = dirs
        .map(|d| d.home_dir().to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let downloads = home.join("Downloads");
    if downloads.is_dir() { downloads } else { home }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryDto {
    /// Raw name, for operations only.
    name: String,
    /// Safe to show: no controls, no bidi overrides, bounded length.
    display: String,
    path: String,
    kind: EntryKind,
    size: u64,
    mode: Option<u32>,
    /// Unix seconds.
    modified: Option<i64>,
}

impl From<RemoteEntry> for EntryDto {
    fn from(e: RemoteEntry) -> Self {
        Self {
            display: display_safe(&e.name, DISPLAY_NAME).into_owned(),
            modified: e.modified.map(time::OffsetDateTime::unix_timestamp),
            name: e.name,
            path: e.path,
            kind: e.kind,
            size: e.size,
            mode: e.mode,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UploadDto {
    token: u64,
    display: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForwardDto {
    /// `-L localhost:8080 → db:5432`.
    display: String,
    /// Listens beyond loopback: the UI must confirm before starting it.
    exposed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TunnelDto {
    id: u64,
    session: u64,
    display: String,
    exposed: bool,
}

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

// ── Remote browsing ───────────────────────────────────────────────────────

#[tauri::command]
pub(crate) async fn sftp_home(
    engine: State<'_, Engine>,
    session: u64,
) -> Result<String, ErrorSummary> {
    engine
        .sftp_home(SessionId::from_raw(session))
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sftp_list(
    engine: State<'_, Engine>,
    session: u64,
    path: String,
) -> Result<Vec<EntryDto>, ErrorSummary> {
    let entries = engine
        .sftp_list(SessionId::from_raw(session), &path)
        .await
        .map_err(summary)?;
    Ok(entries.into_iter().map(EntryDto::from).collect())
}

#[tauri::command]
pub(crate) async fn sftp_mkdir(
    engine: State<'_, Engine>,
    session: u64,
    path: String,
) -> Result<(), ErrorSummary> {
    engine
        .sftp_mkdir(SessionId::from_raw(session), &path)
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sftp_rename(
    engine: State<'_, Engine>,
    session: u64,
    from: String,
    to: String,
) -> Result<(), ErrorSummary> {
    engine
        .sftp_rename(SessionId::from_raw(session), &from, &to)
        .await
        .map_err(summary)
}

#[tauri::command]
pub(crate) async fn sftp_remove(
    engine: State<'_, Engine>,
    session: u64,
    path: String,
) -> Result<(), ErrorSummary> {
    engine
        .sftp_remove(SessionId::from_raw(session), &path)
        .await
        .map_err(summary)
}

// ── Transfers ─────────────────────────────────────────────────────────────

/// Opens the native file picker; the chosen files come back as tokens.
#[tauri::command]
pub(crate) async fn pick_uploads<R: Runtime>(
    app: AppHandle<R>,
    files: State<'_, Files>,
) -> Result<Vec<UploadDto>, ErrorSummary> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Upload files")
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });
    let picked = rx.await.ok().flatten().unwrap_or_default();
    Ok(files.register(picked.into_iter().filter_map(|p| p.into_path().ok())))
}

#[tauri::command]
pub(crate) async fn upload(
    engine: State<'_, Engine>,
    files: State<'_, Files>,
    session: u64,
    token: u64,
    remote_dir: String,
    on_conflict: OnConflict,
) -> Result<u64, ErrorSummary> {
    let local = files.upload_path(token).ok_or_else(|| ErrorSummary {
        code: "unknown_upload".into(),
        message: "this file is no longer available".into(),
        hint: Some("Choose it again.".into()),
    })?;
    let name = local
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let request = TransferRequest {
        direction: Direction::Upload,
        remote: join_remote(&remote_dir, &name),
        local,
        on_conflict,
    };
    let id = engine
        .transfer(SessionId::from_raw(session), request)
        .await
        .map_err(summary)?;
    Ok(id.get())
}

/// Downloads `remote_path` into the download folder, under its own name.
#[tauri::command]
pub(crate) async fn download(
    engine: State<'_, Engine>,
    files: State<'_, Files>,
    session: u64,
    remote_path: String,
    on_conflict: OnConflict,
) -> Result<u64, ErrorSummary> {
    let local = local_destination(&files.download_dir(), remote_name(&remote_path))
        .map_err(|e| summary(e.into()))?;
    let request = TransferRequest {
        direction: Direction::Download,
        local,
        remote: remote_path,
        on_conflict,
    };
    let id = engine
        .transfer(SessionId::from_raw(session), request)
        .await
        .map_err(summary)?;
    Ok(id.get())
}

#[tauri::command]
pub(crate) fn cancel_transfer(engine: State<'_, Engine>, id: u64) -> Result<(), ErrorSummary> {
    engine
        .cancel_transfer(TransferId::from_raw(id))
        .map_err(summary)
}

/// Where downloads go (shown in the UI).
#[tauri::command]
pub(crate) fn download_dir(files: State<'_, Files>) -> String {
    files.download_dir().display().to_string()
}

/// Native folder picker; `None` when the user cancelled.
#[tauri::command]
pub(crate) async fn choose_download_dir<R: Runtime>(
    app: AppHandle<R>,
    files: State<'_, Files>,
) -> Result<Option<String>, ErrorSummary> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Download to")
        .set_directory(files.download_dir())
        .pick_folder(move |picked| {
            let _ = tx.send(picked);
        });
    let Some(dir) = rx.await.ok().flatten().and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let shown = dir.display().to_string();
    *files
        .download_dir
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(dir);
    Ok(Some(shown))
}

// ── Tunnels ───────────────────────────────────────────────────────────────

fn parse_forward(spec: &str) -> Result<ForwardSpec, ErrorSummary> {
    ForwardSpec::parse_cli(spec).map_err(|e| ErrorSummary {
        code: "invalid_forward".into(),
        message: e.to_string(),
        hint: Some("For example -L 8080:db.internal:5432 or -R 9000:localhost:3000.".into()),
    })
}

/// Validates what the user typed; tells whether it needs a confirmation.
#[tauri::command]
pub(crate) fn check_forward(spec: String) -> Result<ForwardDto, ErrorSummary> {
    let spec = parse_forward(&spec)?;
    Ok(ForwardDto {
        display: spec.to_string(),
        exposed: spec.is_exposed(),
    })
}

#[tauri::command]
pub(crate) fn list_tunnels(engine: State<'_, Engine>) -> Vec<TunnelDto> {
    engine
        .tunnels()
        .into_iter()
        .map(|t| TunnelDto {
            id: t.id.get(),
            session: t.session.get(),
            display: t.spec.to_string(),
            exposed: t.spec.is_exposed(),
        })
        .collect()
}

/// `allow_exposed` is sent only after the user confirmed an exposed bind.
#[tauri::command]
pub(crate) async fn start_tunnel(
    engine: State<'_, Engine>,
    session: u64,
    spec: String,
    allow_exposed: bool,
) -> Result<u64, ErrorSummary> {
    let spec = parse_forward(&spec)?;
    let id = engine
        .start_tunnel(SessionId::from_raw(session), spec, allow_exposed)
        .await
        .map_err(summary)?;
    Ok(id.get())
}

#[tauri::command]
pub(crate) fn stop_tunnel(engine: State<'_, Engine>, id: u64) -> Result<(), ErrorSummary> {
    engine.stop_tunnel(TunnelId::from_raw(id)).map_err(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_registered_regular_files_get_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"hello").unwrap();
        let files = Files::default();
        let got = files.register([
            file.clone(),
            dir.path().to_path_buf(),
            dir.path().join("missing"),
        ]);
        assert_eq!(got.len(), 1);
        let token = got.first().unwrap().token;
        assert_eq!(files.upload_path(token), Some(file));
        assert_eq!(files.upload_path(token + 1), None);
        files.clear();
        assert_eq!(files.upload_path(token), None);
    }

    #[test]
    fn names_are_made_safe_for_display() {
        let dto = EntryDto::from(RemoteEntry {
            name: "evil\u{202e}txt.exe".into(),
            path: "/tmp/evil".into(),
            kind: EntryKind::File,
            size: 1,
            mode: None,
            modified: None,
        });
        assert!(!dto.display.contains('\u{202e}'));
        assert_eq!(dto.name, "evil\u{202e}txt.exe");
    }

    #[test]
    fn forwards_are_checked_before_starting() {
        assert!(
            check_forward("-L 0.0.0.0:8080:db:5432".into())
                .unwrap()
                .exposed
        );
        assert!(!check_forward("-L 8080:db:5432".into()).unwrap().exposed);
        assert_eq!(
            check_forward("8080".into()).unwrap_err().code,
            "invalid_forward"
        );
    }
}
