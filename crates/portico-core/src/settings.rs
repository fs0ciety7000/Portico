//! User settings: `settings.toml` in the config dir (Phase 4).
//!
//! ```toml
//! theme = "auto"          # or a theme id: "obsidian", "marble"…
//! dark_theme = "basalt"   # the dark theme `auto` uses
//!
//! [keymap.tui]
//! "connect" = "enter"
//!
//! [keymap.gui]
//! "palette" = "mod+k"
//!
//! [gui]
//! copy_on_select = true
//!
//! [updates]
//! check = true
//! ```
//!
//! The core only stores settings: key syntax and theme ids are each UI's
//! business. It bounds sizes ([`MAX_KEYMAP_ENTRIES`] per keymap, strings of
//! at most [`MAX_SETTING_CHARS`] characters, no control, bidi or invisible
//! character) so a hand-edited or hostile file cannot bloat the UIs.
//!
//! [`save`] is a read-modify-write of the TOML *document* under an
//! exclusive inter-process lock (`settings.toml.lock`): the known keys
//! (`theme`, `dark_theme`, `keymap.tui`, `keymap.gui`, `gui.copy_on_select`, `updates.check`) are replaced, every other key,
//! comment and layout is kept. The write is atomic (`0600`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::store;

/// Most entries in one keymap.
pub const MAX_KEYMAP_ENTRIES: usize = 200;
/// Longest theme id, action name or key binding, in characters.
pub const MAX_SETTING_CHARS: usize = 64;
/// Largest `settings.toml` read, in bytes.
const MAX_SETTINGS_FILE_BYTES: u64 = 256 * 1024;

/// Content of `settings.toml` (unknown keys are ignored here and kept in
/// the file by [`save`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Theme id (`obsidian`, `basalt`, `travertine`, `marble`…) or `auto`
    /// (follow the system); `None` = the UIs' default, `auto`.
    pub theme: Option<String>,
    /// The dark theme `auto` uses when the system is dark (the last dark
    /// theme chosen explicitly); `None` = the UIs' default dark theme.
    pub dark_theme: Option<String>,
    pub keymap: Keymaps,
    /// Preferences of the desktop app only (phase 9).
    pub gui: GuiSettings,
    /// The opt-in update check ([`crate::update`]).
    pub updates: UpdateSettings,
}

/// `[updates]`: the opt-in update check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// `true`: check GitHub for a newer release once a day; `false`: never;
    /// absent (`None`): not asked yet (no check either, the UIs ask once).
    pub check: Option<bool>,
}

/// `[gui]`: desktop app preferences. Absent keys take their default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiSettings {
    /// A finished mouse selection in a terminal goes to the clipboard
    /// (default off: the clipboard only changes on an explicit copy).
    pub copy_on_select: bool,
}

/// Keymap overrides per UI: action name → key binding, both opaque to the core.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keymaps {
    pub tui: BTreeMap<String, String>,
    pub gui: BTreeMap<String, String>,
}

/// Settings out of bounds. Messages name the field, never the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SettingsError {
    #[error("invalid setting {field}: {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
}

impl SettingsError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Invalid { .. } => (
                "settings_invalid",
                Some("Fix or remove the offending entry in settings.toml."),
            ),
        }
    }
}

impl Settings {
    /// Checks the bounds described in the module doc.
    pub fn validate(&self) -> std::result::Result<(), SettingsError> {
        if let Some(theme) = &self.theme {
            check_string("theme", theme)?;
        }
        if let Some(theme) = &self.dark_theme {
            check_string("dark_theme", theme)?;
        }
        for (field, map) in [
            ("keymap.tui", &self.keymap.tui),
            ("keymap.gui", &self.keymap.gui),
        ] {
            if map.len() > MAX_KEYMAP_ENTRIES {
                return Err(SettingsError::Invalid {
                    field,
                    reason: "more than 200 entries",
                });
            }
            for (action, binding) in map {
                check_string(field, action)?;
                check_string(field, binding)?;
            }
        }
        Ok(())
    }
}

fn check_string(field: &'static str, value: &str) -> std::result::Result<(), SettingsError> {
    let invalid = |reason| Err(SettingsError::Invalid { field, reason });
    if value.trim().is_empty() {
        return invalid("empty value");
    }
    if value.chars().count() > MAX_SETTING_CHARS {
        return invalid("longer than 64 characters");
    }
    if value
        .chars()
        .any(|c| c.is_control() || crate::snippets::is_hidden_format(c))
    {
        return invalid("contains a control, bidi or invisible character");
    }
    Ok(())
}

/// Reads and validates `settings.toml` (missing file: defaults). On error
/// the UIs fall back to their defaults and show the summary.
pub(crate) fn load(path: &Path) -> Result<Settings> {
    let settings: Settings = store::load_toml_capped(path, MAX_SETTINGS_FILE_BYTES)?;
    settings.validate()?;
    Ok(settings)
}

