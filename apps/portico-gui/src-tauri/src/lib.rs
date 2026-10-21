//! Tauri shell around `portico-core`.
//!
//! The Rust side stays thin: commands translate IPC calls into `Engine`
//! calls, and a forwarder task re-emits every `EngineEvent` to the webview as
//! the `engine://event` event. PTY bytes use a `tauri::ipc::Channel` per
//! terminal (ordered, binary, flow-controlled), never the global event bus.

mod commands;
mod dto;
mod files;
mod keys;
mod snippets;
mod terminals;

use std::sync::Arc;
use std::time::Duration;

use portico_core::{Engine, EngineConfig};
use tauri::webview::PageLoadEvent;
use tauri::{DragDropEvent, Emitter, Manager, RunEvent, WindowEvent};

use crate::files::Files;
use crate::terminals::Terminals;

/// Event name carrying serialized `EngineEvent`s to the frontend.
pub const ENGINE_EVENT: &str = "engine://event";
/// Files dropped on the window, as upload tokens (`files::UploadDto`).
pub const DROPPED_EVENT: &str = "portico://dropped";
/// A drag with files entered (`true`) or left (`false`) the window.
pub const DRAGGING_EVENT: &str = "portico://dragging";

/// Longest wait for sessions to close when quitting.
const EXIT_GRACE: Duration = Duration::from_millis(500);

#[allow(clippy::missing_panics_doc)] // Tauri's builder reports fatal startup errors itself
pub fn run() {
    // First, before any secret can be in memory: no core dumps, no ptrace
    // (logged once tracing is up).
    let hardening = portico_core::hardening::harden_process();
    init_tracing();
    hardening.log();
    run_app();
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PORTICO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
}

fn run_app() {
    let app = tauri::Builder::default()
        // Rust-side dialogs only: no `dialog:` permission is granted to the
        // webview (see capabilities/default.json).
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let engine = Engine::new(EngineConfig::discover()?);
            forward_engine_events(&engine, app.handle().clone());
            app.manage(engine);
            app.manage(Arc::new(Terminals::default()));
            app.manage(Files::default());
            Ok(())
        })
        // Dropped files become upload tokens; their paths stay in Rust.
        .on_window_event(forward_drag_drop)
        // A (re)loading page has lost every Channel and dialog: close what
        // the previous page opened instead of leaving shells and prompts
        // with no UI.
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started {
                release_page(webview);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_hosts,
            commands::search_hosts,
            commands::list_themes,
            commands::home_dir,
            commands::connect,
            commands::disconnect,
            commands::respond_prompt,
            commands::open_terminal,
            commands::terminal_input,
            commands::terminal_resize,
            commands::terminal_ack,
            commands::close_terminal,
            files::sftp_home,
            files::sftp_list,
            files::sftp_mkdir,
            files::sftp_rename,
            files::sftp_remove,
            files::pick_uploads,
            files::upload,
            files::download,
            files::cancel_transfer,
            files::download_dir,
            files::choose_download_dir,
            files::check_forward,
            files::list_tunnels,
            files::start_tunnel,
            files::stop_tunnel,
            keys::list_keys,
            keys::key_defaults,
            keys::generate_key,
            keys::public_key_text,
            keys::list_agent_keys,
            keys::agent_add,
            keys::agent_remove,
            keys::copy_id,
            keys::verify_key_login,
            keys::vault_status,
            keys::vault_create,
            keys::vault_unlock,
            keys::vault_lock,
            keys::vault_change_passphrase,
            snippets::list_snippets,
            snippets::snippets_for,
            snippets::save_snippet,
            snippets::delete_snippet,
            snippets::run_snippet,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "Portico failed to start");
            std::process::exit(1);
        }
    };
    let mut closing = false;
    app.run(move |handle, event| {
        if let RunEvent::ExitRequested { api, .. } = event {
            if closing {
                return;
            }
            // Hold the exit until sessions have closed cleanly (channel
            // EOF/CLOSE, SSH DISCONNECT), half a second at most.
            closing = true;
            api.prevent_exit();
            if let Some(terminals) = handle.try_state::<Arc<Terminals>>() {
                terminals.close_all();
            }
            let engine = handle.try_state::<Engine>().map(|e| e.inner().clone());
            let handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(engine) = engine {
                    engine.shutdown_and_wait(EXIT_GRACE).await;
                }
                handle.exit(0);
            });
        }
    });
}

/// Closes what a (re)loading page opened: terminals, upload tokens, sessions.
fn release_page(webview: &tauri::Webview) {
    if let Some(terminals) = webview.try_state::<Arc<Terminals>>() {
        terminals.close_all();
    }
    if let Some(files) = webview.try_state::<Files>() {
        files.clear();
    }
    if let Some(engine) = webview.try_state::<Engine>() {
        engine.disconnect_all();
    }
}

/// Dropped files become upload tokens. Tauri also tells the page their paths
/// (`tauri://drag-drop`): names leak, but no command takes a path (see `files`).
fn forward_drag_drop(window: &tauri::Window, event: &WindowEvent) {
    let WindowEvent::DragDrop(drag) = event else {
        return;
    };
    let emitted = match drag {
        DragDropEvent::Enter { .. } => window.emit(DRAGGING_EVENT, true),
        DragDropEvent::Leave => window.emit(DRAGGING_EVENT, false),
        DragDropEvent::Drop { paths, .. } => {
            let _ = window.emit(DRAGGING_EVENT, false);
            match window.try_state::<Files>() {
                Some(files) => window.emit(DROPPED_EVENT, files.register(paths.clone())),
                None => Ok(()),
            }
        }
        _ => Ok(()),
    };
    if let Err(e) = emitted {
        tracing::warn!(error = %e, "failed to forward a drag and drop event");
    }
}

/// Re-emits every engine event to the webview as [`ENGINE_EVENT`].
fn forward_engine_events(engine: &Engine, handle: tauri::AppHandle) {
    let mut events = engine.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Err(e) = handle.emit(ENGINE_EVENT, &event) {
                        tracing::warn!(error = %e, "failed to forward engine event");
                    }
                }
                // A burst overflowed the channel: keep forwarding.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "engine events lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
