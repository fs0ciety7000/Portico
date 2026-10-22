//! "Edit <alias>" form (pure): tags, group and notes of a host, stored in
//! `hosts.toml` by the engine (Portico never edits `~/.ssh/config`).
//!
//! Tags are typed freely, separated by spaces or commas, `#` optional; they
//! are checked as they are typed with the engine's own rules
//! ([`portico_core::hosts::normalize_tags`]) and shown as chips. Notes span
//! lines (`↵` breaks the line there). Only the fields that changed are
//! sent; an engine refusal comes back on the field it concerns.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::hosts::{
    HostMeta, HostMetaError, MAX_GROUP_CHARS, MAX_NOTES_CHARS, MAX_TAG_CHARS, MAX_TAGS,
    normalize_tags,
};

use crate::form::{paste_line, push_char};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditField {
    Tags,
    Group,
    Notes,
}

const FIELDS: [EditField; 3] = [EditField::Tags, EditField::Group, EditField::Notes];
/// Room for the most tags, separators included.
const MAX_TAGS_INPUT: usize = (MAX_TAG_CHARS + 2) * MAX_TAGS;

/// What to write: `None` = unchanged (so `Some(None)` clears the field).
#[derive(Debug, Clone, PartialEq, Eq)]
// `Option<Option<_>>`: unchanged / cleared / set are three distinct cases.
#[allow(clippy::option_option)]
pub(crate) struct EditRequest {
    pub(crate) alias: String,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) group: Option<Option<String>>,
    pub(crate) notes: Option<Option<String>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EditOutcome {
    None,
    Close,
    Save(EditRequest),
}

#[derive(Debug)]
pub(crate) struct EditHostPanel {
    pub(crate) alias: String,
    pub(crate) tags: String,
    pub(crate) group: String,
    pub(crate) notes: String,
    pub(crate) focus: EditField,
    /// Refusal from the engine, on its field.
    pub(crate) error: Option<(EditField, ErrorSummary)>,
    pub(crate) busy: bool,
    original: HostMeta,
}