/// Sets `key` to a string, keeping the decoration (comments above it, inline
/// comment) of an existing entry: `insert` would replace the whole entry.
fn set_string(table: &mut toml_edit::Table, key: &str, text: &str) {
    if let Some(value) = table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        let decor = value.decor().clone();
        *value = toml_edit::Value::from(text);
        *value.decor_mut() = decor;
        return;
    }
    table.insert(key, toml_edit::value(text));
}

/// Validates and writes `settings` (see the module doc); returns them as
/// the next [`load`] will read them. Blocking.
pub(crate) fn save(path: &Path, settings: &Settings) -> Result<Settings> {
    settings.validate()?;
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    save_locked(path, settings)
}

/// Changes only the theme (and, when `dark_theme` is given, the dark theme
/// `auto` uses; `None` keeps it): read and write happen under one lock, so a
/// keymap saved meanwhile by another UI is never overwritten. Blocking.
pub(crate) fn set_theme(
    path: &Path,
    theme: Option<&str>,
    dark_theme: Option<&str>,
) -> Result<Settings> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut settings = load(path)?;
    settings.theme = theme.map(str::to_owned);
    if let Some(dark) = dark_theme {
        settings.dark_theme = Some(dark.to_owned());
    }
    settings.validate()?;
    save_locked(path, &settings)
}

/// Changes only `gui.copy_on_select`, under one lock like [`set_theme`].
/// Blocking.
pub(crate) fn set_copy_on_select(path: &Path, on: bool) -> Result<Settings> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut settings = load(path)?;
    settings.gui.copy_on_select = on;
    save_locked(path, &settings)
}

/// Changes only `updates.check`, under one lock like [`set_theme`].
/// Blocking.
pub(crate) fn set_update_check(path: &Path, on: bool) -> Result<Settings> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut settings = load(path)?;
    settings.updates.check = Some(on);
    save_locked(path, &settings)
}

/// Sets `key` to a boolean, keeping the decoration of an existing entry.
fn set_bool(table: &mut toml_edit::Table, key: &str, on: bool) {
    if let Some(value) = table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        let decor = value.decor().clone();
        *value = toml_edit::Value::from(on);
        *value.decor_mut() = decor;
        return;
    }
    table.insert(key, toml_edit::value(on));
}

