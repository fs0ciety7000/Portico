//! Tauri shell around `portico-core`.
//!
//! The Rust side stays thin: commands translate IPC calls into `Engine`
//! calls, and a forwarder task re-emits every `EngineEvent` to the webview as
//! the `engine://event` event. PTY bytes use a `tauri::ipc::Channel` per
//! terminal (ordered, binary, flow-controlled), never the global event bus.

mod commands;
mod dto;
mod terminals;

use std::sync::Arc;

use portico_core::{Engine, EngineConfig};
use tauri::{Emitter, Manager, RunEvent};

use crate::terminals::Terminals;

/// Event name carrying serialized `EngineEvent`s to the frontend.
pub const ENGINE_EVENT: &str = "engine://event";

#[allow(clippy::missing_panics_doc)] // Tauri's builder reports fatal startup errors itself
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PORTICO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let app = tauri::Builder::default()
        .setup(|app| {
            let engine = Engine::new(EngineConfig::discover()?);
            let mut events = engine.subscribe();
            let handle = app.handle().clone();
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
            app.manage(engine);
            app.manage(Arc::new(Terminals::default()));
            Ok(())
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
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "Portico failed to start");
            std::process::exit(1);
        }
    };
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Close channels and sessions cleanly (EOF, CLOSE, DISCONNECT).
            if let Some(terminals) = handle.try_state::<Arc<Terminals>>() {
                terminals.close_all();
            }
            if let Some(engine) = handle.try_state::<Engine>() {
                engine.shutdown();
            }
        }
    });
}
