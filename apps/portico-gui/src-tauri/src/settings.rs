//! User settings (phase 4): theme and GUI keymap from `settings.toml`.
//!
//! The core reads and writes the file (bounded, validated, other keys and
//! comments kept); the webview never sees its path. Key syntax is the GUI's
//! business: `keymap.gui` is handed over as is and checked in TypeScript
//! (`src/lib/shortcuts.ts`), which ignores and reports invalid entries.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;

use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::State;

/// What the GUI uses from `settings.toml`. An unreadable or invalid file
/// gives the defaults and `error` (the view shows it once).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsDto {
    /// Theme id as written (the view falls back to the default for an
    /// unknown id, and says so).
    pub theme: Option<String>,
    pub keymap: BTreeMap<String, String>,
    pub error: Option<ErrorSummary>,
}

#[tauri::command]
pub(crate) async fn get_settings(engine: State<'_, Engine>) -> Result<SettingsDto, ErrorSummary> {
    Ok(match engine.settings().await {
        Ok(settings) => SettingsDto {
            theme: settings.theme,
            keymap: settings.keymap.gui,
            error: None,
        },
        Err(e) => SettingsDto {
            error: Some(e.summary()),
            ..SettingsDto::default()
        },
    })
}

/// Remembers the theme. Only known theme ids are written, and only over a
/// readable file: an invalid `settings.toml` is reported, never replaced
/// (its keymaps would be lost).
#[tauri::command]
pub(crate) async fn save_theme(
    engine: State<'_, Engine>,
    theme: String,
) -> Result<(), ErrorSummary> {
    check_theme(&theme)?;
    // Only the theme changes, under the core's lock: a keymap saved
    // meanwhile (TUI, another window) is kept.
    engine
        .set_theme(Some(theme))
        .await
        .map(|_| ())
        .map_err(|e| e.summary())
}

fn check_theme(theme: &str) -> Result<(), ErrorSummary> {
    if portico_theme::Theme::by_id(theme).is_some() {
        Ok(())
    } else {
        Err(ErrorSummary {
            code: "unknown_theme".into(),
            message: "this theme does not exist".into(),
            hint: Some("Pick one of the themes listed in the command palette.".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_themes_are_saved() {
        assert!(check_theme("obsidian").is_ok());
        assert!(check_theme("travertine").is_ok());
        assert_eq!(check_theme("solarized").unwrap_err().code, "unknown_theme");
    }
}
