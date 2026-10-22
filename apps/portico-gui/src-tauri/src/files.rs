//! SFTP and tunnel commands.
//!
//! The webview never names a local path. Files to upload come from the
//! native picker or a drop on the window, both handled here in Rust, and the
//! webview only gets opaque tokens for them. Downloads go to one folder kept
//! here (the user's Downloads folder, or one chosen with the native picker),
//! and the file name is built by `portico_core::sftp::local_destination`.
//! A compromised page can therefore not read or write arbitrary local files.
//!
//! Tokens are bound: the first upload ties a token to its session and remote
//! destination (a retry after a conflict question reuses it there only), a
//! final answer (overwrite, skip, keep both) consumes it, and unused tokens
//! expire. Downloads never overwrite a hidden file (`.bashrc`…), and the
//! default folder is never the home folder itself.
//!
//! Tauri itself tells the page the paths of files dropped on the window
//! (`tauri://drag-drop`); that leaks names only, because no command takes a
//! path and the asset protocol and the fs plugin stay disabled (ADR in
//! `docs/STACK.md`, checked by a test).

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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

/// How long a picked or dropped file stays available without being used.
const UPLOAD_TTL: Duration = Duration::from_secs(10 * 60);

/// A local file the user handed to Portico.
#[derive(Debug)]
struct Upload {
    path: PathBuf,
    registered: Instant,
    /// Session and remote path of the first upload: a token is never sent
    /// anywhere else.
    bound: Option<(u64, String)>,
}

/// Local files the user handed to Portico, by token.
#[derive(Debug, Default)]
pub(crate) struct Files {
    uploads: Mutex<HashMap<u64, Upload>>,
    next: AtomicU64,
    download_dir: Mutex<Option<PathBuf>>,
}

impl Files {
    /// Registers files picked or dropped by the user (folders are skipped:
    /// transfers are not recursive yet).
    pub(crate) fn register(&self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<UploadDto> {
        let mut uploads = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        uploads.retain(|_, u| u.registered.elapsed() < UPLOAD_TTL);
        paths
            .into_iter()
            .filter_map(|path| {
                let meta = std::fs::metadata(&path)
                    .ok()
                    .filter(std::fs::Metadata::is_file)?;
                let name = path.file_name()?.to_str()?.to_owned();
                let token = self.next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
                uploads.insert(
                    token,
                    Upload {
                        path,
                        registered: Instant::now(),
                        bound: None,
                    },
                );
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

    /// The file behind `token`, for an upload of it to `remote` on
    /// `session`. `None` when the token is unknown, expired, or bound to
    /// another destination. Only `Ask` keeps the token (for the retry that
    /// answers the conflict question).
    fn take_upload(
        &self,
        token: u64,
        session: u64,
        remote: &str,
        on_conflict: OnConflict,
    ) -> Option<PathBuf> {
        let mut uploads = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
        uploads.retain(|_, u| u.registered.elapsed() < UPLOAD_TTL);
        let upload = uploads.get_mut(&token)?;
        match &upload.bound {
            Some((s, r)) if *s != session || r != remote => return None,
            Some(_) => {}
            None => upload.bound = Some((session, remote.to_owned())),
        }
        if on_conflict == OnConflict::Ask {
            Some(upload.path.clone())
        } else {
            uploads.remove(&token).map(|u| u.path)
        }
    }

    fn file_name(&self, token: u64) -> Option<String> {
        self.uploads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&token)
            .and_then(|u| u.path.file_name()?.to_str().map(str::to_owned))
    }

    fn download_dir(&self) -> PathBuf {
        self.download_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(default_download_dir)
    }
}

/// The XDG / macOS Downloads folder, else `~/Downloads`. Never the home
/// folder itself: a download named `.bashrc` must not land next to the real
/// one. The folder is created (`0700`) on first download if missing.
fn default_download_dir() -> PathBuf {
    let dirs = directories::UserDirs::new();
    if let Some(dir) = dirs.as_ref().and_then(directories::UserDirs::download_dir) {
        return dir.to_path_buf();
    }
    downloads_under(
        &dirs
            .map(|d| d.home_dir().to_path_buf())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(".")),
    )
}

fn downloads_under(home: &std::path::Path) -> PathBuf {
    home.join("Downloads")
}

/// Creates the download folder when it does not exist yet.
fn ensure_dir(dir: &std::path::Path) -> Result<(), ErrorSummary> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir).map_err(|e| ErrorSummary {
        code: "download_dir".into(),
        message: format!("cannot create {}: {e}", dir.display()),
        hint: Some("Choose another download folder.".into()),
    })
}