fn save_locked(path: &Path, settings: &Settings) -> Result<Settings> {
    let text = store::read_capped(path, MAX_SETTINGS_FILE_BYTES)?.unwrap_or_default();
    let mut doc = store::parse_document(path, &text)?;
    let root = doc.as_table_mut();
    for (key, value) in [
        ("theme", &settings.theme),
        ("dark_theme", &settings.dark_theme),
    ] {
        match value {
            Some(theme) => set_string(root, key, theme),
            None => {
                root.remove(key);
            }
        }
    }
    let keymap = store::child_table(root, "keymap", path)?;
    for (name, map) in [("tui", &settings.keymap.tui), ("gui", &settings.keymap.gui)] {
        if map.is_empty() {
            keymap.remove(name);
            continue;
        }
        // Edited in place: comments and order of the kept entries survive.
        if !keymap.get(name).is_some_and(toml_edit::Item::is_table) {
            keymap.insert(name, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        if let Some(table) = keymap.get_mut(name).and_then(toml_edit::Item::as_table_mut) {
            table.retain(|action, _| map.contains_key(action));
            for (action, binding) in map {
                set_string(table, action, binding);
            }
        }
    }
    if keymap.is_empty() {
        root.remove("keymap");
    }
    // `[gui]`: only the known key; a default value is not written.
    let gui = store::child_table(root, "gui", path)?;
    if settings.gui.copy_on_select {
        set_bool(gui, "copy_on_select", true);
    } else {
        gui.remove("copy_on_select");
    }
    if gui.is_empty() {
        root.remove("gui");
    }
    // `[updates]`: `check` is written once answered (`false` included: the
    // question is never asked again); absent means "not asked yet".
    let updates = store::child_table(root, "updates", path)?;
    match settings.updates.check {
        Some(on) => set_bool(updates, "check", on),
        None => {
            updates.remove("check");
        }
    }
    if updates.is_empty() {
        root.remove("updates");
    }
    let written = doc.to_string();
    // Same parse as `load`: never report settings the next read would not see.
    let stored: Settings = store::parse_toml(path, &written)?;
    store::write_atomic_capped(path, written.as_bytes(), MAX_SETTINGS_FILE_BYTES)?;
    Ok(stored)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn set_theme_keeps_a_keymap_saved_in_between() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        // A UI read the settings before the other UI saved its keymap…
        let stale = load(&path).expect("load");
        let mut other = stale.clone();
        other.keymap.gui.insert("app.logs".into(), "F12".into());
        save(&path, &other).expect("save");
        // …then changes the theme: the keymap survives.
        let saved = set_theme(&path, Some("basalt"), None).expect("theme");
        assert_eq!(saved.theme.as_deref(), Some("basalt"));
        assert_eq!(
            saved.keymap.gui.get("app.logs").map(String::as_str),
            Some("F12")
        );
    }

    #[test]
    fn dark_theme_is_kept_unless_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        let saved = set_theme(&path, Some("basalt"), Some("basalt")).expect("theme");
        assert_eq!(saved.dark_theme.as_deref(), Some("basalt"));
        // `auto` (or a light theme) keeps the remembered dark theme.
        let saved = set_theme(&path, Some("auto"), None).expect("auto");
        assert_eq!(saved.theme.as_deref(), Some("auto"));
        assert_eq!(saved.dark_theme.as_deref(), Some("basalt"));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("theme = \"auto\""), "{text}");
        assert!(text.contains("dark_theme = \"basalt\""), "{text}");
        let saved = set_theme(&path, None, None).expect("default");
        assert_eq!(saved.theme, None);
        assert_eq!(
            load(&path).expect("load").dark_theme.as_deref(),
            Some("basalt")
        );
        // Bounds apply to the new key too.
        let bad = Settings {
            dark_theme: Some(" ".into()),
            ..Settings::default()
        };
        assert!(matches!(
            bad.validate(),
            Err(SettingsError::Invalid {
                field: "dark_theme",
                ..
            })
        ));
    }

    #[test]
    fn comments_next_to_saved_keys_survive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "# hand comment\ntheme = \"obsidian\" # inline\n\n[keymap.tui]\n# files on F\n\"hosts.files\" = \"F\"\n\"hosts.debug\" = \"L\"\n",
        )
        .expect("write");
        let mut settings = load(&path).expect("load");
        settings.theme = Some("basalt".into());
        settings.keymap.tui.insert("hosts.files".into(), "G".into());
        settings.keymap.tui.remove("hosts.debug");
        save(&path, &settings).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(
            text.contains("# hand comment\ntheme = \"basalt\" # inline"),
            "{text}"
        );
        assert!(
            text.contains("# files on F\n\"hosts.files\" = \"G\""),
            "{text}"
        );
        assert!(!text.contains("hosts.debug"), "{text}");
    }

    #[test]
    fn copy_on_select_is_written_alone_and_removed_when_off() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "theme = \"basalt\"\n\n[gui]\n# mine\nfont = \"x\"\n\n[keymap.gui]\n\"app.logs\" = \"F12\"\n",
        )
        .expect("write");
        let on = set_copy_on_select(&path, true).expect("on");
        assert!(on.gui.copy_on_select);
        assert_eq!(on.theme.as_deref(), Some("basalt"));
        assert_eq!(
            on.keymap.gui.get("app.logs").map(String::as_str),
            Some("F12")
        );
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("copy_on_select = true"), "{text}");
        assert!(text.contains("# mine\nfont = \"x\""), "{text}");
        assert!(load(&path).expect("load").gui.copy_on_select);

        let off = set_copy_on_select(&path, false).expect("off");
        assert!(!off.gui.copy_on_select);
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("copy_on_select"), "{text}");
        // Another key of `[gui]` keeps the table.
        assert!(text.contains("[gui]"), "{text}");

        // Nothing else in `[gui]`: the table goes with the key.
        let bare = dir.path().join("bare.toml");
        set_copy_on_select(&bare, true).expect("on");
        set_copy_on_select(&bare, false).expect("off");
        let text = std::fs::read_to_string(&bare).expect("read");
        assert!(!text.contains("gui"), "{text}");
    }

    #[test]
    fn update_check_is_absent_until_answered_then_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        std::fs::write(
            &path,
            "theme = \"basalt\"\n\n[updates]\n# mine\nchannel = \"x\"\n",
        )
        .expect("write");
        assert_eq!(load(&path).expect("load").updates.check, None);
        // An unrelated save never answers the question for the user.
        set_theme(&path, Some("obsidian"), None).expect("theme");
        assert_eq!(load(&path).expect("load").updates.check, None);
        assert!(
            !std::fs::read_to_string(&path)
                .expect("read")
                .contains("check")
        );

        let off = set_update_check(&path, false).expect("off");
        assert_eq!(off.updates.check, Some(false));
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("check = false"), "{text}");
        assert!(text.contains("# mine\nchannel = \"x\""), "{text}");
        assert_eq!(load(&path).expect("load").updates.check, Some(false));
        // `false` survives a save of the other keys.
        set_copy_on_select(&path, true).expect("copy");
        assert_eq!(load(&path).expect("load").updates.check, Some(false));

        let on = set_update_check(&path, true).expect("on");
        assert_eq!(on.updates.check, Some(true));
        assert_eq!(on.theme.as_deref(), Some("obsidian"));
        assert!(on.gui.copy_on_select);

        // Back to "not asked": the key goes, and the table with it when empty.
        let bare = dir.path().join("bare.toml");
        set_update_check(&bare, true).expect("on");
        save(&bare, &Settings::default()).expect("save");
        let text = std::fs::read_to_string(&bare).expect("read");
        assert!(!text.contains("updates"), "{text}");

        std::fs::write(&bare, "[updates]\ncheck = \"daily\"\n").expect("write");
        assert!(load(&bare).is_err());
    }

    #[test]
    fn a_wrong_type_for_copy_on_select_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[gui]\ncopy_on_select = \"yes\"\n").expect("write");
        assert!(load(&path).is_err());
    }

    fn sample() -> Settings {
        let mut settings = Settings {
            theme: Some("basalt".into()),
            ..Settings::default()
        };
        settings.keymap.tui.insert("connect".into(), "enter".into());
        settings.keymap.gui.insert("palette".into(), "mod+k".into());
        settings
    }

    #[test]
    fn roundtrip_keeps_unknown_keys_and_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        assert_eq!(load(&path).expect("missing"), Settings::default());
        std::fs::write(
            &path,
            "# mine\nfont_size = 14\ntheme = \"obsidian\"\n\n[keymap]\nvim = { x = \"y\" }\n\n[keymap.tui]\nold = \"x\"\n\n[experimental]\nsplit = true\n",
        )
        .expect("write");
        assert_eq!(
            load(&path).expect("load").theme.as_deref(),
            Some("obsidian")
        );

        let stored = save(&path, &sample()).expect("save");
        assert_eq!(stored, sample());
        assert_eq!(load(&path).expect("load"), sample());
        let text = std::fs::read_to_string(&path).expect("read");
        for kept in [
            "# mine",
            "font_size = 14",
            "[experimental]",
            "split = true",
            "vim",
        ] {
            assert!(text.contains(kept), "lost {kept:?} in:\n{text}");
        }
        assert!(!text.contains("old"), "{text}");

        let cleared = save(&path, &Settings::default()).expect("save");
        assert_eq!(cleared, Settings::default());
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("theme"), "{text}");
        assert!(text.contains("vim"), "{text}");
    }

    #[test]
    fn bounds_are_enforced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        let mut big = Settings::default();
        for i in 0..=MAX_KEYMAP_ENTRIES {
            big.keymap.gui.insert(format!("a{i}"), "x".into());
        }
        let bad = [
            big,
            Settings {
                theme: Some("t".repeat(65)),
                ..Settings::default()
            },
            Settings {
                theme: Some("a\u{202e}b".into()),
                ..Settings::default()
            },
            {
                let mut s = Settings::default();
                s.keymap.tui.insert("ok".into(), "ctrl+\u{1b}".into());
                s
            },
            {
                let mut s = Settings::default();
                s.keymap.tui.insert(" ".into(), "x".into());
                s
            },
        ];
        for settings in bad {
            let err = save(&path, &settings).expect_err("invalid");
            assert_eq!(err.summary().code, "settings_invalid");
        }
        assert!(!path.exists());

        // A hand-edited file out of bounds is reported on load.
        std::fs::write(&path, format!("theme = \"{}\"\n", "t".repeat(65))).expect("write");
        assert_eq!(
            load(&path).expect_err("bad").summary().code,
            "settings_invalid"
        );
        std::fs::write(&path, "keymap = 1\n").expect("write");
        assert_eq!(load(&path).expect_err("bad").summary().code, "serde");
        assert!(
            save(&path, &sample()).is_err(),
            "never replaces a non-table"
        );
    }

    #[test]
    fn save_never_writes_a_file_the_reader_rejects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.toml");
        let filler = usize::try_from(MAX_SETTINGS_FILE_BYTES).expect("usize") - 20;
        let original = format!("#{}\n", "x".repeat(filler));
        std::fs::write(&path, &original).expect("write");
        let err = save(&path, &sample()).expect_err("too large");
        assert_eq!(err.summary().code, "file_too_large");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_settings_are_never_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("real.toml");
        std::fs::write(&target, "theme = \"basalt\"\n").expect("write");
        let path = dir.path().join("settings.toml");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        assert_eq!(load(&path).expect("load").theme.as_deref(), Some("basalt"));
        let err = save(&path, &sample()).expect_err("symlink");
        assert_eq!(err.summary().code, "not_regular_file");
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "theme = \"basalt\"\n"
        );
    }
}
