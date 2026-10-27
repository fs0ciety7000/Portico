//! Bottom bar: mode pill + contextual key hints, and, in a terminal, the
//! session's status on the right.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};
use crate::app_panes::NoteTone;
use crate::dialog::{Dialog, SecretPurpose};
use crate::ui::{fmt, status};

/// Longest alias in the right block, and how short it gets on a narrow
/// bar before any hint goes.
const ALIAS_MAX: usize = 24;
const ALIAS_SHORT: usize = 12;

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let width = usize::from(area.width);
    let (mode, hints) = mode_and_hints(app, frame.area());
    let pill = if (app.prefix || pane_mode(app)) && app.dialogs.is_empty() {
        s.prefix_pill()
    } else {
        s.mode_pill()
    };
    let mut spans = vec![Span::styled(mode, pill), Span::raw(" ")];
    let lead: usize = spans.iter().map(Span::width).sum();
    // The right block is laid out first and its room kept: the session's
    // status and latency, then `● REC`, are never pushed out by hints. On
    // a short bar the remote title goes, then the alias shortens, then
    // hints are dropped.
    let rec = rec_spans(app);
    let right_of = |alias_max: usize, title: bool| {
        let mut right = session_spans(app, alias_max, title);
        right.extend(rec.iter().cloned());
        right
    };
    let width_of = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();

    // A note takes the hints' place for a few seconds: one line, never
    // over a pane (the next keys still go to the focused one).
    if let Some(note) = app.note.as_ref().filter(|_| app.dialogs.is_empty()) {
        let (mark, style) = match note.tone {
            NoteTone::Warning => ("▲ ", s.status(s.theme.status.warning)),
            NoteTone::Neutral => ("", s.secondary()),
        };
        let mark_width = mark.chars().count();
        let message_width = note.message.chars().count();
        let mut right = right_of(ALIAS_MAX, false);
        if lead + mark_width + message_width + 1 + width_of(&right) > width {
            right = right_of(ALIAS_SHORT, false);
        }
        let right_width = width_of(&right);
        let room = width.saturating_sub(lead + mark_width + right_width + 1);
        if !mark.is_empty() {
            spans.push(Span::styled(mark, style));
        }
        spans.push(Span::styled(fmt::truncate(&note.message, room), style));
        // ` · hint`, cut when the bar is narrow (the message comes first).
        let used = width_of(&spans);
        let room = width.saturating_sub(used + 3 + 1 + right_width);
        if room >= 8 && !note.hint.is_empty() {
            spans.push(Span::styled(" · ", s.muted()));
            let hint_style = match note.tone {
                NoteTone::Warning => s.secondary(),
                NoteTone::Neutral => s.muted(),
            };
            spans.push(Span::styled(fmt::truncate(&note.hint, room), hint_style));
        }
        push_right(&mut spans, right, width);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
        return;
    }
    // Hints are listed by importance: drop the tail rather than overflow.
    // `quit` (and the player's `back to list`) is the exception: always
    // last, and its room is kept.
    let mut hints = hints;
    let pinned = if hints
        .last()
        .is_some_and(|(_, label)| matches!(*label, "quit" | "back to list"))
    {
        hints.pop()
    } else {
        None
    };
    let need = |(key, label): &(String, &str)| key.chars().count() + label.chars().count() + 4;
    let all_hints: usize = hints.iter().chain(pinned.iter()).map(need).sum();
    let mut right = right_of(ALIAS_MAX, true);
    for (alias_max, title) in [(ALIAS_MAX, false), (ALIAS_SHORT, false)] {
        if lead + all_hints + width_of(&right) <= width {
            break;
        }
        right = right_of(alias_max, title);
    }
    if lead + width_of(&right) > width {
        // Not even the status fits next to the mode: REC alone.
        right.clone_from(&rec);
    }
    // `0.3.1 available` before the status, when it fits beside the mode
    // and `quit` (hints give way to it, the status never does).
    let badge = badge_spans(app, !right.is_empty());
    if !badge.is_empty()
        && lead + pinned.as_ref().map_or(0, need) + width_of(&badge) + width_of(&right) <= width
    {
        right = badge.into_iter().chain(right).collect();
    }
    // On the host list, the sync item: before the hints when it says
    // something (a state, a count), after them when sync is just off.
    let quiet_sync = with_sync(
        app,
        &mut right,
        width,
        lead + pinned.as_ref().map_or(0, need),
    );
    let mut used = lead + pinned.as_ref().map_or(0, need) + width_of(&right);
    let mut shown: Vec<&(String, &str)> = Vec::new();
    for hint in &hints {
        if used + need(hint) > width {
            break;
        }
        used += need(hint);
        shown.push(hint);
    }
    if !quiet_sync.is_empty() && used + width_of(&quiet_sync) <= width {
        right = quiet_sync.into_iter().chain(right).collect();
    }
    shown.extend(pinned.iter());
    for (key, label) in shown {
        spans.push(Span::styled(format!(" {key} "), s.key_hint()));
        spans.push(Span::styled(format!(" {label} "), s.muted()));
    }
    push_right(&mut spans, right, width);
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
}

