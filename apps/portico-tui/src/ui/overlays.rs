//! Modal overlays (help) and toasts.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::{App, Overlay};
use crate::keymap::{self, HelpSection};
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
/// Widest key cell shown (longer rebound keys are cut).
const MAX_KEY_WIDTH: usize = 14;

/// Width of the widest key cell.
fn help_key_width(help: &[HelpSection]) -> usize {
    help.iter()
        .flat_map(|s| s.rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .min(MAX_KEY_WIDTH)
}

/// Lines of one help section; a description that still does not fit its
/// column ends with `…` rather than being cut mid-word.
fn help_section(
    s: crate::theme::Styles,
    section: &HelpSection,
    key_width: usize,
    column_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(section.title, s.title()))];
    // Indent (2) + key + gap (2).
    let room = column_width.saturating_sub(key_width + 4);
    for (k, d) in &section.rows {
        let k = fmt::truncate(k, key_width);
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<key_width$}  "), s.accent()),
            Span::styled(fmt::truncate(d, room), s.secondary()),
        ]));
    }
    lines
}

/// Height of `sections` stacked in one column, `gap` blank rows apart.
fn stacked(sections: &[HelpSection], gap: usize) -> usize {
    let rows: usize = sections.iter().map(|s| s.rows.len() + 1).sum();
    rows + gap * sections.len().saturating_sub(1)
}

/// Where to cut the sections into two columns (in order) and the gap
/// between sections: the most balanced cut, with a blank row between
/// sections when that still fits `room` rows.
fn help_split(help: &[HelpSection], room: usize) -> (usize, usize) {
    let best = |gap: usize| {
        (1..help.len())
            .map(|cut| {
                let (left, right) = help.split_at(cut);
                (stacked(left, gap).max(stacked(right, gap)), cut)
            })
            .min()
            .unwrap_or((stacked(help, gap), help.len()))
    };
    let (tallest, cut) = best(1);
    if tallest <= room {
        (cut, 1)
    } else {
        (best(0).1, 0)
    }
}

/// One column when it fits the screen height, else sections flow into two
/// balanced columns (80×24 stays readable). Keys are the effective ones.
fn render_help(app: &App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let help = keymap::help(&app.keymap);
    let screen = frame.area();
    let room = usize::from(screen.height.saturating_sub(2));
    let one_column = stacked(&help, 1) <= room;
    let key_width = help_key_width(&help);
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
        (help.len(), 1)
    } else {
        help_split(&help, room)
    };
    let mut columns: Vec<Vec<Line<'_>>> = vec![Vec::new()];
    for (i, section) in help.iter().enumerate() {
        if i == cut {
            columns.push(Vec::new());
        }
        if let Some(current) = columns.last_mut() {
            if !current.is_empty() && gap > 0 {
                current.push(Line::from(""));
            }
            current.extend(help_section(s, section, key_width, column_width));
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
    let screen = frame.area();
    let make = |width| {
        ToastBox::new(
            s,
            toast.is_error,
            &toast.message,
            toast.hint.as_deref(),
            width,
        )
    };
    let in_zone = make(zone.width);
    if zone.width >= 24 && in_zone.fits(zone) {
        in_zone.draw(s, frame, zone);
        return;
    }
    // The zone is too small: the screen, above the status bar.
    let area = Rect {
        height: screen.height.saturating_sub(1),
        ..screen
    };
    make(screen.width).draw(s, frame, area);
}

/// A toast, laid out: wrapped lines and the box size they need.
pub(crate) struct ToastBox {
    lines: Vec<Line<'static>>,
    is_error: bool,
    pub(crate) height: u16,
    width: u16,
}

impl ToastBox {
    /// At most 72 columns (and the screen width), at most 6 text rows.
    pub(crate) fn new(
        s: crate::theme::Styles,
        is_error: bool,
        message: &str,
        hint: Option<&str>,
        screen_width: u16,
    ) -> Self {
        let (icon, icon_style) = if is_error {
            ("✕", s.status(s.theme.status.error))
        } else {
            ("·", s.accent())
        };
        // Borders + one column of padding on each side, then the icon column.
        let max_width = screen_width.saturating_sub(2).min(72);
        let text_room = usize::from(max_width.saturating_sub(4 + 3)).max(1);
        // Wrapped, not truncated; continuation lines align under the text.
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (i, part) in fmt::wrap(message, text_room).into_iter().enumerate() {
            let lead = if i == 0 {
                Span::styled(format!("{icon}  "), icon_style)
            } else {
                Span::raw("   ")
            };
            lines.push(Line::from(vec![lead, Span::styled(part, s.title())]));
        }
        if let Some(hint) = hint {
            for part in fmt::wrap(hint, text_room) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(part, s.secondary()),
                ]));
            }
        }
        lines.truncate(6);
        let longest = lines.iter().map(Line::width).max().unwrap_or(0);
        let width = u16::try_from(longest)
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .min(max_width);
        let height = u16::try_from(lines.len().max(1))
            .unwrap_or(6)
            .saturating_add(2);
        Self {
            lines,
            is_error,
            height,
            width,
        }
    }

    /// The box fits in `zone`, one column clear of its right edge.
    pub(crate) fn fits(&self, zone: Rect) -> bool {
        zone.width > self.width && zone.height >= self.height
    }

    /// Bottom-right of `zone`, one column from its edge.
    pub(crate) fn draw(self, s: crate::theme::Styles, frame: &mut Frame<'_>, zone: Rect) {
        let width = self.width.min(zone.width);
        let height = self.height.min(zone.height);
        let rect = Rect {
            x: zone.right().saturating_sub(width + 1).max(zone.x),
            y: zone.bottom().saturating_sub(height).max(zone.y),
            width,
            height,
        };
        let border = if self.is_error {
            s.status(s.theme.status.error)
        } else {
            s.border_strong()
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(self.lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border)
                    .padding(Padding::horizontal(1))
                    .style(s.overlay()),
            ),
            rect,
        );
    }
}
