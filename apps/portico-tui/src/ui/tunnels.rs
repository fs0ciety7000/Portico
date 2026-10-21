//! Tunnels overlay: running tunnels (every session), forwards suggested by
//! `ssh_config` for the current host, the add form and the exposure
//! confirmation.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph};

use crate::theme::Styles;
use crate::tunnels::{Item, TunnelRow, TunnelStatus, TunnelsPanel};
use crate::ui::fmt;

pub(crate) fn render(
    panel: &mut TunnelsPanel,
    rows: &[TunnelRow],
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(84);
    let list_rows = u16::try_from(panel.items.len().max(2))
        .unwrap_or(12)
        .min(12);
    // Borders, spacer, list, spacer, form (2), notice.
    let height = list_rows
        .saturating_add(7)
        .min(screen.height.saturating_sub(2));
    let [v] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(screen);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(v);

    let title = match &panel.session {
        Some((_, alias)) => Line::from(vec![
            Span::styled(" Tunnels ", s.title()),
            Span::styled(format!("· {} ", fmt::truncate(alias, 32)), s.muted()),
        ]),
        None => Line::from(Span::styled(" Tunnels ", s.title())),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(title)
        .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [list_area, form_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(inner);
    let w = usize::from(inner.width);

    if panel.items.is_empty() {
        let mut lines = vec![Line::from(Span::styled("No tunnels yet", s.secondary()))];
        lines.push(Line::from(if panel.session.is_some() {
            vec![
                Span::styled("a", s.accent()),
                Span::styled(" forward a port, e.g. ", s.muted()),
                Span::styled("-L 8080:db.internal:5432", s.secondary()),
            ]
        } else {
            vec![Span::styled(
                "Connect to a host, then press p to add a tunnel.",
                s.muted(),
            )]
        }));
        frame.render_widget(Paragraph::new(lines), list_area);
    } else {
        let items: Vec<ListItem<'_>> = panel
            .items
            .iter()
            .map(|item| ListItem::new(item_line(item, rows, s, tick, w)))
            .collect();
        frame.render_stateful_widget(
            List::new(items).highlight_style(s.selected()),
            list_area,
            &mut panel.list,
        );
    }
    frame.render_widget(Paragraph::new(form_lines(panel, s, tick)), form_area);
}

fn item_line(item: &Item, rows: &[TunnelRow], s: Styles, tick: u64, width: usize) -> Line<'static> {
    let status = s.theme.status;
    match item {
        Item::Running(id) => {
            let Some(row) = rows.iter().find(|r| r.id == *id) else {
                return Line::from("");
            };
            let (dot, color, detail) = match &row.status {
                TunnelStatus::Starting => (
                    super::spinner(tick),
                    status.connecting,
                    "starting".to_owned(),
                ),
                TunnelStatus::Listening(bind) => {
                    let conns = match row.connections {
                        0 => String::new(),
                        1 => "  · 1 connection".to_owned(),
                        n => format!("  · {n} connections"),
                    };
                    ("●", status.connected, format!("{bind}{conns}"))
                }
                TunnelStatus::Failed { message, hint } => (
                    "●",
                    status.error,
                    hint.as_ref()
                        .map_or_else(|| message.clone(), |h| format!("{message} — {h}")),
                ),
            };
            let spec = row.spec.to_string();
            let alias = fmt::truncate(&row.alias, 14);
            let room = width.saturating_sub(spec.chars().count() + alias.chars().count() + 8);
            let detail_style = if matches!(row.status, TunnelStatus::Failed { .. }) {
                s.status(status.error)
            } else {
                s.muted()
            };
            Line::from(vec![
                Span::styled(format!(" {dot} "), s.status(color)),
                Span::styled(spec, s.title()),
                Span::styled(format!("  {alias}  "), s.secondary()),
                Span::styled(fmt::truncate(&detail, room), detail_style),
            ])
        }
        Item::Suggested(spec) => Line::from(vec![
            Span::styled(" ○ ", s.muted()),
            Span::styled(spec.to_string(), s.secondary()),
            Span::styled("  from ssh_config · ↵ start", s.muted()),
        ]),
    }
}

/// Add form, exposure confirmation, or the last error.
fn form_lines(panel: &TunnelsPanel, s: Styles, tick: u64) -> Vec<Line<'static>> {
    let key = |k: &str| Span::styled(format!(" {k} "), s.key_hint());
    let primary = |k: &str| Span::styled(format!(" {k} "), s.key_primary());
    let label = |t: &str| Span::styled(format!(" {t}  "), s.muted());
    let warning = s.status(s.theme.status.warning);
    let mut lines = vec![Line::from("")];
    if let Some(spec) = &panel.confirm {
        lines.push(Line::from(vec![
            Span::styled("▲ ", warning),
            Span::styled(spec.to_string(), s.title()),
            Span::styled(" listens beyond this machine", warning),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Anyone on the network can reach it.  ", s.secondary()),
            primary("y"),
            label("expose"),
            key("Esc"),
            label("cancel"),
        ]));
        return lines;
    }
    if let Some(input) = &panel.input {
        let cursor = if tick % 12 < 8 { "▏" } else { " " };
        lines.push(Line::from(vec![
            Span::styled("New  ", s.secondary()),
            Span::styled(input.clone(), s.title()),
            Span::styled(cursor, s.accent()),
        ]));
        lines.push(match &panel.error {
            Some(e) => Line::from(Span::styled(e.clone(), s.status(s.theme.status.error))),
            None => Line::from(vec![
                Span::styled("-L local:host:port  ·  -R remote:host:port   ", s.muted()),
                primary("↵"),
                label("start"),
            ]),
        });
        return lines;
    }
    match &panel.error {
        Some(e) => lines.push(Line::from(Span::styled(
            e.clone(),
            s.status(s.theme.status.error),
        ))),
        None => lines.push(Line::from(vec![
            key("a"),
            label("add"),
            key("↵"),
            label("start suggested"),
            key("x"),
            label("stop"),
        ])),
    }
    lines
}
