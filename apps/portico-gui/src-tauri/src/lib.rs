//! Tauri shell around `portico-core`.
//!
//! The Rust side stays thin: commands translate IPC calls into `Engine`
//! calls, and a forwarder task re-emits every `EngineEvent` to the webview as
//! the `engine://event` event. PTY bytes use a `tauri::ipc::Channel` per
//! terminal (ordered, binary, flow-controlled), never the global event bus.

mod commands;
mod debug;
mod dto;
mod files;
mod hosts;
mod import;
mod keys;
mod links;
mod meta;
mod recordings;
mod settings;
mod snippets;
mod sync;
mod terminals;
mod updates;

use std::sync::Arc;
use std::time::Duration;

use portico_core::{Engine, EngineConfig};
use tauri::webview::PageLoadEvent;
use tauri::{DragDropEvent, Emitter, Manager, RunEvent, WindowEvent};

use crate::files::Files;
use crate::import::Imports;
use crate::recordings::Playbacks;
use crate::sync::SyncUi;
use crate::terminals::Terminals;

/// Event name carrying serialized `EngineEvent`s to the frontend.
pub const ENGINE_EVENT: &str = "engine://event";
/// Files dropped on the window, as upload tokens (`files::UploadDto`).
pub const DROPPED_EVENT: &str = "portico://dropped";
/// A drag with files entered (`true`) or left (`false`) the window.
pub const DRAGGING_EVENT: &str = "portico://dragging";
/// The engine event stream lagged (events were lost): the view resyncs
/// what it derives from events (recording state) with queries.
pub const LAGGED_EVENT: &str = "portico://engine-lagged";

/// Longest wait for sessions to close when quitting.
const EXIT_GRACE: Duration = Duration::from_millis(500);

#[allow(clippy::missing_panics_doc)] // Tauri's builder reports fatal startup errors itself
pub fn run() {
    #[cfg(target_os = "linux")]
    let restart = webkit_env::apply();
    // First, before any secret can be in memory: no core dumps, no ptrace
    // (logged once tracing is up).
    let hardening = portico_core::hardening::harden_process();
    init_tracing();
    hardening.log();
    #[cfg(target_os = "linux")]
    if let Some(err) = restart {
        tracing::warn!("could not restart with the WebKitGTK workarounds: {err}");
    }
    run_app();
}

/// `WebKitGTK` workarounds that must be in the environment before it
/// starts. `std::env::set_var` is unsafe in a threaded process (and unsafe
/// code is forbidden here), so the process re-executes itself once with the
/// variables set.
#[cfg(target_os = "linux")]
mod webkit_env {
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt;
    use std::path::Path;

    /// NVIDIA's proprietary driver and `WebKitGTK` disagree on Wayland explicit
    /// sync: the window closes at once (`Error 71 (Protocol error)
    /// dispatching to Wayland display`). Turning explicit sync off keeps the
    /// GPU path; turning `WebKit`'s DMA-BUF renderer off instead also avoids
    /// the crash but renders slowly enough to make typing lag.
    pub(crate) const NV_EXPLICIT_SYNC: &str = "__NV_DISABLE_EXPLICIT_SYNC";

    /// Present when the NVIDIA proprietary kernel driver is loaded.
    const NVIDIA_DRIVER: &str = "/proc/driver/nvidia/version";

    /// The variables to add: none that the user already set (any value
    /// wins, `0` included), and only for the driver that needs them.
    pub(crate) fn missing(
        nvidia: bool,
        is_set: impl Fn(&str) -> bool,
    ) -> Vec<(&'static str, &'static str)> {
        let mut vars = Vec::new();
        if nvidia && !is_set(NV_EXPLICIT_SYNC) {
            vars.push((NV_EXPLICIT_SYNC, "1"));
        }
        vars
    }

