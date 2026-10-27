//! App glue for phase 4: favorites and host metadata, recent hosts, the
//! debug panel, settings (theme and keymap). Engine calls run in tasks and
//! come back as [`Action`]s; panel state lives in `edit_host`, `debug` and
//! `host_list` (pure, tested there).

use std::io::Write;

use crossterm::event::KeyEvent;
use portico_core::settings::Settings;
use portico_core::{EngineEvent, ErrorSummary};
use portico_theme::{AUTO, Theme};

use crate::action::Action;
use crate::app::{App, Focus, Overlay};
use crate::debug::{DebugOutcome, DebugPanel};
use crate::dialog::{ConfirmAction, ConfirmDialog, ConfirmOutcome, Dialog};
use crate::edit_host::{EditField, EditHostPanel, EditOutcome, EditRequest, field_of};
use crate::host_list::RECENT_FETCHED;
use crate::keymap::Keymap;
use crate::keys::Notice;

/// Common title of the settings warning (same words as the GUI):
/// `settings.toml: 2 entries ignored`; `None` when nothing was ignored.
pub(crate) fn settings_title(ignored: usize) -> Option<String> {
    match ignored {
        0 => None,
        1 => Some("settings.toml: 1 entry ignored".to_owned()),
        n => Some(format!("settings.toml: {n} entries ignored")),
    }
}

/// Spawns `fut` and sends its result back as an action.
fn spawn<F>(app: &App, fut: F)
where
    F: Future<Output = Action> + Send + 'static,
{
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(fut.await);
    });
}

