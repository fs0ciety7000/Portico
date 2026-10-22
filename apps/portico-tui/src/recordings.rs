//! The "Recordings" panel (phase 6): the files of
//! `Engine::list_recordings`, newest first, to play or delete.
//!
//! Only names that came from the listing are ever handed back to the
//! engine (`open_recording`, `delete_recording`). A deletion is an armed
//! confirmation: keys are ignored for [`ARM_DELAY`], then only `y`
//! deletes, `n` and `Esc` keep the file, and other keys are ignored (the
//! same rule as the recording confirmation). A recording still being
//! written is refused up front (no question that would only fail): the
//! panel says so, and its delete key is not offered; the engine refuses it
//! too (`recording_active`), should another Portico be writing it. Files
//! whose header is invalid stay listed, marked, and can be deleted.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::recording::RecordingEntry;
use ratatui::widgets::ListState;

use crate::dialog::ARM_DELAY;
use crate::keys::{Load, Notice};

/// What the app should do after a key in the panel.
#[derive(Debug, PartialEq)]
pub(crate) enum RecOutcome {
    None,
    Close,
    Reload,
    /// Play this entry (name from the listing).
    Play(String, Option<f64>),
    /// Delete this entry (name from the listing), confirmed.
    Delete(String),
}

/// A deletion waiting for `y`.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmDelete {
    pub(crate) name: String,
    pub(crate) opened_at: Instant,
}

#[derive(Debug)]
pub(crate) struct RecordingsPanel {
    pub(crate) entries: Load<Vec<RecordingEntry>>,
    pub(crate) list: ListState,
    pub(crate) confirm: Option<ConfirmDelete>,
    pub(crate) notice: Option<Notice>,
    /// The recordings folder (shown in the empty state).
    pub(crate) folder: String,
    /// A playback is being opened.
    pub(crate) opening: bool,
    /// Files being written right now (from the engine's events).
    pub(crate) active: Vec<String>,
    /// A deletion refused up front: `(message, hint)`, as a warning.
    pub(crate) refused: Option<(&'static str, &'static str)>,
}

/// Refusal of a deletion while the file is being written (shared with the
/// GUI).
pub(crate) const STILL_WRITTEN: (&str, &str) = (
    "This recording is still being written",
    "Stop the recording first",
);

impl RecordingsPanel {
    pub(crate) fn new(folder: String) -> Self {
        Self {
            entries: Load::Loading,
            list: ListState::default(),
            confirm: None,
            notice: None,
            folder,
            opening: false,
            active: Vec::new(),
            refused: None,
        }
    }

    /// The selected file is being written (it cannot be deleted yet).
    pub(crate) fn selected_active(&self) -> bool {
        self.selected()
            .is_some_and(|e| self.active.contains(&e.name))
    }

    pub(crate) fn busy(&self) -> bool {
        matches!(self.entries, Load::Loading) || self.opening
    }

    /// New listing: the selection stays on its file when it is still there.
    pub(crate) fn loaded(&mut self, result: Result<Vec<RecordingEntry>, ErrorSummary>) {
        let before = self.selected().map(|e| e.name.clone());
        self.entries = match result {
            Ok(entries) => Load::Ready(entries),
            Err(error) => Load::Failed(error),
        };
        let entries = self.entries.ready().map_or(&[][..], Vec::as_slice);
        let index = before
            .and_then(|name| entries.iter().position(|e| e.name == name))
            .or_else(|| (!entries.is_empty()).then_some(0));
        let index = index.map(|i| i.min(entries.len().saturating_sub(1)));
        self.list.select(index);
        if self
            .confirm
            .as_ref()
            .is_some_and(|c| !entries.iter().any(|e| e.name == c.name))
        {
            self.confirm = None;
        }
    }

    pub(crate) fn selected(&self) -> Option<&RecordingEntry> {
        self.entries.ready()?.get(self.list.selected()?)
    }

    fn len(&self) -> usize {
        self.entries.ready().map_or(0, Vec::len)
    }