/// A hidden file (`.bashrc`, `.profile`…) is never replaced by a download:
/// the server chose its content, and a shell or a tool would run it.
fn check_overwrite(name: &str, on_conflict: OnConflict) -> Result<(), ErrorSummary> {
    if on_conflict == OnConflict::Overwrite && name.starts_with('.') {
        return Err(ErrorSummary {
            code: "hidden_overwrite".into(),
            message: format!(
                "{} is a hidden file and is not replaced by a download",
                display_safe(name, DISPLAY_NAME)
            ),
            hint: Some("Keep both, then compare the two files.".into()),
        });
    }
    Ok(())
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

fn unknown_upload() -> ErrorSummary {
    ErrorSummary {
        code: "unknown_upload".into(),
        message: "this file is no longer available".into(),
        hint: Some("Choose it again.".into()),
    }
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
    let name = files.file_name(token).ok_or_else(unknown_upload)?;
    let remote = join_remote(&remote_dir, &name);
    let local = files
        .take_upload(token, session, &remote, on_conflict)
        .ok_or_else(unknown_upload)?;
    let request = TransferRequest {
        direction: Direction::Upload,
        remote,
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
    let name = remote_name(&remote_path);
    check_overwrite(name, on_conflict)?;
    let dir = files.download_dir();
    let local = local_destination(&dir, name).map_err(|e| summary(e.into()))?;
    ensure_dir(&dir)?;
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
        hint: Some(
            "For example -L 8080:db.internal:5432, -R 9000:localhost:3000 or -D 1080 (SOCKS)."
                .into(),
        ),
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
        assert_eq!(
            files.take_upload(token, 1, "/srv/notes.txt", OnConflict::Ask),
            Some(file)
        );
        assert_eq!(
            files.take_upload(token + 1, 1, "/srv/notes.txt", OnConflict::Ask),
            None
        );
        files.clear();
        assert_eq!(
            files.take_upload(token, 1, "/srv/notes.txt", OnConflict::Ask),
            None
        );
    }

    #[test]
    fn upload_tokens_are_bound_then_consumed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("id_ed25519");
        std::fs::write(&file, b"key").unwrap();
        let files = Files::default();
        let token = files.register([file.clone()]).first().unwrap().token;
        // First try: bound to session 1, /srv/id_ed25519; kept for the retry.
        assert!(
            files
                .take_upload(token, 1, "/srv/id_ed25519", OnConflict::Ask)
                .is_some()
        );
        // Never sent to another host or another folder.
        assert_eq!(
            files.take_upload(token, 2, "/srv/id_ed25519", OnConflict::Ask),
            None
        );
        assert_eq!(
            files.take_upload(token, 1, "/tmp/id_ed25519", OnConflict::Rename),
            None
        );
        // The answer to the conflict question consumes it.
        assert_eq!(
            files.take_upload(token, 1, "/srv/id_ed25519", OnConflict::Rename),
            Some(file)
        );
        assert_eq!(
            files.take_upload(token, 1, "/srv/id_ed25519", OnConflict::Ask),
            None
        );
    }

    #[test]
    fn expired_upload_tokens_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        std::fs::write(&file, b"a").unwrap();
        let files = Files::default();
        let token = files.register([file]).first().unwrap().token;
        let Some(past) = Instant::now().checked_sub(UPLOAD_TTL) else {
            return; // Monotonic clock too young to go back that far.
        };
        files
            .uploads
            .lock()
            .unwrap()
            .get_mut(&token)
            .unwrap()
            .registered = past;
        assert_eq!(files.take_upload(token, 1, "/a", OnConflict::Ask), None);
    }

    #[test]
    fn default_download_dir_is_never_home() {
        let home = std::path::Path::new("/home/someone");
        assert_ne!(downloads_under(home), home);
        assert_ne!(default_download_dir(), PathBuf::from("/"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_ne!(default_download_dir(), PathBuf::from(home));
        }
    }

    #[test]
    fn download_folder_is_created_private() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = downloads_under(dir.path());
        ensure_dir(&downloads).unwrap();
        assert!(downloads.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&downloads).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn hidden_files_are_never_overwritten_by_a_download() {
        assert_eq!(
            check_overwrite(".bashrc", OnConflict::Overwrite)
                .unwrap_err()
                .code,
            "hidden_overwrite"
        );
        assert!(check_overwrite(".bashrc", OnConflict::Rename).is_ok());
        assert!(check_overwrite("notes.txt", OnConflict::Overwrite).is_ok());
    }

    #[test]
    fn tauri_never_gets_the_asset_protocol_or_the_fs_plugin() {
        // Tauri tells the page the paths of dropped files and adds them to the
        // asset scope: harmless only while nothing can read through it.
        for manifest in [
            include_str!("../Cargo.toml"),
            include_str!("../../../../Cargo.toml"),
        ] {
            assert!(!manifest.contains("protocol-asset"));
            assert!(!manifest.contains("tauri-plugin-fs"));
        }
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