impl App {
    pub(crate) fn spawn_load_recent(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::RecentLoaded(engine.recent(RECENT_FETCHED).await.map_err(|e| e.summary()))
        });
    }

    /// Phase 4 actions (the rest is in `App::update` and `update_panels`).
    pub(crate) fn update_meta(&mut self, action: Action) {
        match action {
            Action::ToggleFavorite => self.toggle_favorite(),
            Action::EditHost => self.edit_host(),
            Action::EditKey(key) => self.edit_key(key),
            // Only the favorite toggle saves this way: a short confirmation.
            Action::MetaSaved { alias, result } => match result {
                Ok(meta) => {
                    self.notify(
                        if meta.favorite {
                            "Added to favorites"
                        } else {
                            "Removed from favorites"
                        }
                        .into(),
                    );
                    self.set_meta(&alias, &meta);
                }
                Err(e) => self.notify_error(e),
            },
            Action::HostEdited { alias, result } => match result {
                Ok(meta) => {
                    self.set_meta(&alias, &meta);
                    if self.edit_panel.as_ref().is_some_and(|p| p.alias == alias) {
                        self.edit_panel = None;
                        self.overlay = None;
                    }
                    self.notify(format!("Saved {alias} · hosts.toml"));
                }
                Err((field, e)) => match &mut self.edit_panel {
                    Some(panel) if panel.alias == alias => panel.refused(field, e),
                    _ => self.notify_error(e),
                },
            },
            Action::RecentLoaded(Ok(recent)) => {
                // On the first row (launch): stay on the first row, which
                // is now the most recent host.
                let first = self.entries.iter().position(|e| e.host().is_some());
                let on_first = self.list.selected() == first;
                self.recent = recent;
                self.rebuild_entries();
                if on_first {
                    self.select_first_host();
                }
            }
            Action::RecentLoaded(Err(e)) | Action::RecentEdited(Err(e)) => self.notify_error(e),
            Action::ForgetRecent => self.forget_recent(),
            Action::AskClearRecent => {
                if self.recent.is_empty() {
                    self.notify("No recent hosts to clear".into());
                } else {
                    self.dialogs.push_back(Dialog::Confirm(ConfirmDialog {
                        title: "Clear recent hosts?".into(),
                        subject: None,
                        body:
                            "The Recent section empties. Hosts, tags and groups stay as they are."
                                .into(),
                        confirm: "clear",
                        action: ConfirmAction::ClearRecent,
                        armed: None,
                        answers: crate::dialog::Answers::YesNo,
                        note: None,
                    }));
                }
            }
            Action::OpenDebug => self.open_debug(),
            Action::DebugKey(key) => self.debug_key(key),
            Action::EffectiveLoaded { alias, result } => {
                if let Some(panel) = &mut self.debug_panel {
                    panel.set_config(alias, result);
                }
            }
            Action::SettingsLoaded(result) => self.apply_settings(result),
            Action::SystemAppearance(appearance) => self.theme.system = appearance,
            Action::ThemeSaved(Err(e)) => self.notify_error(ErrorSummary {
                message: format!("theme not saved: {}", e.message),
                ..e
            }),
            other => self.update_recording(other),
        }
    }

    /// Engine events of phase 4 (others are handled in `on_engine_event`).
    pub(crate) fn on_meta_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::HostMeta { alias, meta } => self.set_meta(&alias, &meta),
            EngineEvent::RecentChanged => self.spawn_load_recent(),
            EngineEvent::HostsChanged => self.on_hosts_changed(),
            EngineEvent::Recording {
                terminal, state, ..
            } => self.on_recording_event(terminal, state),
            EngineEvent::Log(record) => {
                if let Some(panel) = &mut self.debug_panel {
                    panel.push(record);
                }
            }
            _ => {}
        }
    }

    /// Updates a host's metadata in place (no inventory reload).
    fn set_meta(&mut self, alias: &str, meta: &portico_core::hosts::HostMeta) {
        let Some(inv) = &mut self.inventory else {
            return;
        };
        let mut changed = false;
        for host in inv.hosts.iter_mut().filter(|h| h.alias == alias) {
            if host.meta != *meta {
                host.meta = meta.clone();
                changed = true;
            }
        }
        if changed {
            self.rebuild_entries();
        }
    }

    // ── favorites and edit form ────────────────────────────────────────────

    fn toggle_favorite(&mut self) {
        let Some(host) = self.selected_host() else {
            return;
        };
        let (alias, on) = (host.alias.clone(), !host.meta.favorite);
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine
                .set_favorite(&alias, on)
                .await
                .map_err(|e| e.summary());
            Action::MetaSaved { alias, result }
        });
    }

    /// Tags, group and notes of `alias` (any origin: they live in
    /// `hosts.toml`).
    pub(crate) fn open_edit_meta(&mut self, alias: &str) {
        let Some(host) = self
            .inventory
            .as_ref()
            .and_then(|i| i.hosts.iter().find(|h| h.alias == alias))
        else {
            self.notify("Select a host to edit".into());
            return;
        };
        let panel = EditHostPanel::new(host.alias.clone(), &host.meta);
        self.close_panels();
        self.edit_panel = Some(panel);
        self.overlay = Some(Overlay::Edit);
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.edit_panel else {
            return;
        };
        match panel.handle_key(key) {
            EditOutcome::None => {}
            EditOutcome::Close => {
                self.edit_panel = None;
                self.overlay = None;
            }
            EditOutcome::Save(request) => self.save_edit(request),
        }
    }

    /// Writes the changed fields in turn; the first refusal stops and is
    /// reported on its field.
    fn save_edit(&self, request: EditRequest) {
        let engine = self.engine.clone();
        spawn(self, async move {
            let alias = request.alias.clone();
            let refused = |field: EditField, e: portico_core::Error| {
                let summary = e.summary();
                (field_of(&summary.code).unwrap_or(field), summary)
            };
            let mut last = None;
            let result = async {
                if let Some(tags) = request.tags {
                    last = Some(
                        engine
                            .set_tags(&alias, tags)
                            .await
                            .map_err(|e| refused(EditField::Tags, e))?,
                    );
                }
                if let Some(group) = request.group {
                    last = Some(
                        engine
                            .set_group(&alias, group)
                            .await
                            .map_err(|e| refused(EditField::Group, e))?,
                    );
                }
                if let Some(notes) = request.notes {
                    last = Some(
                        engine
                            .set_notes(&alias, notes)
                            .await
                            .map_err(|e| refused(EditField::Notes, e))?,
                    );
                }
                last.ok_or_else(|| {
                    (
                        EditField::Tags,
                        ErrorSummary {
                            code: "nothing_to_save".into(),
                            message: "nothing changed".into(),
                            hint: None,
                        },
                    )
                })
            }
            .await;
            Action::HostEdited { alias, result }
        });
    }

    // ── recent hosts ───────────────────────────────────────────────────────

    fn forget_recent(&mut self) {
        let Some(alias) = self.selected_host().map(|h| h.alias.clone()) else {
            return;
        };
        if !self.selected_is_recent() {
            let key = self.keymap.shown("hosts.forget_recent", 1);
            self.notify(format!(
                "{key} removes a host of the Recent section: select it there"
            ));
            return;
        }
        let engine = self.engine.clone();
        self.notify(format!("Removed {alias} from recent hosts"));
        spawn(self, async move {
            Action::RecentEdited(
                engine
                    .forget_recent(&alias)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.summary()),
            )
        });
    }

    pub(crate) fn confirm_key(&mut self, key: KeyEvent) {
        let Some(Dialog::Confirm(dialog)) = self.dialogs.front() else {
            return;
        };
        let action = dialog.action.clone();
        match dialog.handle_key(key) {
            ConfirmOutcome::Confirm => {
                self.dialogs.pop_front();
                match action {
                    ConfirmAction::ClearRecent => {
                        let engine = self.engine.clone();
                        self.notify("Recent hosts cleared".into());
                        spawn(self, async move {
                            Action::RecentEdited(
                                engine.clear_recent().await.map_err(|e| e.summary()),
                            )
                        });
                    }
                    ConfirmAction::StartRecording(terminal) => self.start_recording(terminal),
                    ConfirmAction::DeleteHost(alias) => self.delete_host(alias),
                    ConfirmAction::UpdateQuestion => self.save_update_check(true),
                    ConfirmAction::SetUpdateCheck(on) => self.save_update_check(on),
                    ConfirmAction::CopyUpdate(text) => self.copy_update(&text),
                }
            }
            ConfirmOutcome::Decline => {
                self.dialogs.pop_front();
                if action == ConfirmAction::UpdateQuestion {
                    self.save_update_check(false);
                }
            }
            ConfirmOutcome::Cancel => {
                self.dialogs.pop_front();
            }
            ConfirmOutcome::None => {}
        }
    }

    // ── debug panel ────────────────────────────────────────────────────────

    fn open_debug(&mut self) {
        self.prefix = false;
        // From a terminal tab: its session. From the list: the newest
        // session of the selected host.
        let sessions = self.engine.sessions_with_logs();
        let from_terminal = matches!(self.focus, Focus::Terminal | Focus::Files)
            .then(|| {
                self.focused_slot()
                    .and_then(crate::app_panes::PaneSlot::session)
            })
            .flatten();
        let host = self.selected_host().map(|h| h.alias.clone());
        let preselect = from_terminal.or_else(|| {
            let alias = host.as_deref()?;
            sessions
                .iter()
                .rev()
                .find(|s| s.alias == alias && !s.probe)
                .or_else(|| sessions.iter().rev().find(|s| s.alias == alias))
                .map(|s| s.id)
        });
        let panel = DebugPanel::new(sessions, preselect, host);
        self.close_panels();
        self.debug_panel = Some(panel);
        self.overlay = Some(Overlay::Debug);
        self.load_debug_logs();
    }

    pub(crate) fn refresh_debug_sessions(&mut self) {
        if self.debug_panel.is_none() {
            return;
        }
        let sessions = self.engine.sessions_with_logs();
        let before = self
            .debug_panel
            .as_ref()
            .and_then(|p| p.session().map(|s| s.id));
        if let Some(panel) = &mut self.debug_panel {
            panel.set_sessions(sessions);
        }
        let after = self
            .debug_panel
            .as_ref()
            .and_then(|p| p.session().map(|s| s.id));
        if before != after {
            self.load_debug_logs();
        }
    }

    /// The selected session's logs (a short in-memory read), and its
    /// config when the Config tab shows.
    fn load_debug_logs(&mut self) {
        let Some(panel) = &mut self.debug_panel else {
            return;
        };
        let logs = panel
            .session()
            .and_then(|s| self.engine.session_logs(s.id).ok())
            .unwrap_or_default();
        panel.set_logs(logs);
        if let Some(alias) = panel.needs_config() {
            self.load_config(alias);
        }
    }

    fn load_config(&mut self, alias: String) {
        if let Some(panel) = &mut self.debug_panel {
            panel.config = Some((alias.clone(), crate::keys::Load::Loading));
        }
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine
                .effective_config(&alias)
                .await
                .map_err(|e| e.summary());
            Action::EffectiveLoaded { alias, result }
        });
    }

    /// `?` from the session logs: the help over the panel, on the page
    /// that has its keys.
    pub(crate) fn open_help(&mut self) {
        self.help.over_panel = true;
        self.help.page = 0;
    }

    fn debug_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.debug_panel else {
            return;
        };
        match panel.handle_key(key) {
            DebugOutcome::None => {}
            DebugOutcome::Close => {
                self.debug_panel = None;
                self.overlay = None;
            }
            DebugOutcome::Selected => self.load_debug_logs(),
            DebugOutcome::LoadConfig(alias) => self.load_config(alias),
            DebugOutcome::Refresh => {
                if let Some(panel) = &mut self.debug_panel {
                    panel.config = None;
                }
                let sessions = self.engine.sessions_with_logs();
                if let Some(panel) = &mut self.debug_panel {
                    panel.set_sessions(sessions);
                }
                self.load_debug_logs();
            }
            DebugOutcome::Help => self.open_help(),
            DebugOutcome::Copy { text, lines, total } => {
                self.osc52(&text);
                let config = self
                    .debug_panel
                    .as_ref()
                    .is_some_and(|p| p.tab == crate::debug::DebugTab::Config);
                let message = match (config, lines < total) {
                    (true, true) => format!(
                        "Copied {lines} of {total} options (OSC 52); sensitive ones are never copied"
                    ),
                    (true, false) => format!("Copied {lines} options (OSC 52)"),
                    (false, true) => {
                        format!("Copied the last {lines} of {total} records (OSC 52)")
                    }
                    (false, false) => format!("Copied {lines} records (OSC 52)"),
                };
                if let Some(panel) = &mut self.debug_panel {
                    panel.notice = Some(Notice {
                        ok: true,
                        message,
                        hint: Some(
                            "Nothing in the clipboard? Your terminal ignores OSC 52: select the lines with the mouse (Shift-drag)."
                                .into(),
                        ),
                    });
                }
            }
        }
    }

    /// Sends `text` to the local terminal's clipboard (OSC 52).
    pub(crate) fn osc52(&self, text: &str) {
        // A pending clearing of a copied passphrase no longer applies.
        self.clipboard_secret
            .set(self.clipboard_secret.get().wrapping_add(1));
        if self.clipboard {
            let sequence = format!(
                "\x1b]52;c;{}\x07",
                crate::app_panels::base64(text.as_bytes())
            );
            let mut out = std::io::stdout();
            let _ = out.write_all(sequence.as_bytes());
            let _ = out.flush();
        }
    }

    /// [`Self::osc52`] for a secret (a generated passphrase, or `""` to
    /// empty the clipboard): the encoded text and the sequence live in
    /// pre-sized `Zeroizing` buffers, wiped once written.
    pub(crate) fn osc52_secret(&self, text: &str) {
        self.clipboard_secret
            .set(self.clipboard_secret.get().wrapping_add(1));
        if !self.clipboard {
            return;
        }
        let encoded = zeroize::Zeroizing::new(crate::app_panels::base64(text.as_bytes()));
        let mut sequence = zeroize::Zeroizing::new(String::with_capacity(encoded.len() + 8));
        sequence.push_str("\x1b]52;c;");
        sequence.push_str(&encoded);
        sequence.push('\x07');
        let mut out = std::io::stdout();
        let _ = out.write_all(sequence.as_bytes());
        let _ = out.flush();
    }

    // ── settings ───────────────────────────────────────────────────────────

    /// Theme and keymap from `settings.toml`; problems become one toast.
    fn apply_settings(&mut self, result: Result<Settings, ErrorSummary>) {
        let settings = match result {
            Ok(s) => s,
            // Portico goes on with its defaults: a warning, not an error.
            Err(e) => {
                let detail = match &e.hint {
                    Some(hint) => format!("{} {hint}", crate::ui::fmt::sentence(&e.message)),
                    None => crate::ui::fmt::sentence(&e.message),
                };
                self.notify_warning("settings.toml ignored".into(), Some(detail));
                return;
            }
        };
        let mut problems = Vec::new();
        // `auto` when absent; an unknown id warns and falls back to it.
        self.theme.fixed = None;
        if let Some(id) = settings.theme.as_deref().filter(|id| *id != AUTO) {
            self.theme.fixed = Theme::by_id(id);
            if self.theme.fixed.is_none() {
                problems.push(format!(
                    "Unknown theme “{}”: using Auto",
                    crate::ui::fmt::truncate(id, 24),
                ));
            }
        }
        self.theme.dark = Theme::default_theme();
        if let Some(id) = &settings.dark_theme {
            match Theme::by_id(id).filter(|t| !t.is_light()) {
                Some(dark) => self.theme.dark = dark,
                None => problems.push(format!(
                    "dark_theme “{}” is not a dark theme: using {}",
                    crate::ui::fmt::truncate(id, 24),
                    Theme::default_theme().name
                )),
            }
        }
        let (keymap, ignored) = Keymap::with_overrides(&settings.keymap.tui);
        self.keymap = keymap;
        problems.extend(ignored);
        if let Some(title) = settings_title(problems.len()) {
            self.notify_warning(title, Some(problems.join(" · ")));
        }
    }

    /// `t`: next theme, saved to `settings.toml` (only the theme changes:
    /// another UI's settings are kept).
    pub(crate) fn cycle_theme(&mut self) {
        self.theme = self.theme.next();
        self.notify(format!("Theme · {}", self.theme.label()));
        let (theme, dark) = self.theme.to_settings();
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine
                .set_theme(Some(theme.to_owned()), Some(dark.to_owned()))
                .await
                .map(|_| ())
                .map_err(|e| e.summary());
            Action::ThemeSaved(result)
        });
    }
}