    /// Returns only when nothing had to change, or when the restart failed
    /// (then the app starts as is and the error is logged).
    pub(crate) fn apply() -> Option<std::io::Error> {
        let vars = missing(Path::new(NVIDIA_DRIVER).exists(), |name| {
            std::env::var_os(name).is_some()
        });
        if vars.is_empty() {
            return None;
        }
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(err) => return Some(err),
        };
        Some(
            std::process::Command::new(exe)
                .args(std::env::args_os().skip(1))
                .envs(vars.into_iter().map(|(k, v)| (k, OsString::from(v))))
                .exec(),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn nvidia_gets_explicit_sync_off_unless_the_user_chose() {
            assert_eq!(missing(true, |_| false), vec![(NV_EXPLICIT_SYNC, "1")]);
            assert!(missing(true, |name| name == NV_EXPLICIT_SYNC).is_empty());
        }

        #[test]
        fn other_drivers_keep_webkit_defaults() {
            assert!(missing(false, |_| false).is_empty());
        }
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PORTICO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
}

// One builder chain: its length is the list of IPC commands.
#[allow(clippy::too_many_lines)]
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
            app.manage(Imports::default());
            app.manage(Arc::new(Playbacks::default()));
            app.manage(SyncUi::default());
            // Before the page paints: a light system gets the light theme's
            // background (what `auto`, the default, shows), not a dark flash.
            // The view then sets the exact theme (`set_window_theme`).
            if let Some(window) = app.get_webview_window("main")
                && window.theme().is_ok_and(|t| t == tauri::Theme::Light)
            {
                let _ = window.set_background_color(Some(settings::background(
                    portico_theme::Theme::light_theme(),
                )));
            }
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
            hosts::create_host,
            hosts::update_host,
            hosts::delete_host,
            hosts::duplicate_host,
            hosts::managed_include,
            hosts::managed_host,
            meta::set_favorite,
            meta::set_tags,
            meta::check_tags,
            meta::set_group,
            meta::set_notes,
            meta::recent_hosts,
            meta::clear_recent,
            meta::forget_recent,
            debug::sessions_with_logs,
            debug::session_logs,
            debug::session_logs_text,
            debug::effective_config,
            settings::get_settings,
            settings::save_theme,
            settings::set_window_theme,
            settings::save_copy_on_select,
            links::open_url,
            updates::start_update_checks,
            updates::update_status,
            updates::set_update_check,
            updates::update_instruction,
            import::import_pick,
            import::import_replan,
            import::import_apply,
            import::import_cancel,
            recordings::recording_start,
            recordings::recording_stop,
            recordings::recording_status,
            recordings::recordings_folder,
            recordings::list_recordings,
            recordings::delete_recording,
            recordings::play_recording,
            recordings::playback_control,
            recordings::playback_ack,
            recordings::playback_close,
            sync::sync_start,
            sync::sync_status,
            sync::sync_now,
            sync::sync_devices,
            sync::sync_conflicts,
            sync::sync_issues,
            sync::sync_pending_hosts,
            sync::synced_snippets_unconfirmed,
            sync::sync_default_device_name,
            sync::sync_pick_folder,
            sync::sync_probe_again,
            sync::sync_generate_passphrase,
            sync::sync_forget_generated,
            sync::sync_assess_passphrase,
            sync::sync_create,
            sync::sync_join_preview,
            sync::sync_join_commit,
            sync::sync_unlock,
            sync::sync_resolve,
            sync::sync_skip_on_this_device,
            sync::sync_confirm_hosts,
            sync::sync_reject_hosts,
            sync::confirm_synced_snippet,
            sync::sync_acknowledge_alerts,
            sync::sync_change_passphrase,
            sync::sync_reset_passphrase,
            sync::sync_rename_device,
            sync::sync_set_options,
            sync::sync_disable,
            sync::sync_rotation_preview,
            sync::sync_forget_preview,
            sync::sync_rotate_keys,
            sync::sync_resolve_rotation_conflict,
            sync::sync_restore_bundle,
            sync::sync_newer_files,
            sync::sync_discard_newer_file,
            sync::sync_key_entries,
            sync::sync_remove_key_entry,
            sync::sync_revoked_values,
            sync::sync_revoked_value,
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
            if let Some(playbacks) = handle.try_state::<Arc<Playbacks>>() {
                playbacks.close_all();
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

/// Closes what a (re)loading page opened: terminals (and so their
/// recordings), playbacks, upload tokens, a pending import, the sync
/// wizard's folder and generated passphrase, sessions.
fn release_page(webview: &tauri::Webview) {
    if let Some(terminals) = webview.try_state::<Arc<Terminals>>() {
        terminals.close_all();
    }
    if let Some(playbacks) = webview.try_state::<Arc<Playbacks>>() {
        playbacks.close_all();
    }
    if let Some(files) = webview.try_state::<Files>() {
        files.clear();
    }
    if let Some(imports) = webview.try_state::<Imports>() {
        imports.clear();
    }
    if let Some(sync) = webview.try_state::<SyncUi>() {
        sync.clear();
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
                // A burst overflowed the channel: keep forwarding, and let
                // the view resync what it derives from events.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "engine events lagged");
                    if let Err(e) = handle.emit(LAGGED_EVENT, n) {
                        tracing::warn!(error = %e, "failed to forward the lag notice");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
