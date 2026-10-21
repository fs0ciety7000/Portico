//! User settings: `settings.toml` in the config dir (Phase 4).
//!
//! ```toml
//! theme = "obsidian"
//!
//! [keymap.tui]
//! "connect" = "enter"
//!
//! [keymap.gui]
//! "palette" = "mod+k"
//! ```
//!
//! The core only stores settings: key syntax and theme ids are each UI's
//! business. It bounds sizes ([`MAX_KEYMAP_ENTRIES`] per keymap, strings of
//! at most [`MAX_SETTING_CHARS`] characters, no control, bidi or invisible
//! character) so a hand-edited or hostile file cannot bloat the UIs.
//!
//! [`save`] is a read-modify-write of the TOML *document* under an
//! exclusive inter-process lock (`settings.toml.lock`): the known keys
//! (`theme`, `keymap.tui`, `keymap.gui`) are replaced, every other key,
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
    /// Theme id (`obsidian`, `basalt`, `travertine-night`…); `None` = default.
    pub theme: Option<String>,
    pub keymap: Keymaps,
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
    let text = store::read_capped(path, MAX_SETTINGS_FILE_BYTES)?.unwrap_or_default();
    let mut doc = store::parse_document(path, &text)?;
    let root = doc.as_table_mut();
    match &settings.theme {
        Some(theme) => set_string(root, "theme", theme),
        None => {
            root.remove("theme");
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
