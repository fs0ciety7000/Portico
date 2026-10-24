//! User settings (phase 4): theme and GUI keymap from `settings.toml`;
//! "Copy on select" (`[gui] copy_on_select`, phase 9); `auto` and the dark
//! theme it uses, and the window following the painted theme (phase 10).
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
    /// The dark theme `auto` uses, as written (checked by the view).
    pub dark_theme: Option<String>,
    pub keymap: BTreeMap<String, String>,
    pub copy_on_select: bool,
    pub error: Option<ErrorSummary>,
}

#[tauri::command]
pub(crate) async fn get_settings(engine: State<'_, Engine>) -> Result<SettingsDto, ErrorSummary> {
    Ok(match engine.settings().await {
        Ok(settings) => SettingsDto {
            theme: settings.theme,
            dark_theme: settings.dark_theme,
            keymap: settings.keymap.gui,
            copy_on_select: settings.gui.copy_on_select,
            error: None,
        },
        Err(e) => SettingsDto {
            error: Some(e.summary()),
            ..SettingsDto::default()
        },
    })
}

/// Remembers the theme (`auto` or an id) and the dark theme `auto` uses.
/// Only known values are written, and only over a readable file: an
/// invalid `settings.toml` is reported, never replaced (its keymaps would
/// be lost).
#[tauri::command]
pub(crate) async fn save_theme(
    engine: State<'_, Engine>,
    theme: String,
    dark_theme: String,
) -> Result<(), ErrorSummary> {
    if theme != portico_theme::AUTO {
        check_theme(&theme)?;
    }
    check_dark_theme(&dark_theme)?;
    // Only these keys change, under the core's lock: a keymap saved
    // meanwhile (TUI, another window) is kept.
    engine
        .set_theme(Some(theme), Some(dark_theme))
        .await
        .map(|_| ())
        .map_err(|e| e.summary())
}

/// Remembers "Copy on select" (only that key of `settings.toml` changes).
#[tauri::command]
pub(crate) async fn save_copy_on_select(
    engine: State<'_, Engine>,
    on: bool,
) -> Result<(), ErrorSummary> {
    engine
        .set_copy_on_select(on)
        .await
        .map(|_| ())
        .map_err(|e| e.summary())
}

/// The window follows the theme painted by the view: title bar and window
/// controls in the theme's appearance, and the window background (seen
/// while resizing, and at the next start before the page paints) in its
/// `bg-base`. A command rather than a window capability: the webview gets
/// no general `set_theme` permission.
#[tauri::command]
pub(crate) fn set_window_theme(
    window: tauri::WebviewWindow,
    theme: String,
) -> Result<(), ErrorSummary> {
    let found = check_theme(&theme)?;
    let failed = |_| ErrorSummary {
        code: "window_theme".into(),
        message: "the window did not take the theme".into(),
        hint: None,
    };
    window
        .set_theme(Some(window_theme(found.appearance)))
        .map_err(failed)?;
    window
        .set_background_color(Some(background(found)))
        .map_err(failed)
}

pub(crate) fn window_theme(appearance: portico_theme::Appearance) -> tauri::Theme {
    match appearance {
        portico_theme::Appearance::Light => tauri::Theme::Light,
        portico_theme::Appearance::Dark => tauri::Theme::Dark,
    }
}

/// A theme's `bg-base` as a window colour (opaque).
pub(crate) fn background(theme: &portico_theme::Theme) -> tauri::window::Color {
    let c = theme.palette.bg_base;
    tauri::window::Color(c.0, c.1, c.2, u8::MAX)
}

fn check_theme(theme: &str) -> Result<&'static portico_theme::Theme, ErrorSummary> {
    portico_theme::Theme::by_id(theme).ok_or_else(|| ErrorSummary {
        code: "unknown_theme".into(),
        message: "this theme does not exist".into(),
        hint: Some("Pick one of the themes listed in the command palette.".into()),
    })
}

fn check_dark_theme(theme: &str) -> Result<(), ErrorSummary> {
    match portico_theme::Theme::by_id(theme) {
        Some(t) if !t.is_light() => Ok(()),
        _ => Err(ErrorSummary {
            code: "unknown_theme".into(),
            message: "this is not a dark theme".into(),
            hint: Some("Auto uses one of the dark themes on a dark system.".into()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_themes_are_saved() {
        assert!(check_theme("obsidian").is_ok());
        assert!(check_theme("travertine").is_ok());
        assert!(check_theme("marble").is_ok());
        assert_eq!(check_theme("solarized").unwrap_err().code, "unknown_theme");
        // `auto` is a setting, not a theme the window can take.
        assert!(check_theme("auto").is_err());
        assert!(check_dark_theme("basalt").is_ok());
        assert!(check_dark_theme("marble").is_err());
        assert!(check_dark_theme("auto").is_err());
    }
}
