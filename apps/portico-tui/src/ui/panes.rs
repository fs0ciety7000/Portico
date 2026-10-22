//! A terminal tab: its panes, their title rows and the thin dividers
//! between them (phase 6, split panes).
//!
//! A tab with one pane draws its terminal edge to edge, as before. With
//! several panes, each gets a one-row title: alias, status dot, phase and
//! latency. The focused pane's title carries the accent bar on the chrome
//! surface; the others are dimmed (muted text on the base surface), their
//! status dot keeping its meaning colour. A pane still opening shows the
//! connection phase; a pane whose connection failed shows the error, its
//! hint and `↵ retry` (focused) or how to focus it (others: `↵` goes to
//! the focused pane). A failed pane has a filled dot and its phase in
//! `status-error`, in its title as in its tab; the hollow dot is kept for
//! a pane with no shell attached (exited, closed). Dividers are
//! `border-subtle` strokes on the base surface. Only theme tokens are used
//! (through `Styles`).

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use portico_core::events::SessionPhase;

use crate::app::App;
use crate::app_panes::{PaneSlot, Tab};
use crate::panes::{Divider, Split};
use crate::terminal::Ended;
use crate::theme::Styles;
use crate::ui::{fmt, status};

/// Draws tab `tab` in `area`.
pub(crate) fn render(app: &App, tab: &Tab, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let layout = tab.layout(area);
    let unobstructed = app.dialogs.is_empty() && app.overlay.is_none();
    let count = tab.len();
    let ids = tab.ids();
    for divider in &layout.dividers {
        draw_divider(*divider, s, frame.buffer_mut());
    }
    join_dividers(&layout.dividers, frame.buffer_mut());
    for pane in &layout.panes {
        let Some(slot) = tab.get(pane.id) else {
            continue;
        };
        let focused = pane.id == tab.focus();
        if let Some(title) = pane.title {
            let number = ids
                .iter()
                .position(|id| *id == pane.id)
                .map_or(0, |i| i + 1);
            let place = Place {
                number,
                count,
                zoomed: tab.zoomed(),
            };
            render_title(app, slot, focused, place, frame, title);
        }
        render_body(app, slot, focused && unobstructed, frame, pane.grid);
    }
}

