//! Pure rendering: `render(&App) -> Frame`. No I/O here.

mod details;
mod dialog;
mod hosts;
mod overlays;
mod status;
mod statusbar;
mod terminal;

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
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(app, frame, header);

    // The terminal fills the body edge to edge: fidelity first.
    app.term_size = crate::terminal::GridSize {
        cols: body.width.max(1),
        rows: body.height.max(1),
    };
    if let Some(view) = app.active_terminal() {
        let focused = app.dialogs.is_empty() && app.overlay.is_none();
        terminal::render(view, focused, s, frame, body);
    } else {
        let [sidebar, main] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .spacing(1)
                .areas(body.inner(ratatui::layout::Margin::new(1, 0)));
        hosts::render(app, frame, sidebar);
        details::render(app, frame, main);
    }
    statusbar::render(app, frame, status);
    overlays::render(app, frame);
    if let Some(d) = app.dialogs.front() {
        dialog::render(d, s, frame);
    }
}

/// Brand, then tabs: `Hosts │ 1 web-01 ● │ 2 db ●`, theme on the right.
fn render_header(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let mut spans = vec![
        Span::styled(" ╭┴┴╮ ", s.accent()),
        Span::styled("PORTICO", s.title()),
        Span::styled("  ", s.base()),
    ];
    let tab = |label: String, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                s.selected().add_modifier(ratatui::style::Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), s.muted())
        }
    };
    let count = app.inventory.as_ref().map_or(0, |i| i.hosts.len());
    spans.push(tab(format!("Hosts {count}"), app.active.is_none()));
    for (i, view) in app.terminals.iter().enumerate() {
        let st = status::status(app.session_by_id(view.session), s);
        let dead = view.closed || view.ended.is_some();
        let dot = if dead {
            "○"
        } else if st.busy {
            spinner(app.tick)
        } else {
            "●"
        };
        spans.push(Span::styled("│", s.border()));
        spans.push(tab(
            format!("{} {}", i + 1, truncate(&view.label(), 18)),
            app.active == Some(i),
        ));
        spans.push(Span::styled(
            format!("{dot} "),
            s.status(if dead { s.theme.status.idle } else { st.color }),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
    let right =
        Line::from(vec![Span::styled(s.theme.name, s.muted()), Span::raw("  ")]).right_aligned();
    frame.render_widget(Paragraph::new(right), area);
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut t: String = text.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