    fn select(&mut self, index: usize) {
        let len = self.len();
        if len > 0 {
            self.list.select(Some(index.min(len - 1)));
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> RecOutcome {
        if key.kind == KeyEventKind::Release {
            return RecOutcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(confirm) = &self.confirm {
            // Keys typed before the question showed are not an answer.
            if confirm.opened_at.elapsed() < ARM_DELAY {
                return RecOutcome::None;
            }
            let name = confirm.name.clone();
            return match key.code {
                KeyCode::Char('y') if !ctrl => {
                    self.confirm = None;
                    RecOutcome::Delete(name)
                }
                KeyCode::Char('n') if !ctrl => {
                    self.confirm = None;
                    RecOutcome::None
                }
                KeyCode::Esc => {
                    self.confirm = None;
                    RecOutcome::None
                }
                // Not an answer.
                _ => RecOutcome::None,
            };
        }
        self.notice = None;
        self.refused = None;
        let current = self.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return RecOutcome::Close,
            KeyCode::Char('c') if ctrl => return RecOutcome::Close,
            KeyCode::Char('r') if ctrl => return RecOutcome::Reload,
            KeyCode::Down | KeyCode::Char('j') => self.select(current.saturating_add(1)),
            KeyCode::Up | KeyCode::Char('k') => self.select(current.saturating_sub(1)),
            KeyCode::PageDown => self.select(current.saturating_add(5)),
            KeyCode::PageUp => self.select(current.saturating_sub(5)),
            KeyCode::Home | KeyCode::Char('g') => self.select(0),
            KeyCode::End | KeyCode::Char('G') => self.select(usize::MAX),
            // A file whose header is invalid cannot be played (deleted, yes).
            KeyCode::Enter if !self.opening => {
                if let Some(entry) = self.selected().filter(|e| e.valid) {
                    return RecOutcome::Play(entry.name.clone(), entry.duration);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if self.selected_active() => {
                self.refused = Some(STILL_WRITTEN);
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(entry) = self.selected() {
                    self.confirm = Some(ConfirmDelete {
                        name: entry.name.clone(),
                        opened_at: Instant::now(),
                    });
                }
            }
            _ => {}
        }
        RecOutcome::None
    }

    /// Footer keys (most important first).
    pub(crate) fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm.is_some() {
            return vec![("n / Esc", "keep"), ("y", "delete")];
        }
        let mut hints = Vec::new();
        if self.selected().is_some_and(|e| e.valid) {
            hints.push(("↵", "play"));
        }
        // A file being written cannot be deleted yet: not offered.
        if self.selected().is_some() && !self.selected_active() {
            hints.push(("d", "delete"));
        }
        hints.extend([("Ctrl-R", "refresh"), ("Esc", "close")]);
        hints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, valid: bool) -> RecordingEntry {
        RecordingEntry {
            name: name.into(),
            alias: name.trim_end_matches(".cast").into(),
            started: None,
            size: 120,
            duration: Some(3.0),
            width: Some(80),
            height: Some(24),
            valid,
        }
    }

    fn press(panel: &mut RecordingsPanel, code: KeyCode) -> RecOutcome {
        panel.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn arm(panel: &mut RecordingsPanel) {
        if let Some(c) = &mut panel.confirm {
            c.opened_at = Instant::now()
                .checked_sub(ARM_DELAY * 2)
                .unwrap_or_else(Instant::now);
        }
    }

    #[test]
    fn play_and_delete_use_listed_names_only() {
        let mut panel = RecordingsPanel::new("/r".into());
        panel.loaded(Ok(vec![entry("b.cast", true), entry("a.cast", false)]));
        assert_eq!(panel.selected().map(|e| e.name.as_str()), Some("b.cast"));
        assert_eq!(
            press(&mut panel, KeyCode::Enter),
            RecOutcome::Play("b.cast".into(), Some(3.0))
        );
        press(&mut panel, KeyCode::Down);
        // An invalid entry cannot be played, but it is still deletable.
        assert_eq!(press(&mut panel, KeyCode::Enter), RecOutcome::None);
        assert!(!panel.hints().contains(&("↵", "play")));
        assert_eq!(press(&mut panel, KeyCode::Char('d')), RecOutcome::None);
        assert!(panel.confirm.is_some());
        // Not armed yet: ignored, the question stays.
        assert_eq!(press(&mut panel, KeyCode::Char('y')), RecOutcome::None);
        assert!(panel.confirm.is_some());
        arm(&mut panel);
        assert_eq!(
            press(&mut panel, KeyCode::Char('y')),
            RecOutcome::Delete("a.cast".into())
        );
        // A key that is not an answer is ignored; `n` and `Esc` keep it.
        for keep in [KeyCode::Char('n'), KeyCode::Esc] {
            press(&mut panel, KeyCode::Char('d'));
            arm(&mut panel);
            assert_eq!(press(&mut panel, KeyCode::Enter), RecOutcome::None);
            assert_eq!(press(&mut panel, KeyCode::Char('Y')), RecOutcome::None);
            assert!(panel.confirm.is_some());
            assert_eq!(press(&mut panel, keep), RecOutcome::None);
            assert!(panel.confirm.is_none(), "{keep:?}");
        }
        assert_eq!(press(&mut panel, KeyCode::Esc), RecOutcome::Close);
    }

    #[test]
    fn a_file_being_written_is_refused_before_asking() {
        let mut panel = RecordingsPanel::new("/r".into());
        panel.loaded(Ok(vec![entry("live.cast", true), entry("old.cast", true)]));
        panel.active = vec!["live.cast".into()];
        assert!(!panel.hints().contains(&("d", "delete")));
        assert_eq!(press(&mut panel, KeyCode::Char('d')), RecOutcome::None);
        assert!(panel.confirm.is_none(), "no question that would only fail");
        assert_eq!(panel.refused, Some(STILL_WRITTEN));
        // The next key clears it; another file can be deleted.
        press(&mut panel, KeyCode::Down);
        assert_eq!(panel.refused, None);
        assert!(panel.hints().contains(&("d", "delete")));
        press(&mut panel, KeyCode::Char('d'));
        assert!(panel.confirm.is_some());
    }

    #[test]
    fn a_reload_keeps_the_selection_on_its_file() {
        let mut panel = RecordingsPanel::new("/r".into());
        panel.loaded(Ok(vec![entry("c.cast", true), entry("b.cast", true)]));
        press(&mut panel, KeyCode::Down);
        panel.loaded(Ok(vec![
            entry("d.cast", true),
            entry("c.cast", true),
            entry("b.cast", true),
        ]));
        assert_eq!(panel.selected().map(|e| e.name.as_str()), Some("b.cast"));
        panel.loaded(Ok(vec![]));
        assert_eq!(panel.list.selected(), None);
    }
}
