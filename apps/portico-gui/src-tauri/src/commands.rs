//! IPC commands. Errors cross the boundary as `ErrorSummary` (code, message,
//! hint) so the UI can render them without string matching.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use bytes::Bytes;
use portico_core::engine::ConnectRequest;
use portico_core::ids::{PromptId, SessionId};
use portico_core::search::HostSearch;
use portico_core::{Engine, ErrorSummary};
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::dto::{HostDto, PromptAnswerDto, PtySizeDto, ThemeDto};
use crate::terminals::Terminals;

/// Largest input accepted in one call (the view splits pastes into 64 KiB
/// chunks); with the engine's bounded queue this caps buffered input.
const MAX_INPUT: usize = 64 * 1024;

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

/// Home directory, so the view can show `~/…` paths.
#[tauri::command]
pub(crate) fn home_dir() -> Option<String> {
    std::env::var("HOME").ok()
}

#[tauri::command]
pub(crate) fn list_themes() -> Vec<ThemeDto> {
    portico_theme::THEMES
        .iter()
        .map(|t| ThemeDto {
            id: t.id,
            name: t.name,
            description: t.description,
            appearance: t.appearance,
        })
        .collect()
}

/// Starts a session; progress arrives on `engine://event`.
///
/// `async` on purpose: the engine spawns the session task, which needs the
/// Tokio runtime that Tauri runs async commands on (sync commands run on the
/// main thread, outside any runtime).
#[tauri::command]
pub(crate) async fn connect(engine: State<'_, Engine>, alias: String) -> Result<u64, ErrorSummary> {
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

/// Opens a PTY on `session` (waits for it to be connected). Output is streamed
/// as raw bytes on `output`; the view acknowledges them with `terminal_ack`.
#[tauri::command]
pub(crate) async fn open_terminal(
    engine: State<'_, Engine>,
    terminals: State<'_, Arc<Terminals>>,
    session: u64,
    size: PtySizeDto,
    output: Channel<InvokeResponseBody>,
) -> Result<u64, ErrorSummary> {
    let handle = engine
        .open_terminal(SessionId::from_raw(session), size.into())
        .await
        .map_err(|e| e.summary())?;
    Ok(terminals.attach(handle, output))
}

/// Keystrokes or pasted text. `binary` input (xterm.js `onBinary`, e.g. mouse
/// reports) carries one byte per UTF-16 code unit.
#[tauri::command]
pub(crate) async fn terminal_input(
    terminals: State<'_, Arc<Terminals>>,
    id: u64,
    data: String,
    binary: bool,
) -> Result<(), ErrorSummary> {
    let inputs = terminals.inputs(id).ok_or_else(terminal_gone)?;
    let bytes = if binary {
        // Code units above 0xFF are not valid binary input: drop them.
        data.chars()
            .filter_map(|c| u8::try_from(u32::from(c)).ok())
            .collect::<Vec<u8>>()
    } else {
        data.into_bytes()
    };
    if bytes.len() > MAX_INPUT {
        return Err(ErrorSummary {
            code: "input_too_large".into(),
            message: format!("input of {} bytes is too large", bytes.len()),
            hint: None,
        });
    }
    inputs
        .send(Bytes::from(bytes))
        .await
        .map_err(|_| terminal_gone())
}

#[tauri::command]
pub(crate) fn terminal_resize(
    terminals: State<'_, Arc<Terminals>>,
    id: u64,
    size: PtySizeDto,
) -> Result<(), ErrorSummary> {
    let inputs = terminals.inputs(id).ok_or_else(terminal_gone)?;
    inputs.resize(size.into());
    Ok(())
}

/// The view finished parsing `total` bytes of output (flow control).
#[tauri::command]
pub(crate) fn terminal_ack(terminals: State<'_, Arc<Terminals>>, id: u64, total: u64) {
    terminals.ack(id, total);
}

/// Closes the PTY channel (EOF + CLOSE); the session stays connected.
#[tauri::command]
pub(crate) fn close_terminal(terminals: State<'_, Arc<Terminals>>, id: u64) {
    terminals.close(id);
}

fn terminal_gone() -> ErrorSummary {
    ErrorSummary {
        code: "terminal_closed".into(),
        message: "this terminal is closed".into(),
        hint: None,
    }
}
