//! Tauri shell around `portico-core`.
//!
//! The Rust side stays thin: commands translate IPC calls into `Engine`
//! calls, and a forwarder task re-emits every `EngineEvent` to the webview as
//! the `engine://event` event. PTY bytes will use `tauri::ipc::Channel`
//! (ordered, binary) per terminal, never the global event bus.

mod commands;
mod dto;

use portico_core::{Engine, EngineConfig};
use tauri::{Emitter, Manager};

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

    let result = tauri::Builder::default()
        .setup(|app| {
            let engine = Engine::new(EngineConfig::discover()?);
            let mut events = engine.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = events.recv().await {
                    if let Err(e) = handle.emit(ENGINE_EVENT, &event) {
                        tracing::warn!(error = %e, "failed to forward engine event");
                    }
                }
            });
            app.manage(engine);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_hosts,
            commands::search_hosts,
            commands::list_themes,
            commands::connect,
            commands::disconnect,
            commands::respond_prompt,
        ])
        .run(tauri::generate_context!());

    if let Err(e) = result {
        tracing::error!(error = %e, "Portico failed to start");
        std::process::exit(1);
    }
}
