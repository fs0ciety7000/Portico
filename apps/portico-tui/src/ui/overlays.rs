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

fn render_help(app: &App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, (section, rows)) in HELP.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(*section, s.title())));
        for (k, d) in *rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<20}"), s.accent()),
                Span::styled(*d, s.secondary()),
            ]));
        }
    }
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3)
        .min(frame.area().height);
    let area = centered(frame.area(), 72, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.border_focus())
                .title(Line::from(Span::styled(" Keyboard shortcuts ", s.title())))
                .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
                .padding(Padding::new(2, 2, 0, 0))
                .style(s.overlay()),
        ),
        area,
    );
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
