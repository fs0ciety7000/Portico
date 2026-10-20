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
    let height = u16::try_from(HELP.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4);
    let area = centered(frame.area(), 68, height);
    let lines: Vec<Line<'_>> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:<16}"), s.accent()),
                Span::styled(*d, s.secondary()),
            ])
        })
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.border_focus())
                .title(Line::from(Span::styled(" Keyboard shortcuts ", s.title())))
                .title_bottom(Line::from(Span::styled(" esc to close ", s.muted())).right_aligned())
                .padding(Padding::new(2, 2, 1, 0))
                .style(s.overlay()),
        ),
        area,
    );
}

fn render_toast(app: &App, frame: &mut Frame<'_>) {
    let Some(toast) = &app.toast else { return };
    let s = app.styles();
    let status = s.theme.status;
    let (icon, color) = if toast.is_error {
        ("✕", status.error)
    } else {
        ("✓", status.connected)
    };
    let area = frame.area();
    // Wrap long messages instead of truncating them.
    let max_width = area.width.saturating_sub(4).clamp(20, 72);
    let text_len = u16::try_from(toast.message.chars().count()).unwrap_or(u16::MAX);
    let width = text_len.saturating_add(9).min(max_width);
    let inner = width.saturating_sub(7).max(1);
    let lines = text_len.div_ceil(inner).clamp(1, 4);
    let height = lines + 2;
    let rect = Rect {
        x: area.right().saturating_sub(width + 2),
        y: area.bottom().saturating_sub(height + 2),
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{icon}  "), s.status(color)),
            Span::raw(toast.message.clone()),
        ]))
        .wrap(ratatui::widgets::Wrap { trim: true })
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.status(color))
                .padding(Padding::horizontal(1))
                .style(s.overlay()),
        ),
        rect,
    );
}
