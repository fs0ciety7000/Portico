//! IPC commands. Errors cross the boundary as `ErrorSummary` (code, message,
//! hint) so the UI can render them without string matching.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use portico_core::engine::ConnectRequest;
use portico_core::ids::{PromptId, SessionId};
use portico_core::search::HostSearch;
use portico_core::{Engine, ErrorSummary};
use tauri::State;

use crate::dto::{HostDto, PromptAnswerDto, ThemeDto};

#[tauri::command]
pub(crate) async fn list_hosts(engine: State<'_, Engine>) -> Result<Vec<HostDto>, ErrorSummary> {
    let inv = engine.load_hosts().await.map_err(|e| e.summary())?;
    Ok(inv.hosts.iter().map(HostDto::from).collect())
}

/// Same ranking as the TUI: the matcher lives in `portico-core`.
#[tauri::command]
pub(crate) async fn search_hosts(
    engine: State<'_, Engine>,
    query: String,
) -> Result<Vec<HostDto>, ErrorSummary> {
    let inv = engine.load_hosts().await.map_err(|e| e.summary())?;
    let hits = HostSearch::default().search(&inv.hosts, &query);
    Ok(hits
        .iter()
        .filter_map(|h| inv.hosts.get(h.index))
        .map(HostDto::from)
        .collect())
}

#[tauri::command]
pub(crate) fn list_themes() -> Vec<ThemeDto> {
    portico_theme::THEMES
        .iter()
        .map(|t| ThemeDto {
            id: t.id,
            name: t.name,
            description: t.description,
        })
        .collect()
}

/// Starts a session; progress arrives on `engine://event`.
#[tauri::command]
pub(crate) fn connect(engine: State<'_, Engine>, alias: String) -> Result<u64, ErrorSummary> {
    let id = engine
        .connect(ConnectRequest::new(alias))
        .map_err(|e| e.summary())?;
    Ok(id.get())
}

#[tauri::command]
pub(crate) async fn disconnect(
    engine: State<'_, Engine>,
    session: u64,
) -> Result<(), ErrorSummary> {
    let id = SessionId::from_raw(session);
    engine.disconnect(id).await.map_err(|e| e.summary())
}

/// Answers an engine prompt. Secrets are wrapped immediately and never logged.
#[tauri::command]
pub(crate) fn respond_prompt(
    engine: State<'_, Engine>,
    prompt: u64,
    answer: PromptAnswerDto,
) -> Result<(), ErrorSummary> {
    engine
        .respond(PromptId::from_raw(prompt), answer.into())
        .map_err(|e| e.summary())
}
