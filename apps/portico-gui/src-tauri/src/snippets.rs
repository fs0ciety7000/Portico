//! Snippet commands (phase 3).
//!
//! Rendering and validation happen here, in Rust, with `portico-core`: the
//! webview sends a snippet id and the placeholder values, never the bytes to
//! type. `run_snippet` only reaches a terminal the view opened, and only with
//! a snippet whose scope covers that terminal's host.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use portico_core::ids::SessionId;
use portico_core::snippets::{Snippet, SnippetScope};
use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::State;

use crate::terminals::Terminals;

/// Largest input sent in one piece (same cap as `terminal_input`).
const INPUT_CHUNK: usize = 64 * 1024;

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

/// A snippet with its placeholders (in order of appearance).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnippetDto {
    pub id: String,
    pub name: String,
    pub command: String,
    pub description: Option<String>,
    pub scope: SnippetScope,
    pub run: bool,
    pub placeholders: Vec<String>,
}

impl From<&Snippet> for SnippetDto {
    fn from(s: &Snippet) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            command: s.command.clone(),
            description: s.description.clone(),
            scope: s.scope.clone(),
            run: s.run,
            placeholders: s.placeholders().into_iter().map(str::to_owned).collect(),
        }
    }
}

#[tauri::command]
pub(crate) async fn list_snippets(
    engine: State<'_, Engine>,
) -> Result<Vec<SnippetDto>, ErrorSummary> {
    let all = engine.snippets().await.map_err(summary)?;
    Ok(all.iter().map(SnippetDto::from).collect())
}

/// Snippets whose scope covers `alias` (scope rules live in the core).
#[tauri::command]
pub(crate) async fn snippets_for(
    engine: State<'_, Engine>,
    alias: String,
) -> Result<Vec<SnippetDto>, ErrorSummary> {
    let found = engine.snippets_for(&alias).await.map_err(summary)?;
    Ok(found.iter().map(SnippetDto::from).collect())
}

/// Creates (empty id) or updates a snippet; returns it as stored.
#[tauri::command]
pub(crate) async fn save_snippet(
    engine: State<'_, Engine>,
    snippet: Snippet,
) -> Result<SnippetDto, ErrorSummary> {
    let saved = engine.save_snippet(snippet).await.map_err(summary)?;
    Ok(SnippetDto::from(&saved))
}

#[tauri::command]
pub(crate) async fn delete_snippet(
    engine: State<'_, Engine>,
    id: String,
) -> Result<(), ErrorSummary> {
    engine.delete_snippet(&id).await.map_err(summary)
}

/// Renders snippet `id` with `values` and types it into `terminal`.
/// `bracketed_paste` is the terminal's current mode (xterm.js tracks it);
/// `run` overrides the snippet's own flag when set ("Run it" after the core
/// refused to paste a multi-line command without bracketed paste).
#[tauri::command]
pub(crate) async fn run_snippet(
    engine: State<'_, Engine>,
    terminals: State<'_, Arc<Terminals>>,
    terminal: u64,
    id: String,
    values: HashMap<String, String>,
    bracketed_paste: bool,
    run: Option<bool>,
) -> Result<(), ErrorSummary> {
    let (inputs, session) = terminals.entry(terminal).ok_or_else(terminal_gone)?;
    let alias = engine
        .session_alias(SessionId::from_raw(session))
        .ok_or_else(terminal_gone)?;
    let snippet = engine
        .snippets_for(&alias)
        .await
        .map_err(summary)?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| not_here(&alias))?;
    let pairs: Vec<(&str, &str)> = values
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let rendered = snippet.render_checked(&pairs).map_err(summary)?;
    let bytes = portico_core::snippets::snippet_input(
        &rendered,
        run.unwrap_or(snippet.run),
        bracketed_paste,
    )
    .map_err(summary)?;
    for part in bytes.chunks(INPUT_CHUNK) {
        inputs
            .send(Bytes::copy_from_slice(part))
            .await
            .map_err(|_| terminal_gone())?;
    }
    Ok(())
}

fn not_here(alias: &str) -> ErrorSummary {
    ErrorSummary {
        code: "snippet_not_found".into(),
        message: format!("this snippet is not available on {alias}"),
        hint: Some(
            "Reload the snippets: it may have been deleted or scoped to other hosts.".into(),
        ),
    }
}

fn terminal_gone() -> ErrorSummary {
    ErrorSummary {
        code: "terminal_closed".into(),
        message: "this terminal is closed".into(),
        hint: Some("Run the snippet in an open terminal tab.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_are_sent_with_their_placeholders() {
        let snippet: Snippet = serde_json::from_str(
            r#"{"id":"tail","name":"Tail","command":"tail -n {{lines}} {{file}} | grep {{file}}","scope":{"kind":"hosts","values":["web-01"]},"run":true}"#,
        )
        .unwrap();
        let dto = SnippetDto::from(&snippet);
        assert_eq!(
            dto.placeholders,
            vec!["lines".to_owned(), "file".to_owned()]
        );
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["scope"]["kind"], "hosts");
        assert_eq!(json["scope"]["values"][0], "web-01");
    }

    #[test]
    fn new_snippets_come_without_an_id() {
        let snippet: Snippet =
            serde_json::from_str(r#"{"name":"Disk","command":"df -h","description":null,"scope":{"kind":"global"},"run":true}"#)
                .unwrap();
        assert!(snippet.id.is_empty());
        assert_eq!(snippet.scope, SnippetScope::Global);
    }
}
