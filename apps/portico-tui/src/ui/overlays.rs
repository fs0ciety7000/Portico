//! Modal overlays (help) and toasts.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::{App, Overlay};
use crate::keymap::HELP;
use crate::ui::fmt;

/// `toast_zone`: the inside of the right-hand panel, so a toast never
/// covers a border.
pub(crate) fn render(app: &App, frame: &mut Frame<'_>, toast_zone: Rect) {
    if let Some(Overlay::Help) = app.overlay {
        render_help(app, frame);
    }
    render_toast(app, frame, toast_zone);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(v);
    h
}

/// Width of the widest key cell in [`HELP`].
fn help_key_width() -> usize {
    HELP.iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
}

/// Lines of one help section; a description that still does not fit its
/// column ends with `…` rather than being cut mid-word.
fn help_section<'a>(
    s: crate::theme::Styles,
    title: &'a str,
    rows: &'a [(&'a str, &'a str)],
    key_width: usize,
    column_width: usize,
) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(Span::styled(title, s.title()))];
    // Indent (2) + key + gap (2).
    let room = column_width.saturating_sub(key_width + 4);
    for (k, d) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<key_width$}  "), s.accent()),
            Span::styled(fmt::truncate(d, room), s.secondary()),
        ]));
    }
    lines
}

/// Height of `sections` stacked in one column, `gap` blank rows apart.
fn stacked(sections: &[(&str, &[(&str, &str)])], gap: usize) -> usize {
    let rows: usize = sections.iter().map(|(_, r)| r.len() + 1).sum();
    rows + gap * sections.len().saturating_sub(1)
}

/// Where to cut [`HELP`] into two columns (sections stay in order) and the
/// gap between sections: the most balanced cut, with a blank row between
/// sections when that still fits `room` rows.
fn help_split(room: usize) -> (usize, usize) {
    let best = |gap: usize| {
        (1..HELP.len())
            .map(|cut| {
                let (left, right) = HELP.split_at(cut);
                (stacked(left, gap).max(stacked(right, gap)), cut)
            })
            .min()
            .unwrap_or((stacked(HELP, gap), HELP.len()))
    };
    let (tallest, cut) = best(1);
    if tallest <= room {
        (cut, 1)
    } else {
        (best(0).1, 0)
    }
}

/// One column when it fits the screen height, else sections flow into two
/// balanced columns (80×24 stays readable).
fn render_help(app: &App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let screen = frame.area();
    let room = usize::from(screen.height.saturating_sub(2));
    let one_column = stacked(HELP, 1) <= room;
    let key_width = help_key_width();
    let width = if one_column { 60 } else { 84 };
    let width = width.min(screen.width.saturating_sub(2));
    // Borders (2) + padding (4), then two columns 2 apart.
    let inner = usize::from(width.saturating_sub(6));
    let column_width = if one_column {
        inner
    } else {
        inner.saturating_sub(2) / 2
    };
    let (cut, gap) = if one_column {
        (HELP.len(), 1)
    } else {
        help_split(room)
    };
    let mut columns: Vec<Vec<Line<'_>>> = vec![Vec::new()];
    for (i, (title, section)) in HELP.iter().enumerate() {
        if i == cut {
            columns.push(Vec::new());
        }
        if let Some(current) = columns.last_mut() {
            if !current.is_empty() && gap > 0 {
                current.push(Line::from(""));
            }
            current.extend(help_section(s, title, section, key_width, column_width));
        }
    }
    let tallest = columns.iter().map(Vec::len).max().unwrap_or(0);
    let height = u16::try_from(tallest)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(screen.height);
    let area = centered(screen, width, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(Span::styled(" Keyboard shortcuts ", s.title())))
        .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 0, 0))
        .style(s.overlay());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let constraints = vec![Constraint::Fill(1); columns.len()];
    let areas = Layout::horizontal(constraints).spacing(2).split(inner);
    for (lines, rect) in columns.into_iter().zip(areas.iter()) {
        frame.render_widget(Paragraph::new(lines), *rect);
    }
}

/// Bottom-right toast: neutral for information, error-coloured for
/// failures (message, then hint on its own line).
fn render_toast(app: &App, frame: &mut Frame<'_>, zone: Rect) {
    let Some(toast) = &app.toast else { return };
    let s = app.styles();
    let (icon, icon_style, border) = if toast.is_error {
        let error = s.status(s.theme.status.error);
        ("✕", error, error)
    } else {
        ("·", s.accent(), s.border_strong())
    };
    // Too narrow a panel: fall back to the screen, above the status bar.
    let area = if zone.width >= 24 && zone.height >= 4 {
        zone
    } else {
        let screen = frame.area();
        Rect {
            height: screen.height.saturating_sub(1),
            ..screen
        }
    };
    // Borders + one column of padding on each side, then the icon column.
    let max_width = area.width.saturating_sub(1).min(72);
    let text_room = usize::from(max_width.saturating_sub(4 + 3)).max(1);
    // Wrapped, not truncated; continuation lines align under the text.
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, part) in fmt::wrap(&toast.message, text_room).into_iter().enumerate() {
        let lead = if i == 0 {
            Span::styled(format!("{icon}  "), icon_style)
        } else {
            Span::raw("   ")
        };
        lines.push(Line::from(vec![lead, Span::styled(part, s.title())]));
    }
    if let Some(hint) = &toast.hint {
        for part in fmt::wrap(hint, text_room) {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(part, s.secondary()),
            ]));
        }
    }
    let longest = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(longest)
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .min(max_width);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .clamp(1, 6)
        .saturating_add(2)
        .min(area.height);
    // Bottom-right of the zone, one column from its edge.
    let rect = Rect {
        x: area.right().saturating_sub(width + 1).max(area.x),
        y: area.bottom().saturating_sub(height).max(area.y),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(border)
                .padding(Padding::horizontal(1))
                .style(s.overlay()),
        ),
        rect,
    );
}