/// `web, #prod db` → `["web", "#prod", "db"]`.
pub(crate) fn split_tags(text: &str) -> Vec<String> {
    text.split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

impl EditHostPanel {
    pub(crate) fn new(alias: String, meta: &HostMeta) -> Self {
        Self {
            alias,
            tags: meta.tags.join(" "),
            group: meta.group.clone().unwrap_or_default(),
            notes: meta.notes.clone().unwrap_or_default(),
            focus: EditField::Tags,
            error: None,
            busy: false,
            original: meta.clone(),
        }
    }

    /// The tags as the engine will store them, or why it would refuse.
    pub(crate) fn chips(&self) -> Result<Vec<String>, ErrorSummary> {
        normalize_tags(&split_tags(&self.tags)).map_err(|e| portico_core::Error::from(e).summary())
    }

    /// Index (in [`split_tags`] order) of the tag the engine would refuse,
    /// from the position its error carries.
    pub(crate) fn bad_tag(&self) -> Option<usize> {
        match normalize_tags(&split_tags(&self.tags)) {
            Err(HostMetaError::Tag { position, .. }) => position.checked_sub(1),
            _ => None,
        }
    }

    fn request(&self) -> EditRequest {
        let tags = split_tags(&self.tags);
        let same_tags = normalize_tags(&tags).is_ok_and(|t| t == self.original.tags);
        let group = Some(self.group.trim().to_owned()).filter(|g| !g.is_empty());
        let notes =
            Some(self.notes.replace("\r\n", "\n").trim().to_owned()).filter(|n| !n.is_empty());
        EditRequest {
            alias: self.alias.clone(),
            tags: (!same_tags).then_some(tags),
            group: (group != self.original.group).then_some(group),
            notes: (notes != self.original.notes).then_some(notes),
        }
    }

    fn text(&mut self) -> (&mut String, usize) {
        match self.focus {
            EditField::Tags => (&mut self.tags, MAX_TAGS_INPUT),
            EditField::Group => (&mut self.group, MAX_GROUP_CHARS),
            EditField::Notes => (&mut self.notes, MAX_NOTES_CHARS),
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let i = FIELDS.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = FIELDS.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus = FIELDS.get(next).copied().unwrap_or(EditField::Tags);
    }

    fn submit(&mut self) -> EditOutcome {
        if let Err(e) = self.chips() {
            self.focus = EditField::Tags;
            self.error = Some((EditField::Tags, e));
            return EditOutcome::None;
        }
        let request = self.request();
        if request.tags.is_none() && request.group.is_none() && request.notes.is_none() {
            return EditOutcome::Close;
        }
        self.busy = true;
        self.error = None;
        EditOutcome::Save(request)
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> EditOutcome {
        if self.busy {
            return EditOutcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let editing = |panel: &mut Self| {
            if panel.error.as_ref().is_some_and(|(f, _)| *f == panel.focus) {
                panel.error = None;
            }
        };
        match key.code {
            KeyCode::Esc => return EditOutcome::Close,
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Enter if self.focus == EditField::Notes => {
                if self.notes.chars().count() < MAX_NOTES_CHARS {
                    self.notes.push('\n');
                }
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab => self.move_focus(true),
            KeyCode::BackTab => self.move_focus(false),
            KeyCode::Down if self.focus != EditField::Notes => self.move_focus(true),
            KeyCode::Up if self.focus != EditField::Notes => self.move_focus(false),
            KeyCode::Char('u') if ctrl => {
                editing(self);
                self.text().0.clear();
            }
            KeyCode::Backspace => {
                editing(self);
                self.text().0.pop();
            }
            KeyCode::Char(c) if !ctrl => {
                editing(self);
                let (value, max) = self.text();
                push_char(value, c, max);
            }
            _ => {}
        }
        EditOutcome::None
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.busy {
            return;
        }
        if self.focus == EditField::Notes {
            for c in text.replace("\r\n", "\n").chars() {
                if self.notes.chars().count() >= MAX_NOTES_CHARS {
                    break;
                }
                if c == '\n' || !c.is_control() {
                    self.notes.push(c);
                }
            }
        } else {
            let (value, max) = self.text();
            paste_line(value, text, max);
        }
    }

    /// The engine refused a field: back to it, with the reason.
    pub(crate) fn refused(&mut self, field: EditField, error: ErrorSummary) {
        self.busy = false;
        self.focus = field;
        self.error = Some((field, error));
    }
}

/// The edit field a core error code concerns.
pub(crate) fn field_of(code: &str) -> Option<EditField> {
    match code {
        "host_tag" | "host_tags" => Some(EditField::Tags),
        "host_group" => Some(EditField::Group),
        "host_notes" => Some(EditField::Notes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(panel: &mut EditHostPanel, text: &str) {
        for c in text.chars() {
            panel.handle_key(k(KeyCode::Char(c)));
        }
    }

    #[test]
    fn tags_are_split_and_checked_live() {
        assert_eq!(split_tags(" web,#prod  db,"), ["web", "#prod", "db"]);
        let mut panel = EditHostPanel::new("web".into(), &HostMeta::default());
        typed(&mut panel, "#prod, eu WEB web");
        assert_eq!(
            panel.chips().ok(),
            Some(vec!["prod".into(), "eu".into(), "WEB".into()])
        );
        assert_eq!(panel.bad_tag(), None);
        typed(&mut panel, " bad!");
        let err = panel.chips().err().map(|e| e.code);
        assert_eq!(err.as_deref(), Some("host_tag"));
        // The fifth typed tag is the culprit (duplicates count as typed).
        assert_eq!(panel.bad_tag(), Some(4));
        // Saving with a bad tag stays on the field, with the reason.
        assert_eq!(panel.handle_key(k(KeyCode::Enter)), EditOutcome::None);
        assert!(matches!(&panel.error, Some((EditField::Tags, e)) if e.hint.is_some()));
        // Typing clears it.
        panel.handle_key(k(KeyCode::Backspace));
        assert!(panel.error.is_none());
    }

    #[test]
    fn only_changes_are_sent() {
        let meta = HostMeta {
            tags: vec!["prod".into()],
            group: Some("EU".into()),
            notes: Some("line".into()),
            ..HostMeta::default()
        };
        let mut panel = EditHostPanel::new("web".into(), &meta);
        assert_eq!(panel.handle_key(k(KeyCode::Enter)), EditOutcome::Close);
        panel.handle_key(k(KeyCode::Tab));
        panel.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        panel.handle_key(k(KeyCode::Tab));
        assert_eq!(panel.focus, EditField::Notes);
        panel.handle_key(k(KeyCode::Enter));
        typed(&mut panel, "two");
        assert_eq!(panel.notes, "line\ntwo");
        let outcome = panel.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            outcome,
            EditOutcome::Save(EditRequest {
                alias: "web".into(),
                tags: None,
                group: Some(None),
                notes: Some(Some("line\ntwo".into())),
            })
        );
        assert!(panel.busy);
        panel.refused(
            EditField::Notes,
            ErrorSummary {
                code: "host_notes".into(),
                message: "invalid notes".into(),
                hint: None,
            },
        );
        assert!(!panel.busy);
        assert_eq!(field_of("host_tags"), Some(EditField::Tags));
    }
}
