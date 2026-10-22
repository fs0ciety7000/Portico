//! Bottom bar: mode pill + contextual key hints, and, in a terminal, the
//! session's status on the right.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};
use crate::dialog::{Dialog, SecretPurpose};
use crate::ui::{fmt, status};

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let (mode, hints) = mode_and_hints(app, frame.area());
    let pill = if (app.prefix || pane_mode(app)) && app.dialogs.is_empty() {
        s.prefix_pill()
    } else {
        s.mode_pill()
    };
    let mut spans = vec![Span::styled(mode, pill), Span::raw(" ")];
    // A refusal takes the hints' place for a few seconds: one line, never
    // over a pane (the next keys still go to the focused one).
    if let Some(note) = app.note.as_ref().filter(|_| app.dialogs.is_empty()) {
        let warning = s.status(s.theme.status.warning);
        let width = usize::from(area.width);
        let used: usize = spans.iter().map(Span::width).sum();
        let message = fmt::truncate(&note.message, width.saturating_sub(used + 3));
        spans.push(Span::styled("▲ ", warning));
        spans.push(Span::styled(message, warning));
        let used: usize = spans.iter().map(Span::width).sum();
        // ` · hint`, cut when the bar is narrow (the message comes first).
        let room = width.saturating_sub(used + 3 + 1);
        if room >= 8 {
            spans.push(Span::styled(" · ", s.muted()));
            spans.push(Span::styled(fmt::truncate(&note.hint, room), s.secondary()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
        return;
    }
    // Hints are listed by importance: drop the tail rather than overflow.
    // `quit` is the exception: always last, and its room is kept.
    let mut hints = hints;
    let pinned = if hints.last().is_some_and(|(_, label)| *label == "quit") {
        hints.pop()
    } else {
        None
    };
    let need = |(key, label): &(String, &str)| key.chars().count() + label.chars().count() + 4;
    let mut used: usize =
        spans.iter().map(Span::width).sum::<usize>() + pinned.as_ref().map_or(0, need);
    let mut shown: Vec<&(String, &str)> = Vec::new();
    for hint in &hints {
        if used + need(hint) > usize::from(area.width) {
            break;
        }
        used += need(hint);
        shown.push(hint);
    }
    shown.extend(pinned.iter());
    for (key, label) in shown {
        spans.push(Span::styled(format!(" {key} "), s.key_hint()));
        spans.push(Span::styled(format!(" {label} "), s.muted()));
    }

    // Right side: `● alias · 12 ms · remote title`, only when it fits.
    if let Some(slot) = app
        .focused_slot()
        .filter(|_| matches!(app.focus, Focus::Terminal) || app.prefix)
        .filter(|_| app.focus != Focus::Files)
    {
        let st = status::status(slot.session().and_then(|id| app.session_by_id(id)), s);
        let mut right = vec![
            Span::styled("● ", s.status(st.color)),
            Span::styled(fmt::truncate(slot.alias(), 24), s.secondary()),
        ];
        if let Some(rtt) = st.latency {
            let style = if st.slow {
                s.status(s.theme.status.warning)
            } else {
                s.muted()
            };
            right.push(Span::styled(" · ", s.muted()));
            right.push(Span::styled(rtt, style));
        }
        if let Some(title) = slot
            .view()
            .and_then(crate::terminal::TerminalView::remote_title)
        {
            right.push(Span::styled(" · ", s.muted()));
            right.push(Span::styled(fmt::truncate(&title, 32), s.muted()));
        }
        right.push(Span::raw(" "));
        let used: usize = spans.iter().map(Span::width).sum();
        let need: usize = right.iter().map(Span::width).sum();
        let width = usize::from(area.width);
        if used + need < width {
            spans.push(Span::raw(" ".repeat(width - used - need)));
            spans.extend(right);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
}

type Hints = Vec<(String, &'static str)>;

/// Pane mode is on (the arrows move between panes without the prefix);
/// it ends with the window (`Action::Expire`) or the next other key.
fn pane_mode(app: &App) -> bool {
    app.pane_repeat.is_some()
        && app.focus == Focus::Terminal
        && app.overlay.is_none()
        && app.dialogs.is_empty()
        && app.active_tab().is_some()
}

/// `←↓↑→` (the effective keys of the four pane moves).
fn pane_arrows(app: &App) -> String {
    app.keymap.joined(&[
        "prefix.pane_left",
        "prefix.pane_down",
        "prefix.pane_up",
        "prefix.pane_right",
    ])
}

fn owned(hints: Vec<(&'static str, &'static str)>) -> Hints {
    hints.into_iter().map(|(k, l)| (k.to_owned(), l)).collect()
}

/// Effective keys of `ids`, in order (unbound actions are left out).
fn keyed(app: &App, ids: &[&str]) -> Hints {
    ids.iter().filter_map(|id| app.keymap.hint(id)).collect()
}

/// Mode pill and the key hints, most important first (what does not fit is
/// in `?`). Only keys that work right now are listed.
// A flat table of modes; splitting it would hide which one wins.
#[allow(clippy::too_many_lines)]
fn mode_and_hints(app: &App, screen: Rect) -> (&'static str, Hints) {
    let (mode, hints): (&'static str, &[(&'static str, &'static str)]) = match app.focus {
        // At launch, Esc is not a cancel: Portico goes on without the vault.
        _ if matches!(
            app.dialogs.front(),
            Some(Dialog::Secret(d)) if matches!(d.purpose, SecretPurpose::VaultUnlock { startup: true })
        ) =>
        {
            (" DIALOG ", &[("↵", "unlock"), ("Esc", "continue without")])
        }
        // The same words as the dialog's own footer (`n / Esc keep`, `y clear`).
        _ if matches!(app.dialogs.front(), Some(Dialog::Confirm(_))) => {
            let confirm = match app.dialogs.front() {
                Some(Dialog::Confirm(c)) => c.confirm,
                _ => "confirm",
            };
            return (
                " CONFIRM ",
                owned(vec![("n / Esc", "keep"), ("y", confirm)]),
            );
        }
        _ if !app.dialogs.is_empty() => (" DIALOG ", &[("Esc", "cancel / close")]),
        _ if app.prefix && app.active_tab().is_some_and(|t| t.len() > 1) => {
            // A split tab: the pane commands first, then help, then the
            // way out (what does not fit at 80 columns is in `?`).
            let mut hints = Vec::new();
            let splits = [
                app.keymap.shown("prefix.split_vertical", 1),
                app.keymap.shown("prefix.split_horizontal", 1),
            ]
            .into_iter()
            .filter(|k| !k.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
            if !splits.is_empty() {
                hints.push((splits, "split"));
            }
            let arrows = pane_arrows(app);
            if !arrows.is_empty() {
                hints.push((arrows, "pane"));
            }
            hints.extend(keyed(app, &["prefix.close_pane", "prefix.zoom_pane"]));
            hints.extend(keyed(app, &["prefix.help", "prefix.home"]));
            hints.push(("1-9".into(), "tab"));
            hints.extend(keyed(
                app,
                &[
                    "prefix.split_host",
                    "prefix.next_pane",
                    "prefix.files",
                    "prefix.tunnels",
                    "prefix.debug",
                    "prefix.new_tab",
                    "prefix.close_tab",
                    "prefix.disconnect",
                ],
            ));
            hints.push(("Esc".into(), "cancel"));
            return (" PREFIX ", hints);
        }
        _ if app.prefix => {
            let mut hints = keyed(app, &["prefix.home"]);
            hints.push(("1-9".into(), "tab"));
            // In a tab, the pane commands come first.
            if app.active_tab().is_some() {
                hints.extend(keyed(
                    app,
                    &["prefix.split_vertical", "prefix.split_horizontal"],
                ));
                hints.extend(keyed(app, &["prefix.split_host"]));
            }
            hints.extend(keyed(
                app,
                &[
                    "prefix.files",
                    "prefix.tunnels",
                    "prefix.debug",
                    "prefix.new_tab",
                    "prefix.close_tab",
                    "prefix.disconnect",
                ],
            ));
            hints.push(("Esc".into(), "cancel"));
            return (" PREFIX ", hints);
        }
        _ if pane_mode(app) => {
            let mut hints = Vec::new();
            let arrows = pane_arrows(app);
            if !arrows.is_empty() {
                hints.push((arrows, "pane"));
            }
            let resize = app.keymap.joined(&[
                "prefix.resize_left",
                "prefix.resize_down",
                "prefix.resize_up",
                "prefix.resize_right",
            ]);
            if !resize.is_empty() {
                hints.push((resize, "move divider"));
            }
            hints.push(("any key".into(), "back to the pane"));
            return (" PANE ", hints);
        }
        _ if app.overlay == Some(crate::app::Overlay::Keys) => {
            let hints = app.keys_panel.as_ref().map_or_else(
                || vec![("Esc", "close")],
                |p| p.hints(app.vault, app.copy_state()),
            );
            return (" KEYS ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::Snippets) => {
            let hints = app.snippets_panel.as_ref().map_or_else(
                || vec![("Esc", "close")],
                crate::snippets::SnippetsPanel::hints,
            );
            return (" SNIPPETS ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::Tunnels) => {
            let hints = app
                .tunnels_panel
                .as_ref()
                .map_or_else(|| vec![("Esc", "close")], |p| p.hints(&app.tunnels));
            return (" TUNNELS ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::Edit) => (
            " EDIT ",
            &[
                ("Tab", "next field"),
                ("Ctrl-S", "save"),
                ("Ctrl-U", "clear"),
                ("Esc", "cancel"),
            ],
        ),
        _ if app.help.over_panel || app.overlay == Some(crate::app::Overlay::Help) => {
            if super::overlays::help_paged(app, screen.height) {
                (" HELP ", &[("Tab", "page"), ("Esc", "close")])
            } else {
                (" HELP ", &[("Esc", "close")])
            }
        }
        _ if app.overlay == Some(crate::app::Overlay::Debug) => {
            let narrow = screen.width < crate::debug::NARROW;
            let hints = app
                .debug_panel
                .as_ref()
                .map_or_else(|| vec![("Esc", "close")], |p| p.hints(narrow));
            return (" LOGS ", owned(hints));
        }
        Focus::Files
            if app.files_refused()
                && app
                    .files
                    .as_ref()
                    .is_some_and(|f| f.side == crate::files::Side::Remote) =>
        {
            (
                " FILES ",
                &[("Tab", "local side"), ("?", "help"), ("Esc", "back")],
            )
        }
        Focus::Files
            if app.files.as_ref().is_some_and(|f| {
                f.side == crate::files::Side::Remote && !app.session_alive(f.session)
            }) =>
        {
            (
                " FILES ",
                &[
                    ("↵", "reconnect"),
                    ("Tab", "local side"),
                    ("?", "help"),
                    ("Esc", "back"),
                ],
            )
        }
        Focus::Files => (
            " FILES ",
            &[
                ("Tab", "switch side"),
                ("↵", "open"),
                ("c", "copy"),
                ("n", "folder"),
                ("d", "delete"),
                ("?", "help"),
                ("Esc", "back"),
            ],
        ),
        Focus::Terminal => {
            let mut hints = Vec::new();
            if app
                .focused_slot()
                .and_then(|slot| app.pane_failure(slot))
                .is_some_and(|f| f.retry)
            {
                hints.push(("↵".to_owned(), "retry"));
            }
            hints.push(("Ctrl-Space".to_owned(), "commands"));
            hints.extend(keyed(app, &["terminal.scroll_up"]));
            return (" TERMINAL ", hints);
        }
        // `Ctrl-Space S`: choosing the host of a new pane.
        Focus::Hosts | Focus::Search if app.pane_pick.is_some() => {
            let hints = vec![
                ("↵".to_owned(), "split with host"),
                ("↑↓".to_owned(), "move"),
                ("Esc".to_owned(), "cancel"),
            ];
            return (" SPLIT ", hints);
        }
        Focus::Hosts => {
            let mut ids = vec!["hosts.connect"];
            if app.selected_is_recent() {
                ids.push("hosts.forget_recent");
            }
            ids.extend([
                "hosts.search",
                "hosts.edit",
                "hosts.favorite",
                "hosts.debug",
                "hosts.help",
                "hosts.keys",
                "hosts.snippets",
                "hosts.theme",
                // Always last, and always shown (see `render`).
                "hosts.quit",
            ]);
            return (" HOSTS ", keyed(app, &ids));
        }
        Focus::Search => (
            " SEARCH ",
            &[
                ("↵", "connect"),
                ("↑↓", "move"),
                ("#tag", "filter"),
                ("Esc", "done"),
            ],
        ),
    };
    (mode, owned(hints.to_vec()))
}
