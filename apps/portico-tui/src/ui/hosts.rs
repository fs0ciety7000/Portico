//! Sidebar: search field + host list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, Padding, Paragraph};

use portico_core::hosts::HostOrigin;

use crate::app::{App, Focus};
use crate::host_list::{Entry, section_title};

/// The list's origin mark: `◆` a Portico host (editable), `◇` an imported
/// one; none for `~/.ssh/config` (read-only, the usual case).
pub(crate) fn origin_mark(origin: HostOrigin) -> Option<&'static str> {
    match origin {
        HostOrigin::Portico => Some("◆"),
        HostOrigin::Imported => Some("◇"),
        HostOrigin::SshConfig => None,
    }
}

/// Hosts listed under the section header at `header`.
fn section_size(entries: &[Entry], header: usize) -> usize {
    entries
        .iter()
        .skip(header.saturating_add(1))
        .take_while(|e| e.host().is_some())
        .count()
}

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
        .title(if app.pane_pick.is_some() {
            // `Ctrl-Space S`: the list chooses the new pane's host.
            Line::from(Span::styled(" Split with host… ", s.accent()))
        } else {
            Line::from(Span::styled(" Hosts ", s.secondary()))
        })
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
                Line::from(Span::styled(super::fmt::sentence(err), s.secondary())),
            ])
            .block(block),
            list,
        );
        return;
    }
    if app.entries.is_empty() {
        let lines = if app.query.is_empty() {
            vec![
                Line::from(Span::styled("No hosts yet.", s.secondary())),
                Line::from(Span::styled(
                    format!(
                        "{} adds one here, or add a Host block to ~/.ssh/config, then {} reloads.",
                        app.keymap.shown("hosts.add", 1),
                        app.keymap.shown("hosts.reload", 1)
                    ),
                    s.muted(),
                )),
            ]
        } else {
            vec![
                Line::from(Span::styled("No host matches this search.", s.secondary())),
                Line::from(Span::styled(
                    "Esc clears it · #tag * @group filter",
                    s.muted(),
                )),
            ]
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .block(block),
            list,
        );
        return;
    }

    let Some(inv) = &app.inventory else { return };
    // Leave room for the dot, a favourite star and some of the target.
    let alias_max = usize::from(area.width).saturating_sub(10).max(8);
    // A section row: borders and padding (4), the selection symbol column
    // and the space after it (2), then the title and its count on the right.
    let row_width = usize::from(list.width).saturating_sub(6);
    // A host row: the same, minus the leading space of a section title.
    let host_width = row_width.saturating_add(1);
    let items: Vec<ListItem<'_>> = app
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| match entry {
            Entry::Header(title) => Some(Err((title, section_size(&app.entries, i)))),
            Entry::Host { index, .. } => inv.hosts.get(*index).map(Ok),
        })
        .map(|row| {
            let h = match row {
                Ok(h) => h,
                Err((title, count)) => {
                    let count = count.to_string();
                    let title_max = row_width.saturating_sub(count.len() + 2);
                    let title = section_title(title, title_max);
                    let pad = row_width.saturating_sub(title.chars().count() + count.len());
                    return ListItem::new(Line::from(vec![
                        Span::styled(format!(" {title}"), s.muted()),
                        Span::raw(" ".repeat(pad)),
                        Span::styled(count, s.muted()),
                    ]));
                }
            };
            let st = super::status::status(app.session_for(&h.alias), s);
            let dot = if st.busy {
                super::spinner(app.tick)
            } else {
                "●"
            };
            let mut spans = vec![
                Span::styled(format!("{dot} "), s.status(st.color)),
                Span::styled(super::fmt::truncate(&h.alias, alias_max), s.title()),
            ];
            // Origin, discreetly: Portico's own hosts (editable) and
            // imported ones; `~/.ssh/config` hosts carry no mark.
            if let Some(mark) = origin_mark(h.origin) {
                spans.push(Span::styled(format!(" {mark}"), s.muted()));
            }
            if h.meta.favorite {
                spans.push(Span::styled(" ★", s.accent()));
            }
            // A destination received through sync waits for this device.
            if app.sync.pending_for(&h.alias).is_some() {
                spans.push(Span::styled(" ⇅", s.status(s.theme.status.warning)));
            }
            let target = match &h.resolved.user {
                Some(u) => format!("{u}@{}", h.resolved.hostname),
                None => h.resolved.hostname.clone(),
            };
            // What is left of the row (after the selection column): the
            // target cut with an ellipsis, then the tags that fit whole.
            let used: usize = spans.iter().map(Span::width).sum();
            let mut left = host_width.saturating_sub(used);
            if left > 3 {
                let target = super::fmt::truncate(&target, left - 2);
                left = left.saturating_sub(target.chars().count() + 2);
                spans.push(Span::styled(format!("  {target}"), s.muted()));
                for t in &h.meta.tags {
                    let tag = format!(" #{t}");
                    let width = tag.chars().count();
                    if width > left {
                        break;
                    }
                    left -= width;
                    spans.push(Span::styled(tag, s.secondary()));
                }
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
