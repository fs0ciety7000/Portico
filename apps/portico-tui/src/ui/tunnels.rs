//! Tunnels overlay: running tunnels (every session), forwards suggested by
//! `ssh_config` for the current host, the add form and the exposure
//! confirmation. Errors show their message and hint on separate, wrapped
//! lines, above the key hints (which stay visible).

use portico_core::ErrorSummary;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::theme::Styles;
use crate::tunnels::{Item, TunnelRow, TunnelStatus, TunnelsPanel};
use crate::ui::fmt;

pub(crate) fn render(
    panel: &mut TunnelsPanel,
    rows: &[TunnelRow],
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    band: u16,
) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(84);
    // Borders (2) + horizontal padding (4).
    let inner_width = usize::from(width.saturating_sub(6));
    let form = form_lines(panel, rows, s, tick, inner_width);
    let form_height = u16::try_from(form.len()).unwrap_or(u16::MAX);
    let list_rows = u16::try_from(panel.items.len().max(2))
        .unwrap_or(12)
        .min(12);
    // Borders, top padding, list, form, one spare row.
    let height = list_rows
        .saturating_add(form_height)
        .saturating_add(4)
        .min(screen.height.saturating_sub(2));
    let area = crate::ui::panel_rect(screen, width, height, band);

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

    // The list gives way first: the form (errors, hints) stays readable.
    let [list_area, form_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(form_height)]).areas(inner);
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
        // While typing or confirming, the list is context, not a cursor.
        let browsing = panel.input.is_none() && panel.confirm.is_none();
        let selected = panel.list.selected().filter(|_| browsing);
        let items: Vec<ListItem<'_>> = panel
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let marker = if selected == Some(i) {
                    Span::styled("▍", s.accent())
                } else {
                    Span::raw(" ")
                };
                let mut line = item_line(item, rows, s, tick, w.saturating_sub(1));
                line.spans.insert(0, marker);
                ListItem::new(line)
            })
            .collect();
        let list = List::new(items);
        if browsing {
            frame.render_stateful_widget(
                list.highlight_style(s.selected()),
                list_area,
                &mut panel.list,
            );
        } else {
            // Same scroll offset, no highlight.
            let mut state = ListState::default().with_offset(panel.list.offset());
            frame.render_stateful_widget(list, list_area, &mut state);
        }
    }
    frame.render_widget(Paragraph::new(form), form_area);
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
                // The full message and its hint show under the list when
                // the row is selected.
                TunnelStatus::Failed(error) => ("●", status.error, fmt::sentence(&error.message)),
            };
            let spec = row.spec.to_string();
            let alias = fmt::truncate(&row.alias, 14);
            // Listening beyond loopback: said in the warning colour.
            let exposed = if row.spec.is_exposed() {
                "  exposed"
            } else {
                ""
            };
            let room = width.saturating_sub(
                spec.chars().count() + alias.chars().count() + exposed.chars().count() + 7,
            );
            let detail_style = if matches!(row.status, TunnelStatus::Failed(_)) {
                s.status(status.error)
            } else {
                s.muted()
            };
            Line::from(vec![
                Span::styled(format!("{dot} "), s.status(color)),
                Span::styled(spec, s.title()),
                Span::styled(exposed, s.status(status.warning)),
                Span::styled(format!("  {alias}  "), s.secondary()),
                Span::styled(fmt::truncate(&detail, room), detail_style),
            ])
        }
        Item::Suggested(spec) => Line::from(vec![
            Span::styled("○ ", s.muted()),
            Span::styled(spec.to_string(), s.secondary()),
            Span::styled("  from ssh_config", s.muted()),
        ]),
    }
}

/// Message (error colour) then hint (secondary), each wrapped to `width`.
fn error_lines(error: &ErrorSummary, s: Styles, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = fmt::wrap(&fmt::sentence(&error.message), width)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, s.status(s.theme.status.error))))
        .collect();
    if let Some(hint) = &error.hint {
        lines.extend(
            fmt::wrap(hint, width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, s.secondary()))),
        );
    }
    lines
}

/// Add form or exposure confirmation, then the current error (the panel's,
/// else the selected failed tunnel's), then the keys that work right now.
fn form_lines(
    panel: &TunnelsPanel,
    rows: &[TunnelRow],
    s: Styles,
    tick: u64,
    width: usize,
) -> Vec<Line<'static>> {
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
        lines.push(Line::from(Span::styled(
            "Anyone on the network can reach it.",
            s.secondary(),
        )));
    } else if let Some(input) = &panel.input {
        let cursor = if tick % 12 < 8 { "▏" } else { " " };
        lines.push(Line::from(vec![
            Span::styled("New  ", s.secondary()),
            Span::styled(input.clone(), s.title()),
            Span::styled(cursor, s.accent()),
        ]));
        lines.push(Line::from(Span::styled(
            "-L local:host:port  ·  -R remote:host:port",
            s.muted(),
        )));
    }
    let failed_row = match panel.selected() {
        Some(Item::Running(id)) if panel.input.is_none() && panel.confirm.is_none() => rows
            .iter()
            .find(|r| r.id == *id)
            .and_then(|r| match &r.status {
                TunnelStatus::Failed(e) => Some(e),
                _ => None,
            }),
        _ => None,
    };
    if let Some(error) = panel.error.as_ref().or(failed_row) {
        lines.extend(error_lines(error, s, width));
    }
    // `Esc close` already sits in the border.
    let hints: Vec<_> = panel
        .hints(rows)
        .into_iter()
        .filter(|(k, l)| !(*k == "Esc" && *l == "close"))
        .collect();
    if !hints.is_empty() {
        // Typing or confirming: the first key is the primary action.
        let modal = panel.input.is_some() || panel.confirm.is_some();
        let mut spans = Vec::new();
        for (i, (k, l)) in hints.into_iter().enumerate() {
            spans.push(if modal && i == 0 { primary(k) } else { key(k) });
            spans.push(label(l));
        }
        lines.push(Line::from(spans));
    }
    lines
}
