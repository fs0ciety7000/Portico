//! IPC commands. Errors cross the boundary as `ErrorSummary` (code, message,
//! hint) so the UI can render them without string matching.

use portico_core::engine::ConnectRequest;
use portico_core::search::HostSearch;
use portico_core::{Engine, ErrorSummary};
use tauri::State;

use crate::dto::{HostDto, ThemeDto};

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

#[tauri::command]
pub(crate) async fn connect(engine: State<'_, Engine>, alias: String) -> Result<u64, ErrorSummary> {
    let request = ConnectRequest {
        alias,
        user_override: None,
        pty: None,
    };
    let (session, _terminal) = engine.connect(request).await.map_err(|e| e.summary())?;
    Ok(session.get())
}
