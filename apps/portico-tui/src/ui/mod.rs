//! Pure rendering: `render(&App) -> Frame`. No I/O here.

mod details;
mod hosts;
mod overlays;
mod status;
mod statusbar;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

/// Braille spinner, advanced by `App::tick`.
pub(crate) const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

pub(crate) fn spinner(tick: u64) -> &'static str {
    // `tick % len` is always in bounds.
    let i = usize::try_from(tick % SPINNER.len() as u64).unwrap_or(0);
    SPINNER.get(i).copied().unwrap_or("·")
}

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>) {
    let s = app.styles();
    frame.render_widget(Block::new().style(s.base()), frame.area());

    let [header, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(app, frame, header);

    let [sidebar, main] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
            .spacing(1)
            .areas(body.inner(ratatui::layout::Margin::new(1, 0)));
    hosts::render(app, frame, sidebar);
    details::render(app, frame, main);
    statusbar::render(app, frame, status);
    overlays::render(app, frame);
}

fn render_header(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let count = app.inventory.as_ref().map_or(0, |i| i.hosts.len());
    let left = Line::from(vec![
        Span::styled("  ╭┴┴╮ ", s.accent()),
        Span::styled("P O R T I C O", s.title()),
        Span::styled(format!("   {count} hosts"), s.muted()),
    ]);
    let right = Line::from(vec![
        Span::styled(s.theme.name, s.secondary()),
        Span::raw("  "),
    ])
    .right_aligned();
    frame.render_widget(Paragraph::new(left).style(s.base()), area);
    frame.render_widget(Paragraph::new(right), area);
}