/// Phase 4 screens at 80×24 and their logic.
#[cfg(test)]
mod render_tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use portico_core::effective::{EffectiveConfig, EffectiveOption, OptionOrigin, RouteHop};
    use portico_core::engine::HostInventory;
    use portico_core::hosts::HostMeta;
    use portico_core::ids::SessionId;
    use portico_core::logs::{LogLevel, LogRecord, SessionLogInfo};
    use portico_core::recent::RecentHost;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use time::OffsetDateTime;

    use super::*;
    use crate::app::test_app;
    use crate::host_list::test_host;

    fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|f| crate::ui::render(app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    fn check(lines: &[String], needles: &[&str]) {
        let text = lines.join("\n");
        for n in needles {
            assert!(text.contains(n), "missing `{n}` in:\n{text}");
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// web and db in "prod", lab without group; lab is recent.
    fn hosts_app() -> App {
        let mut app = test_app();
        app.update(Action::HostsLoaded(Ok(HostInventory {
            hosts: vec![
                test_host("web", Some("prod")),
                test_host("db", Some("prod")),
                test_host("lab", None),
            ],
            warnings: Vec::new(),
        })));
        app.update(Action::RecentLoaded(Ok(vec![RecentHost {
            alias: "lab".into(),
            last: OffsetDateTime::UNIX_EPOCH,
            count: 3,
        }])));
        app
    }

    #[test]
    fn long_host_target_ends_with_an_ellipsis() {
        let mut app = test_app();
        let mut long = test_host("api", None);
        long.resolved.hostname = "api-gateway.internal.eu-west-3.example.com".into();
        long.meta.tags = vec!["prod".into()];
        let mut short = test_host("db", None);
        short.meta.tags = vec!["sql".into()];
        app.update(Action::HostsLoaded(Ok(HostInventory {
            hosts: vec![long, short],
            warnings: Vec::new(),
        })));
        let lines = screen(&mut app, 80, 24);
        let row = lines
            .iter()
            .find(|l| l.contains("api-gateway"))
            .unwrap_or_else(|| panic!("{}", lines.join("\n")));
        assert!(row.contains('…'), "cut with an ellipsis: {row}");
        assert!(!row.contains("#prod"), "no tag after a cut target: {row}");
        // The ellipsis is the last character before the panel's border.
        let after = row.split_once('…').unwrap().1;
        assert!(after.trim_start().starts_with('│'), "{row}");
        let row = lines.iter().find(|l| l.contains("● db")).unwrap();
        assert!(row.contains("db  db #sql") && !row.contains('…'), "{row}");
    }

    #[test]
    fn light_theme_dims_the_screen_under_a_modal() {
        use crate::theme::c;
        let bg_at = |app: &mut App, x: u16, y: u16| {
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|f| crate::ui::render(app, f)).unwrap();
            terminal.backend().buffer().cell((x, y)).unwrap().bg
        };
        for (id, dims) in [("marble", true), ("obsidian", false)] {
            let mut app = hosts_app();
            app.update(Action::SettingsLoaded(Ok(Settings {
                theme: Some(id.into()),
                ..Settings::default()
            })));
            let p = app.styles().theme.palette;
            // A cell of the details panel, far from the centred modal.
            let before = bg_at(&mut app, 78, 20);
            assert!([c(p.bg_base), c(p.bg_surface)].contains(&before), "{id}");
            app.update(Action::AskClearRecent);
            assert!(!app.dialogs.is_empty());
            let under = if dims { c(p.bg_elevated) } else { before };
            assert_eq!(bg_at(&mut app, 78, 20), under, "{id}");
        }
    }

    #[test]
    fn host_list_sections_and_selection() {
        let mut app = hosts_app();
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["R E C E N T", "P R O D", "O T H E R"]);
        assert_eq!(app.selected_host().map(|h| h.alias.as_str()), Some("lab"));
        assert!(app.selected_is_recent());
        // On a Recent row, the status bar offers to forget it.
        check(&lines, &[" x  forget"]);
        // Moving down steps over the PROD title.
        app.update(Action::SelectNext);
        assert_eq!(app.selected_host().map(|h| h.alias.as_str()), Some("web"));
        assert!(!app.selected_is_recent());
        app.update(Action::SelectLast);
        assert_eq!(app.selected_host().map(|h| h.alias.as_str()), Some("lab"));
        app.update(Action::SelectNext);
        assert_eq!(app.selected_host().map(|h| h.alias.as_str()), Some("lab"));
        // `x` outside Recent explains instead of acting.
        app.update(Action::SelectPrev);
        app.update(Action::ForgetRecent);
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.message.contains("Recent"))
        );
        // A search makes the list flat.
        for c in "d".chars() {
            app.update(Action::SearchInput(c));
        }
        assert!(app.entries.iter().all(|e| e.host().is_some()));
    }

    #[test]
    fn metadata_events_update_the_list_in_place() {
        let mut app = hosts_app();
        app.update(Action::SelectNext);
        app.update(Action::Engine(EngineEvent::HostMeta {
            alias: "web".into(),
            meta: HostMeta {
                favorite: true,
                group: Some("prod".into()),
                notes: Some("Primary web node\nPatch on Tuesdays".into()),
                ..HostMeta::default()
            },
        }));
        // The selection stays on web.
        assert_eq!(app.selected_host().map(|h| h.alias.as_str()), Some("web"));
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["web ★", "Notes", "Patch on Tuesdays", "Group"]);
        // Moved out of its group: the list follows.
        app.update(Action::Engine(EngineEvent::HostMeta {
            alias: "db".into(),
            meta: HostMeta::default(),
        }));
        let lines = screen(&mut app, 80, 24);
        let text = lines.join("\n");
        let prod = text.find("P R O D").unwrap_or(0);
        let other = text.find("O T H E R").unwrap_or(0);
        let db = text.rfind("● db").unwrap_or(0);
        assert!(prod < other && other < db, "{text}");
    }

    #[test]
    fn edit_form_fits_80x24_with_chips_and_inline_errors() {
        let mut app = hosts_app();
        app.update(Action::EditHost);
        // A ~/.ssh/config host: the read-only view, `t` its tags and notes.
        app.update(Action::HostFormKey(key(KeyCode::Char('t'))));
        assert_eq!(app.overlay, Some(Overlay::Edit));
        for c in "#prod, eu".chars() {
            app.update(Action::EditKey(key(KeyCode::Char(c))));
        }
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Edit lab",
                "Tags",
                " #prod ",
                " #eu ",
                "Group",
                "Notes",
                "Portico never edits ~/.ssh/config: these live in hosts.toml",
                " EDIT ",
            ],
        );
        app.update(Action::HostEdited {
            alias: "lab".into(),
            result: Err((
                EditField::Group,
                ErrorSummary {
                    code: "host_group".into(),
                    message: "invalid group: longer than 64 characters".into(),
                    hint: Some("A group is one line of at most 64 visible characters.".into()),
                },
            )),
        });
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["longer than 64 characters", "at most 64 visible"]);
        assert_eq!(
            app.edit_panel.as_ref().map(|p| p.focus),
            Some(EditField::Group)
        );
        // Saved: the panel closes and says where.
        app.update(Action::HostEdited {
            alias: "lab".into(),
            result: Ok(HostMeta {
                tags: vec!["prod".into()],
                ..HostMeta::default()
            }),
        });
        assert_eq!(app.overlay, None);
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.message.contains("hosts.toml"))
        );
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["#prod"]);
    }

    #[tokio::test]
    async fn debug_panel_empty_logs_and_config() {
        let mut app = hosts_app();
        app.update(Action::OpenDebug);
        assert_eq!(app.overlay, Some(Overlay::Debug));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Session logs",
                "No session yet.",
                " Logs │ Effective config ",
                "No session selected.",
                " LOGS ",
            ],
        );
        let lines = screen(&mut app, 110, 28);
        check(&lines, &["Sessions", "No session yet."]);
        app.update(Action::DebugKey(key(KeyCode::Tab)));
        let option = |key: &str, value: &str, origin: OptionOrigin| EffectiveOption {
            key: key.into(),
            values: vec![value.into()],
            origin,
            sensitive: portico_core::effective::SENSITIVE_KEYWORDS.contains(&key),
        };
        app.update(Action::EffectiveLoaded {
            alias: "lab".into(),
            result: Ok(EffectiveConfig {
                alias: "lab".into(),
                options: vec![
                    option(
                        "hostname",
                        "lab.internal",
                        OptionOrigin::Config {
                            file: "/etc/ssh/ssh_config".into(),
                            line: 12,
                        },
                    ),
                    option(
                        "proxycommand",
                        "sshpass -p hunter2 ssh",
                        OptionOrigin::Default,
                    ),
                    option("user", "alice", OptionOrigin::Command),
                    option("stricthostkeychecking", "ask", OptionOrigin::Default),
                ],
                route: vec![
                    RouteHop {
                        alias: "bastion".into(),
                        user: "ops".into(),
                        hostname: "b.example.com".into(),
                        port: 2222,
                    },
                    RouteHop {
                        alias: "lab".into(),
                        user: "alice".into(),
                        hostname: "lab.internal".into(),
                        port: 22,
                    },
                ],
                route_error: None,
                warnings: Vec::new(),
            }),
        });
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Route",
                "You → bastion → lab",
                "1/2",
                "ops@b.example.com:2222",
                "hostname",
                "/etc/ssh/ssh_config:12",
                "default",
                "at connect",
                "•••  hidden · s reveals · never copied",
                " s  reveal",
            ],
        );
        assert!(!lines.join("\n").contains("hunter2"));
        app.update(Action::DebugKey(key(KeyCode::Char('s'))));
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["hunter2", "· never copied"]);
    }

    fn record(session: portico_core::ids::SessionId, level: LogLevel, n: usize) -> LogRecord {
        LogRecord {
            at: OffsetDateTime::UNIX_EPOCH,
            level,
            session: Some(session),
            target: "t".into(),
            message: format!("message {n} {}", "word ".repeat(n % 3 * 12)),
        }
    }

    /// The debug panel on three sessions, the first one's records loaded.
    fn logs_app(records: Vec<LogRecord>) -> App {
        let mut app = hosts_app();
        let ids: Vec<_> = (0..3).map(|_| SessionId::next()).collect();
        let infos = ids
            .iter()
            .zip(["lab", "web", "db"])
            .map(|(id, alias)| SessionLogInfo {
                id: *id,
                alias: alias.into(),
                probe: false,
                started: OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(22 * 60 + 23),
                ended: None,
                outcome: None,
            })
            .collect();
        // The oldest (lab) selected: newest first, it is the last of three.
        let mut panel = crate::debug::DebugPanel::new(infos, ids.first().copied(), None);
        let first = panel.session().map(|s| s.id);
        panel.set_logs(
            records
                .into_iter()
                .map(|r| LogRecord {
                    session: first,
                    ..r
                })
                .collect(),
        );
        app.debug_panel = Some(panel);
        app.overlay = Some(Overlay::Debug);
        app
    }

    fn fg_at(app: &mut App, cols: u16, rows: u16, needle: &str) -> Option<ratatui::style::Color> {
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|f| crate::ui::render(app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..rows {
            let line: String = (0..cols)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect();
            if let Some(byte) = line.find(needle) {
                let x = line.get(..byte).map_or(0, |p| p.chars().count());
                let x = u16::try_from(x).unwrap();
                return buffer.cell((x, y)).map(|c| c.fg);
            }
        }
        None
    }

    #[test]
    fn log_levels_keep_saturated_colour_for_meaning() {
        let any = SessionId::next();
        let rec = |level, message: &str| LogRecord {
            message: message.into(),
            ..record(any, level, 0)
        };
        let mut app = logs_app(vec![
            rec(LogLevel::Debug, "kex done"),
            rec(LogLevel::Info, "route ok"),
            rec(LogLevel::Warn, "slow agent"),
            rec(LogLevel::Error, "no method left"),
        ]);
        let s = app.styles();
        let (p, st) = (s.theme.palette, s.theme.status);
        let c = crate::theme::c;
        assert_eq!(fg_at(&mut app, 110, 28, "DEBUG"), Some(c(p.text_muted)));
        assert_eq!(fg_at(&mut app, 110, 28, "INFO "), Some(c(p.text_secondary)));
        assert_ne!(fg_at(&mut app, 110, 28, "INFO "), Some(c(st.connected)));
        assert_eq!(fg_at(&mut app, 110, 28, "WARN "), Some(c(st.warning)));
        assert_eq!(fg_at(&mut app, 110, 28, "ERROR"), Some(c(st.error)));
        // Messages are body text; only an ERROR's message is emphasised.
        let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
        terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let modifier_of = |needle: &str| {
            (0..28u16).find_map(|y| {
                let line: String = (0..110u16)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect();
                let x = line.find(needle)?;
                let x = u16::try_from(line.get(..x)?.chars().count()).ok()?;
                buffer.cell((x, y)).map(|c| (c.fg, c.modifier))
            })
        };
        let bold = ratatui::style::Modifier::BOLD;
        let (fg, m) = modifier_of("route ok").unwrap();
        assert_eq!(fg, c(p.text_primary));
        assert!(!m.contains(bold));
        let (_, m) = modifier_of("no method left").unwrap();
        assert!(m.contains(bold));
    }

    #[test]
    fn logs_start_on_a_record_and_follow_with_g() {
        let any = SessionId::next();
        // Records of 1, 2 and 3 rows at 80 columns.
        let mut app = logs_app((0..40).map(|n| record(any, LogLevel::Info, n)).collect());
        for (cols, rows) in [(80, 24), (110, 28)] {
            for scroll in [0, 1, 3, 7] {
                if let Some(panel) = &mut app.debug_panel {
                    panel.scroll = scroll;
                }
                let lines = screen(&mut app, cols, rows);
                // The first non-blank row under the tabs is a record's head
                // (read from the tabs' column: the session column is left).
                let tabs = lines.iter().position(|l| l.contains(" Logs │ ")).unwrap();
                let col = lines
                    .get(tabs)
                    .and_then(|l| {
                        l.find(" Logs │ ")
                            .map(|b| l.get(..b).unwrap().chars().count())
                    })
                    .unwrap();
                let first = lines
                    .iter()
                    .skip(tabs + 2)
                    .map(|l| l.chars().skip(col).collect::<String>())
                    .find(|l| !l.trim_matches(|c: char| c == '│' || c == ' ').is_empty())
                    .unwrap();
                assert!(
                    first.trim_start().starts_with("00:00:00 INFO  t: message"),
                    "{first}\n{}",
                    lines.join("\n")
                );
                if scroll > 0 {
                    let what = if scroll == 1 { "record" } else { "records" };
                    check(&lines, &[&format!("↓ {scroll} newer {what} · G follows")]);
                    check(&lines, &[" G  follow"]);
                }
            }
        }
        app.update(Action::DebugKey(key(KeyCode::Char('G'))));
        assert_eq!(app.debug_panel.as_ref().map(|p| p.scroll), Some(0));
        // A filter that hides everything says so, with the word "record".
        app.update(Action::DebugKey(key(KeyCode::Char('/'))));
        for c in "zzz".chars() {
            app.update(Action::DebugKey(key(KeyCode::Char(c))));
        }
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["No record matches."]);
    }

    #[test]
    fn narrow_logs_use_a_session_selector() {
        let mut app = logs_app(Vec::new());
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "◂ ● lab · 22:23 (3/3) ▸",
                "No record yet for this session.",
                " [ ]  session",
            ],
        );
        assert!(!lines.join("\n").contains("Sessions"), "no column");
        // One column of margin inside the border.
        let row = lines.iter().find(|l| l.contains("◂ ●")).unwrap();
        assert!(row.starts_with(" │ ◂"), "{row}");
        app.update(Action::DebugKey(key(KeyCode::Char('['))));
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["◂ ● web · 22:23 (2/3) ▸"]);
        app.update(Action::DebugKey(key(KeyCode::Left)));
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["(1/3)"]);
        // Wide: the column, and the arrows move in it.
        let lines = screen(&mut app, 110, 28);
        check(&lines, &["Sessions", " ↑↓  session", "Effective config"]);
        assert!(!lines.join("\n").contains("(1/3)"));
    }

    #[test]
    fn section_headers_fit_80x24_with_their_count() {
        let mut app = test_app();
        app.update(Action::HostsLoaded(Ok(HostInventory {
            hosts: vec![
                test_host("lab", Some("Production Europe West")),
                test_host("broken", Some("Production Europe West")),
                test_host("db", Some("Staging")),
            ],
            warnings: Vec::new(),
        })));
        let lines = screen(&mut app, 80, 24);
        // Too long spaced: plain capitals; the count against the border.
        let header = lines
            .iter()
            .find(|l| l.contains("PRODUCTION"))
            .unwrap_or_else(|| panic!("{}", lines.join("\n")));
        assert!(header.contains("PRODUCTION EUROPE W"), "{header}");
        assert!(header.contains("2 │"), "count right-aligned: {header}");
        check(&lines, &["S T A G I N G"]);
        let staging = lines.iter().find(|l| l.contains("S T A G")).unwrap();
        assert!(staging.contains("1 │"), "{staging}");
        // Wide enough: spaced capitals.
        let lines = screen(&mut app, 140, 30);
        check(&lines, &["P R O D U C T I O N   E U R O P E   W E S T"]);
    }

    #[tokio::test]
    async fn details_favorite_masked_commands_and_failure_remedies() {
        let mut app = hosts_app();
        // lab: a ProxyCommand with a password in it.
        if let Some(inv) = &mut app.inventory {
            for host in &mut inv.hosts {
                host.resolved.options.insert(
                    "proxycommand".into(),
                    vec![(
                        vec!["sshpass".into(), "-p".into(), "hunter2".into()],
                        host.source.clone(),
                    )],
                );
                host.resolved.proxy_command = Some("sshpass -p hunter2".into());
            }
        }
        app.update(Action::MetaSaved {
            alias: "lab".into(),
            result: Ok(HostMeta {
                favorite: true,
                ..HostMeta::default()
            }),
        });
        assert_eq!(
            app.toast.as_ref().map(|t| t.message.as_str()),
            Some("Added to favorites")
        );
        app.toast = None;
        let lines = screen(&mut app, 110, 28);
        check(
            &lines,
            &["lab ★", "ProxyCommand", "•••  hidden · l reveals in logs"],
        );
        assert!(!lines.join("\n").contains("hunter2"));
        app.update(Action::MetaSaved {
            alias: "lab".into(),
            result: Ok(HostMeta::default()),
        });
        assert_eq!(
            app.toast.as_ref().map(|t| t.message.as_str()),
            Some("Removed from favorites")
        );
        app.toast = None;

        // A failure on the host on screen: in the details, not in a toast.
        let id = SessionId::next();
        app.sessions.insert(
            "lab".into(),
            crate::app::SessionView {
                id,
                phase: portico_core::events::SessionPhase::Connecting {
                    hop: 1,
                    hops: 1,
                    target: "lab".into(),
                },
                latency: None,
            },
        );
        let failed = || {
            Action::Engine(EngineEvent::Session {
                id,
                event: portico_core::events::SessionEvent::Phase(
                    portico_core::events::SessionPhase::Failed {
                        error: ErrorSummary {
                            code: "connect_failed".into(),
                            message: "cannot connect to lab:22: connection refused".into(),
                            hint: Some("Is sshd running?".into()),
                        },
                    },
                ),
            })
        };
        app.update(failed());
        assert!(app.toast.is_none(), "already in the details");
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &["Cannot connect to lab:22", " ↵  retry", " l  logs"],
        );
        // Another host selected: the toast is the only place it shows.
        app.update(Action::SelectNext);
        app.update(failed());
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.kind == crate::app::ToastKind::Error)
        );
    }

    #[test]
    fn edit_form_says_esc_once_clears_its_edges_and_marks_the_bad_tag() {
        let mut app = hosts_app();
        app.update(Action::EditHost);
        app.update(Action::HostFormKey(key(KeyCode::Char('t'))));
        for c in "web ok bad! db".chars() {
            app.update(Action::EditKey(key(KeyCode::Char(c))));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines: Vec<String> = (0..24u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect()
            })
            .collect();
        let text = lines.join("\n");
        // `Esc` once in the panel (its border); the status bar has its own.
        let panel_rows: Vec<&String> = lines.iter().take(23).collect();
        let esc = panel_rows
            .iter()
            .map(|l| l.matches("Esc").count())
            .sum::<usize>();
        assert_eq!(esc, 1, "{text}");
        // The panel's corners have a clear column beside them.
        let top = lines.iter().position(|l| l.contains("╭ Edit lab")).unwrap();
        let row = lines.get(top).unwrap();
        let corner = row.chars().position(|c| c == '╭').unwrap();
        assert_eq!(row.chars().nth(corner - 1), Some(' '), "{text}");
        // The third tag is the refused one: drawn in the error colour.
        let s = app.styles();
        // (The chips' row is under the input, which has the same words.)
        let chip_row = lines.iter().rposition(|l| l.contains(" bad! ")).unwrap();
        let chip_x = lines
            .get(chip_row)
            .and_then(|l| {
                l.find(" bad! ")
                    .map(|b| l.get(..b).unwrap().chars().count() + 1)
            })
            .unwrap();
        let cell = buffer
            .cell((
                u16::try_from(chip_x).unwrap(),
                u16::try_from(chip_row).unwrap(),
            ))
            .unwrap();
        assert_eq!(cell.fg, crate::theme::c(s.theme.status.error), "{text}");
        let ok_x = lines
            .get(chip_row)
            .and_then(|l| {
                l.find(" ok ")
                    .map(|b| l.get(..b).unwrap().chars().count() + 1)
            })
            .unwrap();
        let ok = buffer
            .cell((
                u16::try_from(ok_x).unwrap(),
                u16::try_from(chip_row).unwrap(),
            ))
            .unwrap();
        assert_ne!(ok.fg, crate::theme::c(s.theme.status.error));
        check(&lines, &["Invalid tag 3"]);
    }

    #[test]
    fn status_bar_words_match_and_quit_comes_last() {
        let mut app = hosts_app();
        for width in [80, 110] {
            let lines = screen(&mut app, width, 24);
            let bar = lines.last().unwrap();
            assert!(bar.trim_end().ends_with("q  quit"), "{bar}");
            assert!(!bar.contains("star"), "{bar}");
        }
        app.update(Action::AskClearRecent);
        let lines = screen(&mut app, 80, 24);
        let bar = lines.last().unwrap();
        // The dialog says `y clear`: so does the bar.
        assert!(
            bar.contains(" y  clear") && bar.contains(" n / Esc  keep"),
            "{bar}"
        );
        check(&lines, &[" y  clear "]);
    }

    #[test]
    fn help_opens_over_the_logs_and_closes_back_to_them() {
        let mut app = logs_app(Vec::new());
        app.update(Action::DebugKey(key(KeyCode::Char('?'))));
        assert!(app.help.over_panel);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Keyboard shortcuts", "Session logs", "Host form"]);
        check(&lines, &["Switch tab · reveal", "Scroll · G follows"]);
        check(&lines, &["Next · prev field", "New line in notes"]);
        let lines = screen(&mut app, 110, 28);
        check(&lines, &[" HELP ", " Tab  page "]);
        app.update(Action::CloseHelp);
        assert!(!app.help.over_panel);
        assert_eq!(app.overlay, Some(Overlay::Debug), "the panel is still open");
        assert!(app.debug_panel.is_some());
        let lines = screen(&mut app, 80, 24);
        assert!(!lines.join("\n").contains("Keyboard shortcuts"));
    }

    /// A detection that gave up mid-reply (the terminal answers late).
    fn incomplete_light() -> crate::appearance::Detection {
        crate::appearance::Detection {
            appearance: portico_theme::Appearance::Dark,
            source: crate::appearance::Source::Fallback,
            elapsed: std::time::Duration::from_millis(100),
            complete: false,
            inside_osc: false,
        }
    }

    /// The keys `crossterm` makes of an OSC 11 reply: `ESC ]` is Alt+],
    /// the body plain characters, ST (`ESC \`) Alt+\ and BEL Ctrl+G.
    fn osc11_reply_as_keys(body: &str, bel: bool) -> Vec<crossterm::event::Event> {
        use crossterm::event::Event;
        let mut keys = vec![Event::Key(KeyEvent::new(
            KeyCode::Char(']'),
            KeyModifiers::ALT,
        ))];
        keys.extend(body.chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
        keys.push(Event::Key(if bel {
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
        } else {
            KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT)
        }));
        keys
    }

    #[tokio::test]
    async fn late_osc11_reply_is_not_typed_into_hosts() {
        let mut app = hosts_app();
        // Without the filter, such a reply is a burst of shortcuts.
        let typed: Vec<Action> = osc11_reply_as_keys("11;rgba:f4f4/f1f1/ecec/ffff", false)
            .into_iter()
            .filter_map(|e| app.map_event(e))
            .collect();
        assert!(
            typed.iter().any(|a| matches!(a, Action::OpenFiles)),
            "{typed:?}"
        );
        let mut app = hosts_app();
        app.detect_appearance(incomplete_light).await;
        assert!(app.stray_reply.armed());
        for bel in [false, true] {
            if bel {
                app.detect_appearance(incomplete_light).await;
            }
            for event in osc11_reply_as_keys("11;rgba:f4f4/f1f1/ecec/ffff", bel) {
                let action = app.map_event(event);
                assert!(
                    !matches!(
                        action,
                        Some(
                            Action::OpenFiles | Action::EditHost | Action::AddHost | Action::Reload
                        )
                    ),
                    "{action:?}"
                );
                if let Some(action) = action {
                    app.update(action);
                }
            }
            assert!(app.overlay.is_none());
            assert!(app.dialogs.is_empty());
            assert!(app.files.is_none());
            assert_eq!(app.focus, Focus::Hosts);
            assert!(!app.stray_reply.armed(), "disarmed at the end of the reply");
        }
        // The user's keys are theirs again.
        assert!(matches!(
            app.map_event(crossterm::event::Event::Key(key(KeyCode::Char('j')))),
            Some(Action::SelectNext)
        ));
    }

    static QUERIED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counted_light() -> crate::appearance::Detection {
        QUERIED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::appearance::Detection {
            appearance: portico_theme::Appearance::Light,
            source: crate::appearance::Source::Osc11,
            elapsed: std::time::Duration::ZERO,
            complete: true,
            inside_osc: false,
        }
    }

    #[tokio::test]
    async fn fixed_theme_skips_background_query() {
        use std::sync::atomic::Ordering;
        let mut app = test_app();
        app.update(Action::SettingsLoaded(Ok(Settings {
            theme: Some("basalt".into()),
            ..Settings::default()
        })));
        let before = QUERIED.load(Ordering::SeqCst);
        app.detect_appearance(counted_light).await;
        assert_eq!(QUERIED.load(Ordering::SeqCst), before, "not asked");
        assert_eq!(app.styles().theme.id, "basalt");
        assert!(!app.stray_reply.armed());
        // Cycling to Auto later: `COLORFGBG`, then dark, never the query.
        assert_eq!(
            app.theme.system,
            crate::appearance::without_query().appearance
        );
        // Auto (absent key) asks.
        let mut app = test_app();
        app.update(Action::SettingsLoaded(Ok(Settings::default())));
        app.detect_appearance(counted_light).await;
        assert!(QUERIED.load(Ordering::SeqCst) > before);
        assert_eq!(app.styles().theme.id, "marble");
    }

    #[test]
    fn auto_theme_from_settings_and_the_detected_background() {
        let mut app = test_app();
        // Absent theme: auto, dark on an undetected terminal.
        app.update(Action::SettingsLoaded(Ok(Settings::default())));
        assert!(app.theme.is_auto());
        assert_eq!(app.styles().theme.id, "obsidian");
        let settings = Settings {
            theme: Some("auto".into()),
            dark_theme: Some("basalt".into()),
            ..Settings::default()
        };
        app.update(Action::SettingsLoaded(Ok(settings)));
        assert_eq!(app.styles().theme.id, "basalt");
        assert!(app.toast.is_none());
        app.update(Action::SystemAppearance(portico_theme::Appearance::Light));
        assert_eq!(app.styles().theme.id, "marble");
        let lines = screen(&mut app, 80, 24);
        assert!(
            lines
                .first()
                .is_some_and(|l| l.trim_end().ends_with("Auto · Marble")),
            "{lines:?}"
        );
        // A light theme as the dark one is refused, with a notice.
        let settings = Settings {
            theme: Some("auto".into()),
            dark_theme: Some("marble".into()),
            ..Settings::default()
        };
        app.update(Action::SystemAppearance(portico_theme::Appearance::Dark));
        app.update(Action::SettingsLoaded(Ok(settings)));
        assert_eq!(app.styles().theme.id, "obsidian");
        let hint = app.toast.clone().and_then(|t| t.hint).unwrap_or_default();
        assert!(hint.contains("is not a dark theme"), "{hint}");
    }

    #[test]
    fn settings_theme_and_keymap() {
        let mut app = test_app();
        let mut settings = Settings {
            theme: Some("basalt".into()),
            ..Settings::default()
        };
        settings.keymap.tui.insert("hosts.edit".into(), "E".into());
        app.update(Action::SettingsLoaded(Ok(settings.clone())));
        assert_eq!(app.styles().theme.id, "basalt");
        assert!(app.toast.is_none());
        let lines = screen(&mut app, 110, 28);
        check(&lines, &[" E  edit"]);

        settings.theme = Some("solarized".into());
        settings.keymap.tui.insert("hosts.nope".into(), "z".into());
        app.update(Action::SettingsLoaded(Ok(settings)));
        assert_eq!(app.styles().theme.id, "obsidian");
        let toast = app.toast.clone().unwrap();
        // A warning (Portico goes on), the common title, the detail under it.
        assert_eq!(toast.kind, crate::app::ToastKind::Warning);
        assert_eq!(toast.message, "settings.toml: 2 entries ignored");
        let hint = toast.hint.unwrap_or_default();
        assert!(hint.contains("Unknown theme “solarized”"), "{hint}");
        assert!(hint.contains("hosts.nope (unknown action)"), "{hint}");
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["▲  settings.toml: 2 entries ignored"]);
        assert!(!lines.join("\n").contains('✕'), "not drawn as an error");
        assert_eq!(
            settings_title(1).as_deref(),
            Some("settings.toml: 1 entry ignored")
        );
        assert_eq!(settings_title(0), None);

        app.update(Action::SettingsLoaded(Err(ErrorSummary {
            code: "settings_invalid".into(),
            message: "invalid setting theme: empty value".into(),
            hint: Some("Fix or remove the offending entry in settings.toml.".into()),
        })));
        let toast = app.toast.clone().unwrap();
        assert_eq!(toast.kind, crate::app::ToastKind::Warning);
        assert_eq!(toast.message, "settings.toml ignored");
        assert!(
            toast
                .hint
                .is_some_and(|h| h.starts_with("Invalid setting theme"))
        );
    }

    #[test]
    fn clearing_recent_asks_first() {
        let mut app = hosts_app();
        app.update(Action::AskClearRecent);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Clear recent hosts?", " y  clear", " CONFIRM "]);
        app.update(Action::DialogKey(key(KeyCode::Char('n'))));
        assert!(app.dialogs.is_empty());
        assert_eq!(app.recent.len(), 1);
        let mut empty = test_app();
        empty.update(Action::AskClearRecent);
        assert!(empty.dialogs.is_empty(), "nothing to clear: no question");
    }

    #[test]
    fn help_and_prefix_bar_show_rebound_keys() {
        let mut app = test_app();
        let mut settings = Settings::default();
        settings
            .keymap
            .tui
            .insert("prefix.debug".into(), "L".into());
        app.update(Action::SettingsLoaded(Ok(settings)));
        app.update(Action::Prefix);
        let lines = screen(&mut app, 110, 28);
        check(&lines, &[" L  logs"]);
        app.update(Action::CancelPrefix);
        app.update(Action::ToggleHelp);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["d · L", "Disconnect · logs"]);
    }
}
