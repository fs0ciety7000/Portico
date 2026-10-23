//! Opt-in update check (0.3.0): thin commands over the engine.
//!
//! The engine makes the only request, and only once `[updates] check =
//! true` (see `portico_core::update`); nothing here reaches the network.
//! The status is the core's `UpdateStatus`, as the `update` engine event
//! carries it, so the view has one shape for both. The release page is
//! opened through `open_url` (validated again there, http/https only).

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use portico_core::update::{Frontend, UpdateInstruction, UpdateStatus};
use portico_core::{Engine, ErrorSummary};
use tauri::State;

/// At startup (and on a page reload): starts the daily check if the user
/// turned it on, and returns the status (`check: null` = ask once).
#[tauri::command]
pub(crate) async fn start_update_checks(
    engine: State<'_, Engine>,
) -> Result<UpdateStatus, ErrorSummary> {
    engine.start_update_checks().await.map_err(|e| e.summary())
}

/// The status again (after lagged engine events).
#[tauri::command]
pub(crate) async fn update_status(engine: State<'_, Engine>) -> Result<UpdateStatus, ErrorSummary> {
    engine.update_status().await.map_err(|e| e.summary())
}

/// "Check daily" / "Not now", or the Settings toggle.
#[tauri::command]
pub(crate) async fn set_update_check(
    engine: State<'_, Engine>,
    on: bool,
) -> Result<UpdateStatus, ErrorSummary> {
    engine.set_update_check(on).await.map_err(|e| e.summary())
}

/// How to update this install to the known newer release (its version
/// is taken from the engine, never from the view).
#[tauri::command]
pub(crate) async fn update_instruction(
    engine: State<'_, Engine>,
) -> Result<Option<UpdateInstruction>, ErrorSummary> {
    let status = engine.update_status().await.map_err(|e| e.summary())?;
    let Some(available) = status.available else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        portico_core::update::install_instruction(Frontend::Gui, &available.version)
    })
    .await
    .map(Some)
    .map_err(|_| ErrorSummary {
        code: "update_instruction".into(),
        message: "cannot tell how this copy was installed".into(),
        hint: Some("Download the new version from the release page.".into()),
    })
}
