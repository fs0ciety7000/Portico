//! Snippets panel state: a filterable list of the snippets that apply to the
//! target host (the active tab's), the placeholder form, the editor and the
//! delete confirmation.
//!
//! Typing filters the list, so the commands use `Ctrl`: `Ctrl-N` new,
//! `Ctrl-E` edit, `Ctrl-D` delete. A snippet body is only ever typed into
//! the target tab's remote terminal (through
//! `portico_core::snippets::snippet_input`), never run locally.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::snippets::{MAX_COMMAND_BYTES, Snippet, SnippetScope};
use ratatui::widgets::ListState;

use crate::form::{MAX_LINE, paste_line, push_char};
use crate::keys::{Load, Notice};

/// Most placeholder value characters accepted by the form.
const MAX_VALUE: usize = 1024;

/// Bytes for the terminal, or why they cannot be sent as asked.
///
/// The core is moving `snippet_input` from `Vec<u8>` to `Result<Vec<u8>>`
/// (it refuses a multi-line paste-only snippet when the remote program has
/// not enabled bracketed paste: lines 1..n-1 would run). [`SnippetBytes`]
/// accepts both signatures, and the same refusal is checked here first so
/// the behaviour does not depend on which core this is built against.
pub(crate) fn snippet_bytes(
    rendered: &str,
    run: bool,
    bracketed_paste: bool,
) -> Result<Vec<u8>, ErrorSummary> {
    let multi_line = rendered
        .trim_end_matches(['\n', '\r'])
        .contains(['\n', '\r']);
    if multi_line && !run && !bracketed_paste {
        return Err(ErrorSummary {
            code: "snippet_needs_paste_mode".into(),
            message: "this multi-line snippet cannot be pasted without running".into(),
            hint: Some(
                "The remote program does not accept a paste as a whole, so each line would run. Run it, or cancel."
                    .into(),
            ),
        });
    }
    portico_core::snippets::snippet_input(rendered, run, bracketed_paste).into_bytes()
}

/// Bridges both `snippet_input` signatures (see [`snippet_bytes`]).
trait SnippetBytes {
    fn into_bytes(self) -> Result<Vec<u8>, ErrorSummary>;
}

impl SnippetBytes for Vec<u8> {
    fn into_bytes(self) -> Result<Vec<u8>, ErrorSummary> {
        Ok(self)
    }
}

impl SnippetBytes for portico_core::Result<Vec<u8>> {
    fn into_bytes(self) -> Result<Vec<u8>, ErrorSummary> {
        self.map_err(|e| e.summary())
    }
}

