//! App glue for phase 4: favorites and host metadata, recent hosts, the
//! debug panel, settings (theme and keymap). Engine calls run in tasks and
//! come back as [`Action`]s; panel state lives in `edit_host`, `debug` and
//! `host_list` (pure, tested there).

use std::io::Write;

use crossterm::event::{KeyCode, KeyEvent};
use portico_core::settings::Settings;
use portico_core::{EngineEvent, ErrorSummary};
use portico_theme::{THEMES, Theme};

use crate::action::Action;
use crate::app::{App, Focus, Overlay};
use crate::debug::{DebugOutcome, DebugPanel};
use crate::dialog::{ConfirmAction, ConfirmDialog, Dialog};
use crate::edit_host::{EditField, EditHostPanel, EditOutcome, EditRequest, field_of};
use crate::host_list::RECENT_FETCHED;
use crate::keymap::Keymap;
use crate::keys::Notice;

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
            Action::EditHost => self.open_edit(),
            Action::EditKey(key) => self.edit_key(key),
            Action::MetaSaved { alias, result } => match result {
                Ok(meta) => self.set_meta(&alias, &meta),
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
                        body:
                            "The Recent section empties. Hosts, tags and groups stay as they are."
                                .into(),
                        confirm: "clear",
                        action: ConfirmAction::ClearRecent,
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
            Action::ThemeSaved(Err(e)) => self.notify_error(ErrorSummary {
                message: format!("theme not saved: {}", e.message),
                ..e
            }),
            _ => {}
        }
    }

    /// Engine events of phase 4 (others are handled in `on_engine_event`).
    pub(crate) fn on_meta_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::HostMeta { alias, meta } => self.set_meta(&alias, &meta),
            EngineEvent::RecentChanged => self.spawn_load_recent(),
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

    fn open_edit(&mut self) {
        let Some(host) = self.selected_host() else {
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
        let action = dialog.action;
        match key.code {
            KeyCode::Char('y' | 'Y') => {
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
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.dialogs.pop_front();
            }
            _ => {}
        }
    }

    // ── debug panel ────────────────────────────────────────────────────────

    fn open_debug(&mut self) {
        self.prefix = false;
        // From a terminal tab: its session. From the list: the newest
        // session of the selected host.
        let sessions = self.engine.sessions_with_logs();
        let from_terminal = matches!(self.focus, Focus::Terminal | Focus::Files)
            .then(|| self.active_terminal().map(|t| t.session))
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
            DebugOutcome::Copy { text, lines, total } => {
                self.osc52(&text);
                let message = if lines < total {
                    format!("Copied the last {lines} of {total} lines (OSC 52)")
                } else {
                    format!("Copied {lines} lines (OSC 52)")
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

    // ── settings ───────────────────────────────────────────────────────────

    /// Theme and keymap from `settings.toml`; problems become one toast.
    fn apply_settings(&mut self, result: Result<Settings, ErrorSummary>) {
        let settings = match result {
            Ok(s) => s,
            Err(e) => {
                self.notify_error(ErrorSummary {
                    message: format!("settings.toml ignored: {}", e.message),
                    ..e
                });
                return;
            }
        };
        let mut problems = Vec::new();
        if let Some(id) = &settings.theme {
            if let Some(i) = THEMES.iter().position(|t| t.id == id) {
                self.theme_index = i;
            } else {
                self.theme_index = 0;
                problems.push(format!(
                    "unknown theme “{}”: using {}",
                    crate::ui::fmt::truncate(id, 24),
                    Theme::default_theme().name
                ));
            }
        }
        let (keymap, ignored) = Keymap::with_overrides(&settings.keymap.tui);
        self.keymap = keymap;
        if !ignored.is_empty() {
            let n = ignored.len();
            problems.push(format!(
                "{n} keymap entr{} ignored: {}",
                if n == 1 { "y" } else { "ies" },
                ignored.join(" · ")
            ));
        }
        if !problems.is_empty() {
            self.toast = Some(crate::app::Toast {
                message: "settings.toml: some entries were ignored".into(),
                hint: Some(problems.join(" — ")),
                is_error: true,
                ttl: 100,
            });
        }
    }

    /// `t`: next theme, saved to `settings.toml` (re-read first, so other
    /// settings, even another UI's, are kept).
    pub(crate) fn cycle_theme(&mut self) {
        self.theme_index = (self.theme_index + 1) % THEMES.len();
        let theme = self.styles().theme;
        self.notify(format!("Theme · {}", theme.name));
        let (engine, id) = (self.engine.clone(), theme.id);
        spawn(self, async move {
            let result = async {
                let mut settings = engine.settings().await?;
                settings.theme = Some(id.to_owned());
                engine.save_settings(settings).await.map(|_| ())
            }
            .await
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
                "Debug",
                "Sessions",
                "No session yet.",
                "Logs",
                "Config",
                " DEBUG ",
            ],
        );
        app.update(Action::DebugKey(key(KeyCode::Tab)));
        let option = |key: &str, value: &str, origin: OptionOrigin| EffectiveOption {
            key: key.into(),
            values: vec![value.into()],
            origin,
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
                "1/2",
                "ops@b.example.com:2222",
                "hostname",
                "/etc/ssh/ssh_config:12",
                "default",
                "hidden · s reveals",
                " s  reveal",
            ],
        );
        assert!(!lines.join("\n").contains("hunter2"));
        app.update(Action::DebugKey(key(KeyCode::Char('s'))));
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["hunter2"]);
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
        let hint = app
            .toast
            .as_ref()
            .and_then(|t| t.hint.clone())
            .unwrap_or_default();
        assert!(hint.contains("unknown theme “solarized”"), "{hint}");
        assert!(hint.contains("hosts.nope (unknown action)"), "{hint}");

        app.update(Action::SettingsLoaded(Err(ErrorSummary {
            code: "settings_invalid".into(),
            message: "invalid setting theme: empty value".into(),
            hint: Some("Fix or remove the offending entry in settings.toml.".into()),
        })));
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.is_error && t.message.starts_with("settings.toml ignored"))
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
