//! Bottom bar: mode pill + contextual key hints, and, in a terminal, the
//! session's status on the right.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Focus};
use crate::ui::{fmt, status};

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let (mode, hints) = mode_and_hints(app);
    let pill = if app.prefix && app.dialogs.is_empty() {
        s.prefix_pill()
    } else {
        s.mode_pill()
    };
    let mut spans = vec![Span::styled(mode, pill), Span::raw(" ")];
    for (key, label) in hints {
        spans.push(Span::styled(format!(" {key} "), s.key_hint()));
        spans.push(Span::styled(format!(" {label} "), s.muted()));
    }

    // Right side: `● alias · 12 ms · remote title`, only when it fits.
    if let Some(view) = app
        .active_terminal()
        .filter(|_| matches!(app.focus, Focus::Terminal) || app.prefix)
        .filter(|_| app.focus != Focus::Files)
    {
        let st = status::status(app.session_by_id(view.session), s);
        let mut right = vec![
            Span::styled("● ", s.status(st.color)),
            Span::styled(fmt::truncate(&view.alias, 24), s.secondary()),
        ];
        if let Some(rtt) = st.latency {
            let style = if st.slow {
                s.status(s.theme.status.warning)
            } else {
                s.muted()
            };
            right.push(Span::styled(" · ", s.muted()));
            right.push(Span::styled(rtt, style));
        }
        if let Some(title) = view.remote_title() {
            right.push(Span::styled(" · ", s.muted()));
            right.push(Span::styled(fmt::truncate(&title, 32), s.muted()));
        }
        right.push(Span::raw(" "));
        let used: usize = spans.iter().map(Span::width).sum();
        let need: usize = right.iter().map(Span::width).sum();
        let width = usize::from(area.width);
        if used + need < width {
            spans.push(Span::raw(" ".repeat(width - used - need)));
            spans.extend(right);
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.base()), area);
}

/// Mode pill and the key hints that fit 80 columns (the rest is in `?`).
fn mode_and_hints(app: &App) -> (&'static str, &'static [(&'static str, &'static str)]) {
    match app.focus {
        _ if !app.dialogs.is_empty() => (" DIALOG ", &[("Esc", "cancel / close")]),
        _ if app.prefix => (
            " PREFIX ",
            &[
                ("h", "hosts"),
                ("1-9", "tab"),
                ("n", "new"),
                ("w", "close"),
                ("d", "disconnect"),
                ("Esc", "cancel"),
            ],
        ),
        _ if app.overlay == Some(crate::app::Overlay::Tunnels) => (
            " TUNNELS ",
            &[
                ("a", "add"),
                ("↵", "start"),
                ("x", "stop"),
                ("Esc", "close"),
            ],
        ),
        Focus::Files => (
            " FILES ",
            &[
                ("Tab", "pane"),
                ("↵", "open"),
                ("c", "copy"),
                ("n", "folder"),
                ("d", "delete"),
                ("?", "help"),
                ("Esc", "back"),
            ],
        ),
        Focus::Terminal => (
            " TERMINAL ",
            &[("Ctrl-Space", "commands"), ("Shift-PgUp", "scroll")],
        ),
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
                ("Esc", "done"),
            ],
        ),
    }
}