/// `● alias · 12 ms · remote title ` for the focused pane, in a terminal
/// (or behind a panel opened from one); empty elsewhere. `alias_max`: how
/// much of the alias shows; `title`: with the remote title.
fn session_spans(app: &App, alias_max: usize, title: bool) -> Vec<Span<'static>> {
    let s = app.styles();
    let mut right = Vec::new();
    let Some(slot) = app
        .focused_slot()
        .filter(|_| matches!(app.focus, Focus::Terminal) || app.prefix)
        .filter(|_| app.focus != Focus::Files)
    else {
        return right;
    };
    let st = status::status(slot.session().and_then(|id| app.session_by_id(id)), s);
    right.push(Span::styled("● ", s.status(st.color)));
    right.push(Span::styled(
        fmt::truncate(slot.alias(), alias_max),
        s.secondary(),
    ));
    if let Some(rtt) = st.latency {
        let style = if st.slow {
            s.status(s.theme.status.warning)
        } else {
            s.muted()
        };
        right.push(Span::styled(" · ", s.muted()));
        right.push(Span::styled(rtt, style));
    }
    if title
        && let Some(remote) = slot
            .view()
            .and_then(crate::terminal::TerminalView::remote_title)
    {
        right.push(Span::styled(" · ", s.muted()));
        right.push(Span::styled(fmt::truncate(&remote, 32), s.muted()));
    }
    right.push(Span::raw(" "));
    right
}

/// Puts the sync item before `right` when it says something and fits
/// (`used`: what the bar needs besides); returns it when sync is just off
/// (shown only if room is left after the hints).
fn with_sync(
    app: &App,
    right: &mut Vec<Span<'static>>,
    width: usize,
    used: usize,
) -> Vec<Span<'static>> {
    let width_of = |spans: &[Span<'static>]| spans.iter().map(Span::width).sum::<usize>();
    let Some((sync, quiet)) = home_sync(app, width.saturating_sub(used + 12)) else {
        return Vec::new();
    };
    if quiet {
        return sync;
    }
    if used + width_of(&sync) + width_of(right) <= width {
        let rest = std::mem::take(right);
        *right = sync.into_iter().chain(rest).collect();
    }
    Vec::new()
}

/// The sync item, on the host list only (a terminal's bar is its
/// session's). `(spans, quiet)`.
fn home_sync(app: &App, max: usize) -> Option<(Vec<Span<'static>>, bool)> {
    let home = app.active.is_none()
        && app.overlay.is_none()
        && app.dialogs.is_empty()
        && !app.prefix
        && matches!(app.focus, Focus::Hosts | Focus::Search);
    if !home {
        return None;
    }
    crate::ui::sync::status_spans(app, max.clamp(8, 40))
}

