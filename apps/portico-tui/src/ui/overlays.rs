//! Modal overlays (help) and toasts.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::{App, Overlay};
use crate::keymap::HELP;

pub(crate) fn render(app: &App, frame: &mut Frame<'_>) {
    if let Some(Overlay::Help) = app.overlay {
        render_help(app, frame);
    }
    render_toast(app, frame);
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

/// Lines of one help section.
fn help_section<'a>(
    s: crate::theme::Styles,
    title: &'a str,
    rows: &'a [(&'a str, &'a str)],
    key_width: usize,
) -> Vec<Line<'a>> {
    let mut lines = vec![Line::from(Span::styled(title, s.title()))];
    for (k, d) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<key_width$}"), s.accent()),
            Span::styled(*d, s.secondary()),
        ]));
    }
    lines
}

/// One column when it fits the screen height, else sections flow into two
/// columns (80×24 stays readable).
fn render_help(app: &App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let screen = frame.area();
    let rows: usize = HELP.iter().map(|(_, r)| r.len() + 2).sum();
    let one_column = rows + 2 <= usize::from(screen.height);
    let (key_width, width) = if one_column { (16, 60) } else { (15, 84) };
    let width = width.min(screen.width.saturating_sub(2));
    let mut columns: Vec<Vec<Line<'_>>> = vec![Vec::new()];
    let half = rows.div_ceil(2);
    for (title, section) in HELP {
        let count = columns.len();
        let used = columns.last().map_or(0, Vec::len);
        if !one_column && used > 0 && used + section.len() > half && count < 2 {
            columns.push(Vec::new());
        }
        if let Some(current) = columns.last_mut() {
            if !current.is_empty() {
                current.push(Line::from(""));
            }
            current.extend(help_section(s, title, section, key_width));
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
fn render_toast(app: &App, frame: &mut Frame<'_>) {
    let Some(toast) = &app.toast else { return };
    let s = app.styles();
    let (icon, icon_style, border) = if toast.is_error {
        let error = s.status(s.theme.status.error);
        ("✕", error, error)
    } else {
        ("·", s.accent(), s.border_strong())
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{icon}  "), icon_style),
        Span::styled(toast.message.clone(), s.title()),
    ])];
    if let Some(hint) = &toast.hint {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(hint.clone(), s.secondary()),
        ]));
    }
    let area = frame.area();
    // Wrap long messages instead of truncating them.
    let max_width = area.width.saturating_sub(4).clamp(20, 72);
    let longest = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(longest)
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .min(max_width);
    // Borders + one column of padding on each side.
    let inner = usize::from(width.saturating_sub(4)).max(1);
    let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .clamp(1, 6)
        .saturating_add(2);
    let rect = Rect {
        x: area.right().saturating_sub(width + 2),
        y: area.bottom().saturating_sub(height + 2),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(border)
                    .padding(Padding::horizontal(1))
                    .style(s.overlay()),
            ),
        rect,
    );
}