fn draw_divider(divider: Divider, s: Styles, buf: &mut Buffer) {
    let symbol = match divider.split {
        Split::Vertical => "│",
        Split::Horizontal => "─",
    };
    // The stroke colour over the base surface (not the base text colour).
    let style = s.base().patch(s.border());
    for y in divider.area.top()..divider.area.bottom() {
        for x in divider.area.left()..divider.area.right() {
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// Where dividers meet, the lines join (`├ ┤ ┬ ┴ ┼`), so they read as
/// one structure: each divider cell takes the arms its neighbours carry.
fn join_dividers(dividers: &[Divider], buf: &mut Buffer) {
    let on = |split: Split, x: u16, y: u16| {
        dividers
            .iter()
            .any(|d| d.split == split && d.area.contains(Position::new(x, y)))
    };
    for d in dividers {
        for y in d.area.top()..d.area.bottom() {
            for x in d.area.left()..d.area.right() {
                let symbol = match d.split {
                    Split::Vertical => {
                        let left = x
                            .checked_sub(1)
                            .is_some_and(|l| on(Split::Horizontal, l, y));
                        let right = on(Split::Horizontal, x.saturating_add(1), y);
                        match (left, right) {
                            (true, true) => "┼",
                            (true, false) => "┤",
                            (false, true) => "├",
                            (false, false) => continue,
                        }
                    }
                    Split::Horizontal => {
                        let up = y.checked_sub(1).is_some_and(|u| on(Split::Vertical, x, u));
                        let down = on(Split::Vertical, x, y.saturating_add(1));
                        match (up, down) {
                            (true, true) => "┼",
                            (true, false) => "┴",
                            (false, true) => "┬",
                            (false, false) => continue,
                        }
                    }
                };
                if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                    cell.set_symbol(symbol);
                }
            }
        }
    }
}

/// A pane's place in its tab (for the title).
#[derive(Debug, Clone, Copy)]
struct Place {
    number: usize,
    count: usize,
    zoomed: bool,
}

/// `▍web-01  ● Connected · 12 ms          2/4` (focused), dimmed otherwise.
fn render_title(
    app: &App,
    slot: &PaneSlot,
    focused: bool,
    place: Place,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let s = app.styles();
    let width = usize::from(area.width);
    let PaneStatus {
        dot,
        color: dot_color,
        label,
        latency,
        failed,
    } = pane_status(app, slot);
    let latency_style = match &latency {
        Some((_, true)) => s.status(s.theme.status.warning),
        _ => s.muted(),
    };
    let (surface, alias_style, text_style) = if focused {
        (s.chrome(), s.title(), s.secondary())
    } else {
        (s.base(), s.muted(), s.muted())
    };
    // A failure reads the same in every pane, focused or not.
    let label_style = if failed {
        s.status(dot_color)
    } else {
        text_style
    };
    let lead = if focused {
        Span::styled("▍", s.accent())
    } else {
        Span::raw(" ")
    };
    // A leading space keeps the counter clear of the latency when a long
    // alias fills the row.
    let right = match (focused, place.zoomed) {
        (true, true) => format!(" zoom {}/{} ", place.number, place.count),
        (true, false) => format!(" {}/{} ", place.number, place.count),
        _ => String::new(),
    };
    // What the status needs: `● label · 12 ms`; the label goes first when
    // the pane is narrow, the alias keeps at least a few characters.
    let latency_text = latency.map(|(t, _)| t);
    let status_width = |with_label: bool| {
        2 + if with_label { label.chars().count() } else { 0 }
            + latency_text
                .as_ref()
                .map_or(0, |t| t.chars().count() + if with_label { 3 } else { 0 })
    };
    let fixed = 1 + 2 + right.chars().count();
    let with_label = width >= fixed + status_width(true) + 8;
    let alias_room = width
        .saturating_sub(fixed + status_width(with_label))
        .max(4);
    let mut spans = vec![
        lead,
        Span::styled(fmt::truncate(slot.alias(), alias_room), alias_style),
        Span::raw("  "),
        Span::styled(format!("{dot} "), s.status(dot_color)),
    ];
    if with_label {
        spans.push(Span::styled(label, label_style));
    }
    if let Some(text) = latency_text {
        if with_label {
            spans.push(Span::styled(" · ", s.muted()));
        }
        spans.push(Span::styled(text, latency_style));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if used + right.chars().count() <= width {
        spans.push(Span::raw(" ".repeat(width - used - right.chars().count())));
        spans.push(Span::styled(right, s.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(surface), area);
}

/// What a pane's title (and its tab, when it has the focus) shows.
pub(super) struct PaneStatus {
    pub(super) dot: &'static str,
    pub(super) color: portico_theme::Rgb,
    pub(super) label: String,
    /// Latency, and whether it is slow.
    pub(super) latency: Option<(String, bool)>,
    /// The connection failed: filled dot, phase in the dot's colour.
    pub(super) failed: bool,
}

/// The status of a pane, shared by its title and its tab so both say the
/// same: `●` + `status-error` for a failure, `○` for no shell attached.
pub(super) fn pane_status(app: &App, slot: &PaneSlot) -> PaneStatus {
    let s = app.styles();
    let view = slot.session().and_then(|id| app.session_by_id(id));
    let session_failed = view.is_some_and(|v| matches!(v.phase, SessionPhase::Failed { .. }));
    let st = status::status(view, s);
    let idle = s.theme.status.idle;
    let plain = |dot, color, label| PaneStatus {
        dot,
        color,
        label,
        latency: None,
        failed: false,
    };
    let failure = |color, label| PaneStatus {
        failed: true,
        ..plain("●", color, label)
    };
    match slot {
        // The session's failure (or refusal) says it best.
        PaneSlot::Failed { .. } if session_failed => failure(st.color, st.label),
        PaneSlot::Failed { .. } => failure(s.theme.status.error, "Failed".to_owned()),
        PaneSlot::Live(view) if view.ended.is_some() || view.closed => match &view.ended {
            Some(Ended::Exited(code)) => plain("○", idle, format!("Exited ({code})")),
            Some(Ended::Killed(sig)) => plain("○", idle, format!("Killed (SIG{sig})")),
            _ if session_failed => failure(st.color, st.label),
            _ => plain("○", idle, "Closed".to_owned()),
        },
        PaneSlot::Opening { .. } | PaneSlot::Live(_) => {
            let dot = if st.busy {
                super::spinner(app.tick)
            } else {
                "●"
            };
            PaneStatus {
                latency: st.latency.map(|l| (l, st.slow)),
                ..plain(dot, st.color, st.label)
            }
        }
    }
}

fn render_body(app: &App, slot: &PaneSlot, focused: bool, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let close = app.keymap.shown("prefix.close_pane", 1);
    let failure = app.pane_failure(slot);
    match slot {
        PaneSlot::Live(view) => {
            // A failed connection gets the full card, not the one-line banner.
            let banner = failure.as_ref().is_none_or(|f| f.error.is_none());
            super::terminal::render(view, focused, banner, &close, s, frame, area);
            if let Some(failure) = failure.filter(|f| f.error.is_some()) {
                render_failure(app, &failure, focused, frame, area);
            }
        }
        PaneSlot::Opening { alias, session } => {
            frame.render_widget(Block::new().style(s.base()), area);
            let st = status::status(app.session_by_id(*session), s);
            let lines = vec![
                Line::from(vec![
                    Span::styled(super::spinner(app.tick), s.status(st.color)),
                    Span::styled(format!("  {}", st.label), s.secondary()),
                ]),
                Line::from(Span::styled(
                    format!("   Opening a shell on {}", fmt::truncate(alias, 32)),
                    s.muted(),
                )),
            ];
            render_centered(lines, frame, area);
        }
        PaneSlot::Failed { .. } => {
            frame.render_widget(Block::new().style(s.base()), area);
            if let Some(failure) = failure {
                render_failure(app, &failure, focused, frame, area);
            }
        }
    }
}

/// The error, its hint and the next keys, centred in the pane.
fn render_failure(
    app: &App,
    failure: &crate::app_panes::PaneFailure,
    focused: bool,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let s = app.styles();
    let width = usize::from(area.width.saturating_sub(4)).max(8);
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &failure.error {
        Some(error) => {
            let message = if error.code == "host_key_changed" {
                "Host key changed: possible man-in-the-middle.".to_owned()
            } else {
                fmt::sentence(&error.message)
            };
            let message_style = if error.code == "host_key_rejected" {
                s.secondary()
            } else {
                s.status(s.theme.status.error)
            };
            lines.extend(
                fmt::wrap(&message, width)
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, message_style))),
            );
            if let Some(hint) = error.hint.as_deref().filter(|_| failure.retry) {
                lines.extend(
                    fmt::wrap(hint, width)
                        .into_iter()
                        .map(|l| Line::from(Span::styled(l, s.secondary()))),
                );
            }
        }
        None => lines.push(Line::from(Span::styled("Connection closed", s.secondary()))),
    }
    lines.push(Line::from(""));
    // Each key with its label; side by side when they fit, else stacked.
    let mut keys: Vec<Vec<Span<'static>>> = Vec::new();
    if failure.retry && focused {
        keys.push(vec![
            Span::styled(" ↵ ", s.key_primary()),
            Span::styled(" retry", s.muted()),
        ]);
    } else if failure.retry {
        // `↵` goes to the focused pane: say how to get here first.
        let arrows = app.keymap.joined(&[
            "prefix.pane_left",
            "prefix.pane_down",
            "prefix.pane_up",
            "prefix.pane_right",
        ]);
        let text = if arrows.is_empty() {
            "focus to retry".to_owned()
        } else {
            format!("focus to retry (Ctrl-Space {arrows})")
        };
        lines.extend(
            fmt::wrap(&text, width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, s.muted()))),
        );
    }
    keys.push(vec![
        Span::styled(
            format!(" Ctrl-Space {} ", app.keymap.shown("prefix.close_pane", 1)),
            s.key_hint(),
        ),
        Span::styled(" close", s.muted()),
    ]);
    let gap = 3;
    let total: usize = keys
        .iter()
        .map(|k| k.iter().map(Span::width).sum::<usize>())
        .sum::<usize>()
        + gap * keys.len().saturating_sub(1);
    if total <= width {
        let mut line = Vec::new();
        for (i, key) in keys.into_iter().enumerate() {
            if i > 0 {
                line.push(Span::raw(" ".repeat(gap)));
            }
            line.extend(key);
        }
        lines.push(Line::from(line));
    } else {
        lines.extend(keys.into_iter().map(Line::from));
    }
    frame.render_widget(Block::new().style(s.base()), area);
    render_centered(lines, frame, area);
}

/// `lines`, left-aligned as a block, centred vertically, inset 2 columns.
fn render_centered(lines: Vec<Line<'static>>, frame: &mut Frame<'_>, area: Rect) {
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(area.height);
    let y = area.y + area.height.saturating_sub(height) / 2;
    let inset = 2.min(area.width);
    let block = Rect {
        x: area.x + inset,
        y,
        width: area.width - inset,
        height,
    };
    frame.render_widget(Paragraph::new(lines), block);
}