/// `0.3.1 available ` in the accent (a notice, not a status colour), when
/// the update check knows a newer release; `0.3.1 available · ` (muted
/// separator) when the session status or `● REC` follows.
fn badge_spans(app: &App, followed: bool) -> Vec<Span<'static>> {
    let s = app.styles();
    app.update.badge().map_or_else(Vec::new, |badge| {
        let gap = if followed {
            Span::styled(" · ", s.muted())
        } else {
            Span::raw(" ")
        };
        vec![Span::styled(badge, s.accent()), gap]
    })
}

/// `● REC` (`● REC 2` for two recorded terminals) in the error colour,
/// whenever a terminal is being recorded; empty otherwise.
fn rec_spans(app: &App) -> Vec<Span<'static>> {
    let s = app.styles();
    match app.recording.len() {
        0 => Vec::new(),
        1 => vec![Span::styled("● REC", super::rec_style(s)), Span::raw(" ")],
        n => vec![
            Span::styled(format!("● REC {n}"), super::rec_style(s)),
            Span::raw(" "),
        ],
    }
}

/// `right`, flush right after `spans`, when it fits.
fn push_right(spans: &mut Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) {
    let used: usize = spans.iter().map(Span::width).sum();
    let need: usize = right.iter().map(Span::width).sum();
    if need > 0 && used + need <= width {
        spans.push(Span::raw(" ".repeat(width - used - need)));
        spans.extend(right);
    }
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

/// `R  record` or `R  stop recording`, for the focused pane.
fn record_hint(app: &App) -> Option<(String, &'static str)> {
    let (key, label) = app.keymap.hint("prefix.record")?;
    let recorded = app.focused_slot().is_some_and(|slot| app.recorded_in(slot));
    Some((
        key,
        if recorded {
            crate::app_recording::STOP_HINT
        } else {
            label
        },
    ))
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
            let keys = match app.dialogs.front() {
                Some(Dialog::Confirm(c)) => {
                    let mut keys = c.other_hints();
                    keys.push(("y", c.confirm));
                    keys
                }
                _ => vec![("n / Esc", "keep"), ("y", "confirm")],
            };
            return (" CONFIRM ", owned(keys));
        }
        // The vault a host's password waits for: Esc goes back to the form.
        _ if matches!(app.dialogs.front(), Some(Dialog::Secret(d)) if d.host_save) => {
            if matches!(
                app.dialogs.front(),
                Some(Dialog::Secret(d)) if matches!(d.purpose, SecretPurpose::VaultCreate { .. })
            ) {
                (" DIALOG ", &[("↵", "create & save"), ("Esc", "not now")])
            } else {
                (" DIALOG ", &[("↵", "unlock & save"), ("Esc", "not now")])
            }
        }
        // A new host key: the two answers (the same words as the dialog).
        _ if matches!(
            app.dialogs.front(),
            Some(Dialog::Prompt(p)) if matches!(p.kind, portico_core::events::PromptKind::UnknownHostKey { .. })
                && !p.needs_typed_confirmation()
        ) =>
        {
            (" HOST KEY ", &[("y", "trust"), ("n", "refuse")])
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
            hints.extend(keyed(app, &["prefix.split_host"]));
            hints.extend(record_hint(app));
            hints.extend(keyed(
                app,
                &[
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
                hints.extend(record_hint(app));
            }
            hints.extend(keyed(
                app,
                &[
                    "prefix.recordings",
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
        _ if app.overlay == Some(crate::app::Overlay::Sync) => {
            let hints = app.sync_panel.as_ref().map_or_else(
                || vec![("Esc", "close")],
                |p| p.hints(&app.sync, std::time::Instant::now()),
            );
            return (" SYNC ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::SyncWizard) => {
            return (" SYNC ", owned(vec![("Esc", "back")]));
        }
        _ if app.overlay == Some(crate::app::Overlay::Recordings) => {
            let hints = app.recordings_panel.as_ref().map_or_else(
                || vec![("Esc", "close")],
                crate::recordings::RecordingsPanel::hints,
            );
            return (" RECORDINGS ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::Player) => {
            // Most used first; `q back to list` is pinned (see `render`).
            let space = match app.player.as_ref().map(crate::player::Player::state) {
                Some(crate::player::PlayerState::Ended) => {
                    return (
                        " PLAYBACK ",
                        owned(vec![("Space", "replay"), ("q", "back to list")]),
                    );
                }
                Some(crate::player::PlayerState::Paused) => ("Space", "resume"),
                _ => ("Space", "pause"),
            };
            let hints = vec![
                space,
                ("←", "back 5 s"),
                ("→", "skip idle"),
                ("+ -", "speed"),
                ("Home", "restart"),
                ("q", "back to list"),
            ];
            return (" PLAYBACK ", owned(hints));
        }
        _ if app.overlay == Some(crate::app::Overlay::HostForm) => {
            let hints: &[(&'static str, &'static str)] = match &app.host_form {
                Some(f) if matches!(f.mode, crate::host_form::FormMode::ReadOnly { .. }) => &[
                    ("D", "duplicate"),
                    ("t", "tags and notes"),
                    ("Esc", "close"),
                ],
                Some(f) if f.confirm_discard => &[("y", "discard"), ("n / Esc", "keep editing")],
                Some(f) if f.busy => &[],
                Some(f) if matches!(f.mode, crate::host_form::FormMode::Duplicate { .. }) => {
                    &[("↵", "duplicate"), ("Esc", "cancel")]
                }
                Some(f) if f.on_picker() => &[
                    ("↵", "save"),
                    ("Tab", "next field"),
                    ("← →", "choose"),
                    ("Esc", "cancel"),
                ],
                _ => &[("↵", "save"), ("Tab", "next field"), ("Esc", "cancel")],
            };
            return (" HOST ", owned(hints.to_vec()));
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
        // The search bar has the keys: its own hints (Shift-↵ is not told
        // apart from ↵ by most terminals, Ctrl-N is).
        Focus::Terminal if app.searching() => (
            " SEARCH ",
            &[("↵", "older"), ("Ctrl-N", "newer"), ("Esc", "close")],
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
            // A recorded pane: how to stop comes first (it names the
            // prefix too, so it is what stays on a narrow bar).
            if app.focused_slot().is_some_and(|slot| app.recorded_in(slot))
                && let Some((key, _)) = app.keymap.hint("prefix.record")
            {
                hints.push((format!("Ctrl-Space {key}"), crate::app_recording::STOP_HINT));
            }
            hints.push(("Ctrl-Space".to_owned(), "commands"));
            hints.extend(keyed(app, &["terminal.scroll_up"]));
            if let Some((key, label)) = app.keymap.hint("terminal.search") {
                hints.push((format!("Ctrl-Space {key}"), label));
            }
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
                "hosts.add",
                "hosts.edit",
                "hosts.favorite",
                "hosts.debug",
                "hosts.help",
                "hosts.keys",
                "hosts.snippets",
                "hosts.recordings",
                "hosts.sync",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_badge_is_separated_from_what_follows_it() {
        let mut app = crate::app::test_app();
        assert!(badge_spans(&app, true).is_empty());
        app.update.status = Some(portico_core::update::UpdateStatus {
            check: Some(true),
            current: "0.3.0".into(),
            available: Some(portico_core::update::AvailableUpdate {
                version: "0.3.1".into(),
                url: "https://github.com/fs0ciety7000/Portico/releases/tag/v0.3.1".into(),
            }),
            checked_at: None,
        });
        assert_eq!(text(&badge_spans(&app, false)), "0.3.1 available ");
        let followed = badge_spans(&app, true);
        assert_eq!(text(&followed), "0.3.1 available · ");
        let s = app.styles();
        assert_eq!(followed.last().map(|sp| sp.style), Some(s.muted()));
    }
}