/// `global`, `hosts: web, db`, `#prod #eu`.
pub(crate) fn scope_label(scope: &SnippetScope) -> String {
    match scope {
        SnippetScope::Global => "global".to_owned(),
        SnippetScope::Hosts(hosts) if hosts.len() == 1 => format!("host {}", hosts.join("")),
        SnippetScope::Hosts(hosts) => format!("hosts {}", hosts.join(", ")),
        SnippetScope::Tags(tags) => tags
            .iter()
            .map(|t| format!("#{t}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Rank of `snippet` for `query` (higher is better), `None` if it does not
/// match. Name before command, substring before subsequence.
pub(crate) fn score(snippet: &Snippet, query: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let name = snippet.name.to_lowercase();
    let command = snippet.command.to_lowercase();
    if name.contains(&query) {
        Some(4)
    } else if command.contains(&query) {
        Some(3)
    } else if subsequence(&name, &query) {
        Some(2)
    } else if subsequence(&command, &query) {
        Some(1)
    } else {
        None
    }
}

fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|n| chars.any(|h| h == n))
}

/// Placeholder form: one line per `{{name}}`.
#[derive(Debug)]
pub(crate) struct FillForm {
    pub(crate) snippet: Snippet,
    pub(crate) values: Vec<(String, String)>,
    pub(crate) focus: usize,
    pub(crate) error: Option<ErrorSummary>,
}

impl FillForm {
    fn new(snippet: Snippet, remembered: &HashMap<String, String>) -> Self {
        let values = snippet
            .placeholders()
            .into_iter()
            .map(|p| {
                let value = remembered.get(p).cloned().unwrap_or_default();
                (p.to_owned(), value)
            })
            .collect();
        Self {
            snippet,
            values,
            focus: 0,
            error: None,
        }
    }

    /// Filled-in values; an empty field counts as missing (the core then
    /// names the placeholder that needs a value).
    fn pairs(&self) -> Vec<(&str, &str)> {
        self.values
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    /// The command as it will be typed, or why it cannot be.
    pub(crate) fn preview(&self) -> Result<String, ErrorSummary> {
        self.snippet
            .render_checked(&self.pairs())
            .map_err(|e| e.summary())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditField {
    Name,
    Command,
    Description,
    Scope,
    ScopeValues,
    Run,
}

/// Scope choices of the editor.
pub(crate) const SCOPES: [&str; 3] = ["Global", "Hosts", "Tags"];

/// Snippet editor (new or existing).
#[derive(Debug)]
pub(crate) struct EditForm {
    /// Empty for a new snippet (the core derives one).
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) command: String,
    pub(crate) description: String,
    pub(crate) scope: usize,
    /// Comma-separated hosts or tags.
    pub(crate) scope_values: String,
    pub(crate) run: bool,
    pub(crate) focus: EditField,
    pub(crate) error: Option<ErrorSummary>,
    pub(crate) busy: bool,
    /// `Ctrl-S` was pressed: empty required fields are pointed out.
    pub(crate) submitted: bool,
    /// Suggested scope values when switching to Hosts / Tags.
    default_host: Option<String>,
    default_tags: Vec<String>,
}

impl EditForm {
    pub(crate) fn new(host: Option<String>, tags: Vec<String>) -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            command: String::new(),
            description: String::new(),
            scope: 0,
            scope_values: String::new(),
            run: true,
            focus: EditField::Name,
            error: None,
            busy: false,
            submitted: false,
            default_host: host,
            default_tags: tags,
        }
    }

    pub(crate) fn edit(snippet: &Snippet, host: Option<String>, tags: Vec<String>) -> Self {
        let (scope, scope_values) = match &snippet.scope {
            SnippetScope::Global => (0, String::new()),
            SnippetScope::Hosts(h) => (1, h.join(", ")),
            SnippetScope::Tags(t) => (2, t.join(", ")),
        };
        Self {
            id: snippet.id.clone(),
            name: snippet.name.clone(),
            command: snippet.command.clone(),
            description: snippet.description.clone().unwrap_or_default(),
            scope,
            scope_values,
            run: snippet.run,
            ..Self::new(host, tags)
        }
    }

    fn fields(&self) -> Vec<EditField> {
        let mut fields = vec![
            EditField::Name,
            EditField::Command,
            EditField::Description,
            EditField::Scope,
        ];
        if self.scope != 0 {
            fields.push(EditField::ScopeValues);
        }
        fields.push(EditField::Run);
        fields
    }

    fn move_focus(&mut self, forward: bool) {
        let fields = self.fields();
        let i = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = fields.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus = fields.get(next).copied().unwrap_or(EditField::Name);
    }

    fn cycle_scope(&mut self, forward: bool) {
        let n = SCOPES.len();
        self.scope = if forward {
            (self.scope + 1) % n
        } else {
            (self.scope + n - 1) % n
        };
        self.scope_values = match self.scope {
            1 => self.default_host.clone().unwrap_or_default(),
            2 => self.default_tags.join(", "),
            _ => String::new(),
        };
    }

    fn entries(&self) -> Vec<String> {
        self.scope_values
            .split([',', ' '])
            .map(|e| e.trim().trim_start_matches('#').to_owned())
            .filter(|e| !e.is_empty())
            .collect()
    }

    /// Problem with a field, shown under it.
    pub(crate) fn problem(&self, field: EditField) -> Option<&'static str> {
        if !self.submitted {
            return None;
        }
        match field {
            EditField::Name if self.name.trim().is_empty() => Some("Name the snippet."),
            EditField::Command if self.command.trim().is_empty() => {
                Some("Type the command to send.")
            }
            EditField::ScopeValues if self.scope != 0 && self.entries().is_empty() => {
                Some(if self.scope == 1 {
                    "List at least one host alias."
                } else {
                    "List at least one tag."
                })
            }
            _ => None,
        }
    }

    fn snippet(&self) -> Snippet {
        let description = self.description.trim();
        Snippet {
            id: self.id.clone(),
            name: self.name.trim().to_owned(),
            command: self.command.clone(),
            description: (!description.is_empty()).then(|| description.to_owned()),
            scope: match self.scope {
                1 => SnippetScope::Hosts(self.entries()),
                2 => SnippetScope::Tags(self.entries()),
                _ => SnippetScope::Global,
            },
            run: self.run,
        }
    }

    fn submit(&mut self) -> Option<Snippet> {
        self.submitted = true;
        let first = [EditField::Name, EditField::Command, EditField::ScopeValues]
            .into_iter()
            .find(|f| self.problem(*f).is_some());
        if let Some(field) = first {
            self.focus = field;
            return None;
        }
        self.busy = true;
        self.error = None;
        Some(self.snippet())
    }

    fn text(&mut self) -> Option<(&mut String, usize)> {
        match self.focus {
            EditField::Name => Some((&mut self.name, portico_core::snippets::MAX_NAME_CHARS)),
            EditField::Command => Some((&mut self.command, MAX_COMMAND_BYTES)),
            EditField::Description => Some((
                &mut self.description,
                portico_core::snippets::MAX_DESCRIPTION_CHARS,
            )),
            EditField::ScopeValues => Some((&mut self.scope_values, MAX_LINE * 4)),
            EditField::Scope | EditField::Run => None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Snippet> {
        if self.busy {
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => return self.submit(),
            // The command may span lines: `↵` breaks the line there.
            KeyCode::Enter if self.focus == EditField::Command => {
                if self.command.len() < MAX_COMMAND_BYTES {
                    self.command.push('\n');
                }
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(false),
            KeyCode::Left if self.focus == EditField::Scope => self.cycle_scope(false),
            KeyCode::Right | KeyCode::Char(' ') if self.focus == EditField::Scope => {
                self.cycle_scope(true);
            }
            KeyCode::Char(' ') if self.focus == EditField::Run => self.run = !self.run,
            KeyCode::Char('u') if ctrl => {
                if let Some((v, _)) = self.text() {
                    v.clear();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some((v, max)) = self.text() {
                    push_char(v, c, max);
                }
            }
            KeyCode::Backspace => {
                if let Some((v, _)) = self.text() {
                    v.pop();
                }
            }
            _ => {}
        }
        None
    }

    fn paste(&mut self, text: &str) {
        if self.busy {
            return;
        }
        if self.focus == EditField::Command {
            // Several lines are fine here; controls other than tab are not.
            for c in text.replace("\r\n", "\n").chars() {
                if self.command.len() >= MAX_COMMAND_BYTES {
                    break;
                }
                if c == '\n' || c == '\t' || !c.is_control() {
                    self.command.push(c);
                }
            }
        } else if let Some((v, max)) = self.text() {
            paste_line(v, text, max);
        }
    }
}

#[derive(Debug)]
pub(crate) enum SnipMode {
    Browse,
    Fill(FillForm),
    Edit(EditForm),
    ConfirmDelete {
        id: String,
        name: String,
    },
    /// A multi-line paste-only snippet cannot be pasted here: run it?
    NeedsRun {
        rendered: String,
        error: ErrorSummary,
    },
}

#[derive(Debug)]
pub(crate) enum SnipOutcome {
    None,
    Close,
    Reload,
    /// Type `rendered` into the target tab (`run` adds Enter). `values`
    /// are remembered for the next time.
    Send {
        rendered: String,
        run: bool,
        values: Vec<(String, String)>,
    },
    Save(Snippet),
    Delete {
        id: String,
        name: String,
    },
}

#[derive(Debug)]
pub(crate) struct SnippetsPanel {
    pub(crate) all: Load<Vec<Snippet>>,
    /// Host whose snippets are listed (`None`: every snippet).
    pub(crate) alias: Option<String>,
    pub(crate) tags: Vec<String>,
    /// Why snippets cannot be sent right now (no live terminal tab).
    pub(crate) cannot_run: Option<String>,
    pub(crate) filter: String,
    pub(crate) list: ListState,
    pub(crate) mode: SnipMode,
    pub(crate) notice: Option<Notice>,
}

impl SnippetsPanel {
    pub(crate) fn new(
        alias: Option<String>,
        tags: Vec<String>,
        cannot_run: Option<String>,
    ) -> Self {
        Self {
            all: Load::Loading,
            alias,
            tags,
            cannot_run,
            filter: String::new(),
            list: ListState::default(),
            mode: SnipMode::Browse,
            notice: None,
        }
    }

    /// Loading or saving (spinner).
    pub(crate) fn busy(&self) -> bool {
        matches!(self.all, Load::Loading) || matches!(&self.mode, SnipMode::Edit(f) if f.busy)
    }

    pub(crate) fn set(&mut self, result: Result<Vec<Snippet>, ErrorSummary>) {
        self.all = match result {
            Ok(list) => Load::Ready(list),
            Err(e) => Load::Failed(e),
        };
        self.clamp();
    }

    /// Snippets in scope for the target host, matching the filter, best
    /// first.
    pub(crate) fn visible(&self) -> Vec<&Snippet> {
        let Some(all) = self.all.ready() else {
            return Vec::new();
        };
        let mut hits: Vec<(u8, &Snippet)> = all
            .iter()
            .filter(|s| {
                self.alias
                    .as_deref()
                    .is_none_or(|a| s.applies_to_alias(a, &self.tags))
            })
            .filter_map(|s| score(s, &self.filter).map(|rank| (rank, s)))
            .collect();
        // Stable: equal ranks keep the file order.
        hits.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
        hits.into_iter().map(|(_, s)| s).collect()
    }

    /// Number of snippets in scope (before the filter).
    pub(crate) fn in_scope(&self) -> usize {
        self.all.ready().map_or(0, |all| {
            all.iter()
                .filter(|s| {
                    self.alias
                        .as_deref()
                        .is_none_or(|a| s.applies_to_alias(a, &self.tags))
                })
                .count()
        })
    }

    pub(crate) fn selected(&self) -> Option<&Snippet> {
        self.visible().get(self.list.selected()?).copied()
    }

    /// Selects the snippet `id` (after saving it).
    pub(crate) fn select_id(&mut self, id: &str) {
        let index = self.visible().iter().position(|s| s.id == id);
        if index.is_some() {
            self.list.select(index);
        }
    }

    fn clamp(&mut self) {
        let len = self.visible().len();
        let selected = self.list.selected().unwrap_or(0);
        self.list.select((len > 0).then(|| selected.min(len - 1)));
    }

    fn refilter(&mut self) {
        let len = self.visible().len();
        self.list.select((len > 0).then_some(0));
    }

    /// Run the selection: its placeholder form, or straight to the tab.
    fn run_selected(&mut self, remembered: &HashMap<String, String>) -> SnipOutcome {
        let Some(snippet) = self.selected().cloned() else {
            return SnipOutcome::None;
        };
        if let Some(why) = &self.cannot_run {
            self.notice = Some(Notice {
                ok: false,
                message: why.clone(),
                hint: Some("Open a terminal tab on a host, then Ctrl-Space s.".into()),
            });
            return SnipOutcome::None;
        }
        if snippet.placeholders().is_empty() {
            return match snippet.render_checked(&[]) {
                Ok(rendered) => SnipOutcome::Send {
                    rendered,
                    run: snippet.run,
                    values: Vec::new(),
                },
                Err(e) => {
                    self.notice = Some(Notice::error(e.summary()));
                    SnipOutcome::None
                }
            };
        }
        self.mode = SnipMode::Fill(FillForm::new(snippet, remembered));
        SnipOutcome::None
    }

    /// Keys that do something right now.
    pub(crate) fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            SnipMode::Fill(_) => vec![("↵", "send"), ("Tab", "next"), ("Esc", "back")],
            SnipMode::Edit(form) => {
                if form.busy {
                    return Vec::new();
                }
                let mut hints = vec![("Ctrl-S", "save")];
                match form.focus {
                    EditField::Command => hints.push(("↵", "new line")),
                    EditField::Scope => hints.push(("← →", "scope")),
                    EditField::Run => hints.push(("Space", "toggle")),
                    _ => hints.push(("↵", "save")),
                }
                hints.push(("Tab", "next"));
                hints.push(("Esc", "cancel"));
                hints
            }
            SnipMode::ConfirmDelete { .. } => vec![("y", "delete"), ("Esc", "keep")],
            SnipMode::NeedsRun { .. } => vec![("r", "run it"), ("Esc", "cancel")],
            SnipMode::Browse => {
                let mut hints = Vec::new();
                let any = self.selected().is_some();
                if any && self.cannot_run.is_none() {
                    hints.push(("↵", "run"));
                }
                hints.push(("Ctrl-N", "new"));
                if any {
                    hints.push(("Ctrl-E", "edit"));
                    hints.push(("Ctrl-D", "delete"));
                }
                hints.push((
                    "Esc",
                    if self.filter.is_empty() {
                        "close"
                    } else {
                        "clear"
                    },
                ));
                hints
            }
        }
    }

    // One arm per mode; splitting it would scatter the key table.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        remembered: &HashMap<String, String>,
    ) -> SnipOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            SnipMode::Fill(form) => {
                let n = form.values.len().max(1);
                match key.code {
                    KeyCode::Esc => self.mode = SnipMode::Browse,
                    KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % n,
                    KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + n - 1) % n,
                    KeyCode::Enter => match form.preview() {
                        Ok(rendered) => {
                            let run = form.snippet.run;
                            let values = form.values.clone();
                            self.mode = SnipMode::Browse;
                            return SnipOutcome::Send {
                                rendered,
                                run,
                                values,
                            };
                        }
                        Err(e) => form.error = Some(e),
                    },
                    KeyCode::Char('u') if ctrl => {
                        if let Some((_, v)) = form.values.get_mut(form.focus) {
                            v.clear();
                        }
                    }
                    KeyCode::Char(c) if !ctrl => {
                        if let Some((_, v)) = form.values.get_mut(form.focus) {
                            push_char(v, c, MAX_VALUE);
                        }
                        form.error = None;
                    }
                    KeyCode::Backspace => {
                        if let Some((_, v)) = form.values.get_mut(form.focus) {
                            v.pop();
                        }
                        form.error = None;
                    }
                    _ => {}
                }
                return SnipOutcome::None;
            }
            SnipMode::Edit(form) => {
                if key.code == KeyCode::Esc && !form.busy {
                    self.mode = SnipMode::Browse;
                    return SnipOutcome::None;
                }
                return form
                    .handle_key(key)
                    .map_or(SnipOutcome::None, SnipOutcome::Save);
            }
            SnipMode::ConfirmDelete { id, name } => {
                let outcome = match key.code {
                    KeyCode::Char('y' | 'Y') => SnipOutcome::Delete {
                        id: id.clone(),
                        name: name.clone(),
                    },
                    _ => SnipOutcome::None,
                };
                self.mode = SnipMode::Browse;
                return outcome;
            }
            SnipMode::NeedsRun { rendered, .. } => {
                let outcome = match key.code {
                    KeyCode::Char('r' | 'R') | KeyCode::Enter => SnipOutcome::Send {
                        rendered: std::mem::take(rendered),
                        run: true,
                        values: Vec::new(),
                    },
                    _ => SnipOutcome::None,
                };
                self.mode = SnipMode::Browse;
                return outcome;
            }
            SnipMode::Browse => {}
        }
        self.notice = None;
        let len = self.visible().len();
        let current = self.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    return SnipOutcome::Close;
                }
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Enter => return self.run_selected(remembered),
            KeyCode::Down if len > 0 => self.list.select(Some((current + 1).min(len - 1))),
            KeyCode::Up if len > 0 => self.list.select(Some(current.saturating_sub(1))),
            KeyCode::PageDown if len > 0 => self.list.select(Some((current + 5).min(len - 1))),
            KeyCode::PageUp if len > 0 => self.list.select(Some(current.saturating_sub(5))),
            KeyCode::Char('r') if ctrl => return SnipOutcome::Reload,
            KeyCode::Char('n') if ctrl => {
                self.mode = SnipMode::Edit(EditForm::new(self.alias.clone(), self.tags.clone()));
            }
            KeyCode::Char('e') if ctrl => {
                if let Some(snippet) = self.selected() {
                    let form = EditForm::edit(snippet, self.alias.clone(), self.tags.clone());
                    self.mode = SnipMode::Edit(form);
                }
            }
            KeyCode::Char('d') if ctrl => {
                if let Some(snippet) = self.selected() {
                    self.mode = SnipMode::ConfirmDelete {
                        id: snippet.id.clone(),
                        name: snippet.name.clone(),
                    };
                }
            }
            KeyCode::Delete => {
                if let Some(snippet) = self.selected() {
                    self.mode = SnipMode::ConfirmDelete {
                        id: snippet.id.clone(),
                        name: snippet.name.clone(),
                    };
                }
            }
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
            }
            KeyCode::Char(c) if !ctrl => {
                push_char(&mut self.filter, c, 64);
                self.refilter();
            }
            _ => {}
        }
        SnipOutcome::None
    }

    pub(crate) fn paste(&mut self, text: &str) {
        match &mut self.mode {
            SnipMode::Edit(form) => form.paste(text),
            SnipMode::Fill(form) => {
                if let Some((_, v)) = form.values.get_mut(form.focus) {
                    paste_line(v, text, MAX_VALUE);
                }
            }
            SnipMode::Browse => {
                paste_line(&mut self.filter, text, 64);
                self.refilter();
            }
            _ => {}
        }
    }

    /// The engine saved (or refused) the snippet being edited.
    pub(crate) fn saved(&mut self, result: Result<Snippet, ErrorSummary>) -> Option<Snippet> {
        match result {
            Ok(snippet) => {
                self.mode = SnipMode::Browse;
                self.notice = Some(Notice::ok(format!("Saved {}", snippet.name)));
                Some(snippet)
            }
            Err(error) => {
                if let SnipMode::Edit(form) = &mut self.mode {
                    form.busy = false;
                    form.error = Some(error);
                } else {
                    self.notice = Some(Notice::error(error));
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(id: &str, name: &str, command: &str, scope: SnippetScope) -> Snippet {
        Snippet {
            id: id.into(),
            name: name.into(),
            command: command.into(),
            description: None,
            scope,
            run: true,
        }
    }

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn panel() -> SnippetsPanel {
        let mut panel = SnippetsPanel::new(Some("web".into()), vec!["prod".into()], None);
        panel.set(Ok(vec![
            snippet("disk", "Disk usage", "df -h", SnippetScope::Global),
            snippet(
                "logs",
                "Tail logs",
                "journalctl -u {{unit}} -n {{lines}}",
                SnippetScope::Tags(vec!["prod".into()]),
            ),
            snippet(
                "db",
                "DB shell",
                "psql",
                SnippetScope::Hosts(vec!["db".into()]),
            ),
        ]));
        panel
    }

    #[test]
    fn scope_and_filter() {
        let mut panel = panel();
        let names: Vec<&str> = panel.visible().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Disk usage", "Tail logs"], "db is for another host");
        assert_eq!(panel.in_scope(), 2);
        for c in "jour".chars() {
            panel.handle_key(k(KeyCode::Char(c)), &HashMap::new());
        }
        let names: Vec<&str> = panel.visible().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Tail logs"]);
        // Esc clears the filter first, then closes.
        assert!(matches!(
            panel.handle_key(k(KeyCode::Esc), &HashMap::new()),
            SnipOutcome::None
        ));
        assert!(panel.filter.is_empty());
        assert!(matches!(
            panel.handle_key(k(KeyCode::Esc), &HashMap::new()),
            SnipOutcome::Close
        ));
        assert_eq!(
            scope_label(&SnippetScope::Tags(vec!["a".into(), "b".into()])),
            "#a #b"
        );
    }

    #[test]
    fn placeholders_are_asked_and_checked() {
        let mut panel = panel();
        panel.handle_key(k(KeyCode::Down), &HashMap::new());
        let remembered = HashMap::from([("unit".to_owned(), "nginx".to_owned())]);
        assert!(matches!(
            panel.handle_key(k(KeyCode::Enter), &remembered),
            SnipOutcome::None
        ));
        let SnipMode::Fill(form) = &panel.mode else {
            panic!("placeholder form");
        };
        assert_eq!(form.values[0], ("unit".to_owned(), "nginx".to_owned()));
        // `lines` is missing: refused inline.
        panel.handle_key(k(KeyCode::Enter), &remembered);
        let SnipMode::Fill(form) = &panel.mode else {
            panic!("still the form");
        };
        assert_eq!(
            form.error.as_ref().map(|e| e.code.as_str()),
            Some("snippet_missing_value")
        );
        panel.handle_key(k(KeyCode::Tab), &remembered);
        panel.handle_key(k(KeyCode::Char('5')), &remembered);
        let SnipOutcome::Send {
            rendered,
            run,
            values,
        } = panel.handle_key(k(KeyCode::Enter), &remembered)
        else {
            panic!("send");
        };
        assert_eq!(rendered, "journalctl -u nginx -n 5");
        assert!(run);
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn without_a_terminal_running_is_explained() {
        let mut panel =
            SnippetsPanel::new(None, Vec::new(), Some("No terminal tab is open.".into()));
        panel.set(Ok(vec![snippet(
            "disk",
            "Disk usage",
            "df -h",
            SnippetScope::Global,
        )]));
        assert!(!panel.hints().contains(&("↵", "run")));
        assert!(matches!(
            panel.handle_key(k(KeyCode::Enter), &HashMap::new()),
            SnipOutcome::None
        ));
        assert!(panel.notice.as_ref().is_some_and(|n| !n.ok));
    }

    #[test]
    fn editor_validates_then_saves() {
        let mut panel = panel();
        panel.handle_key(ctrl('n'), &HashMap::new());
        assert!(matches!(
            panel.handle_key(ctrl('s'), &HashMap::new()),
            SnipOutcome::None
        ));
        let SnipMode::Edit(form) = &panel.mode else {
            panic!("editor");
        };
        assert_eq!(form.problem(EditField::Name), Some("Name the snippet."));
        for c in "Uptime".chars() {
            panel.handle_key(k(KeyCode::Char(c)), &HashMap::new());
        }
        panel.handle_key(k(KeyCode::Tab), &HashMap::new());
        for c in "uptime".chars() {
            panel.handle_key(k(KeyCode::Char(c)), &HashMap::new());
        }
        panel.handle_key(k(KeyCode::Enter), &HashMap::new());
        panel.handle_key(k(KeyCode::Char('w')), &HashMap::new());
        // Scope: Hosts, prefilled with the current host.
        panel.handle_key(k(KeyCode::Tab), &HashMap::new());
        panel.handle_key(k(KeyCode::Tab), &HashMap::new());
        panel.handle_key(k(KeyCode::Right), &HashMap::new());
        let SnipOutcome::Save(saved) = panel.handle_key(ctrl('s'), &HashMap::new()) else {
            panic!("save");
        };
        assert_eq!(saved.command, "uptime\nw");
        assert_eq!(saved.scope, SnippetScope::Hosts(vec!["web".into()]));
        assert!(saved.id.is_empty());
    }

    #[test]
    fn paste_only_multi_line_needs_bracketed_paste() {
        let refused = snippet_bytes("a\nb", false, false);
        assert_eq!(
            refused.err().map(|e| e.code),
            Some("snippet_needs_paste_mode".to_owned())
        );
        assert_eq!(
            snippet_bytes("a\nb", true, false).ok(),
            Some(b"a\rb\r".to_vec())
        );
        assert!(snippet_bytes("a\nb", false, true).is_ok());
        assert_eq!(snippet_bytes("ls", false, false).ok(), Some(b"ls".to_vec()));
    }

    #[test]
    fn delete_asks_first() {
        let mut panel = panel();
        panel.handle_key(ctrl('d'), &HashMap::new());
        assert!(matches!(panel.mode, SnipMode::ConfirmDelete { .. }));
        assert!(matches!(
            panel.handle_key(k(KeyCode::Char('y')), &HashMap::new()),
            SnipOutcome::Delete { .. }
        ));
    }
}
