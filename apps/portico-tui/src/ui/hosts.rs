//! Sidebar: search field + host list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, Padding, Paragraph};

use crate::app::{App, Focus};

#[allow(clippy::too_many_lines)] // will split into SearchField + HostList widgets
pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let [search, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);

    // Search field
    let searching = app.focus == Focus::Search;
    let border = if searching {
        s.border_focus()
    } else {
        s.border()
    };
    let content = if app.query.is_empty() && !searching {
        Line::from(vec![
            Span::styled("⌕ ", s.muted()),
            Span::styled("Search hosts", s.muted()),
            Span::styled("   /", s.muted()),
        ])
    } else {
        let cursor = if searching && app.tick % 12 < 8 {
            "▏"
        } else {
            " "
        };
        Line::from(vec![
            Span::styled("⌕ ", s.accent()),
            Span::raw(app.query.clone()),
            Span::styled(cursor, s.accent()),
        ])
    };
    frame.render_widget(
        Paragraph::new(content).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(border)
                .padding(Padding::horizontal(1))
                .style(s.surface()),
        ),
        search,
    );

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if searching {
            s.border()
        } else {
            s.border_focus()
        })
        .title(Line::from(Span::styled(" Hosts ", s.secondary())))
        .padding(Padding::horizontal(1))
        .style(s.surface());

    if app.is_loading() {
        let line = Line::from(vec![
            Span::styled(super::spinner(app.tick), s.accent()),
            Span::styled("  Reading ~/.ssh/config…", s.secondary()),
        ]);
        frame.render_widget(Paragraph::new(line).block(block), list);
        return;
    }
    if let Some(err) = &app.load_error {
        let status = s.theme.status;
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled("Could not load hosts", s.status(status.error))),
                Line::from(Span::styled(err.clone(), s.secondary())),
            ])
            .block(block),
            list,
        );
        return;
    }
    if app.hits.is_empty() {
        let msg = if app.query.is_empty() {
            "No hosts in ~/.ssh/config yet."
        } else {
            "No host matches this search."
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, s.muted()))).block(block),
            list,
        );
        return;
    }

    let Some(inv) = &app.inventory else { return };
    let idle = s.theme.status.idle;
    let items: Vec<ListItem<'_>> = app
        .hits
        .iter()
        .filter_map(|hit| inv.hosts.get(hit.index))
        .map(|h| {
            let mut spans = vec![
                Span::styled("● ", s.status(idle)),
                Span::styled(h.alias.clone(), s.title()),
            ];
            if h.meta.favorite {
                spans.push(Span::styled(" ★", s.accent()));
            }
            let target = match &h.resolved.user {
                Some(u) => format!("  {u}@{}", h.resolved.hostname),
                None => format!("  {}", h.resolved.hostname),
            };
            spans.push(Span::styled(target, s.muted()));
            for t in &h.meta.tags {
                spans.push(Span::styled(format!(" #{t}"), s.secondary()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list_widget = List::new(items)
        .block(block)
        .highlight_style(s.selected())
        .highlight_symbol(Line::from(Span::styled("▍", s.accent())));
    frame.render_stateful_widget(list_widget, list, &mut app.list);
}
