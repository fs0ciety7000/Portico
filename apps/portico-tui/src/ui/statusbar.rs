//! Bottom bar: mode pill + contextual key hints.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};
use crate::theme::c;

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let p = s.theme.palette;
    let (mode, hints): (&str, &[(&str, &str)]) = match app.focus {
        Focus::Hosts => (
            " HOSTS ",
            &[
                ("↵", "connect"),
                ("/", "search"),
                ("t", "theme"),
                ("?", "help"),
                ("q", "quit"),
            ],
        ),
        Focus::Search => (
            " SEARCH ",
            &[
                ("↵", "connect"),
                ("↑↓", "move"),
                ("#tag", "filter"),
                ("esc", "done"),
            ],
        ),
    };
    let mut spans = vec![
        Span::styled(
            mode,
            ratatui::style::Style::new()
                .bg(c(p.accent))
                .fg(c(p.text_on_accent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    for (key, label) in hints {
        spans.push(Span::styled(format!(" {key} "), s.key_hint()));
        spans.push(Span::styled(format!(" {label}  "), s.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
}
