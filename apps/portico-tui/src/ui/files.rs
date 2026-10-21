//! SFTP explorer: local | remote panes, the transfer queue, and one line for
//! the current question or the last error. Server-provided names go through
//! `display_safe` (no control or bidi characters reach the terminal).

use portico_core::sftp::{Direction, EntryKind, RemoteEntry, display_safe};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, Padding, Paragraph};
use time::OffsetDateTime;

use crate::files::{FilesPrompt, FilesView, Pane, Side, TransferRow, TransferStatus, human_size};
use crate::theme::Styles;
use crate::ui::fmt;

/// Transfers listed at most (most recent last).
const QUEUE_ROWS: usize = 4;
/// Width of the text progress bar.
const BAR: u16 = 14;

pub(crate) fn render(
    view: &mut FilesView,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let shown = view.transfers.len().min(QUEUE_ROWS);
    let queue_height = if shown == 0 {
        0
    } else {
        u16::try_from(shown).unwrap_or(4).saturating_add(2)
    };
    let [panes, queue, footer] = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(queue_height),
        Constraint::Length(1),
    ])
    .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(1)
            .areas(panes);

    let hidden = view.hidden;
    let local_title = fmt::display_path(&view.local.cwd);
    let remote_title = format!("{}:{}", view.alias, view.remote.cwd);
    let active = view.side;
    render_pane(
        &mut view.local,
        PaneLook {
            label: "Local",
            path: &local_title,
            focused: active == Side::Local,
            hidden,
            loading_text: "Reading folder…",
        },
        s,
        tick,
        frame,
        left,
    );
    render_pane(
        &mut view.remote,
        PaneLook {
            label: "Remote",
            path: &remote_title,
            focused: active == Side::Remote,
            hidden,
            loading_text: "Opening SFTP…",
        },
        s,
        tick,
        frame,
        right,
    );
    if shown > 0 {
        render_queue(view, s, tick, frame, queue);
    }
    render_footer(view, s, frame, footer);
}

#[derive(Clone, Copy)]
struct PaneLook<'a> {
    label: &'a str,
    path: &'a str,
    focused: bool,
    hidden: bool,
    loading_text: &'a str,
}

fn render_pane(
    pane: &mut Pane,
    look: PaneLook<'_>,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let inner_width = usize::from(area.width.saturating_sub(4));
    // "Remote  host:/path", the path shortened from the left.
    let path_room = inner_width.saturating_sub(look.label.len() + 4);
    let path = tail(&display_safe(look.path, 4096), path_room);
    let title = Line::from(vec![
        Span::styled(
            format!(" {} ", look.label),
            if look.focused {
                s.title()
            } else {
                s.secondary()
            },
        ),
        Span::styled(format!("{path} "), s.muted()),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if look.focused {
            s.border_focus()
        } else {
            s.border()
        })
        .title(title)
        .padding(Padding::horizontal(1))
        .style(s.surface());

    if let Some(lines) = placeholder(pane, look, s, tick, inner_width) {
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .block(block),
            area,
        );
        return;
    }
    let entries: Vec<&RemoteEntry> = pane.visible(look.hidden).collect();
    let show_date = inner_width >= 52;
    let size_w = 9;
    let date_w = if show_date { 13 } else { 0 };
    // Marker (2) + name + size + date.
    let name_w = inner_width.saturating_sub(2 + size_w + date_w).max(4);
    let selected = pane.list.selected();
    let items: Vec<ListItem<'_>> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let is_sel = selected == Some(i);
            let marker = if is_sel && look.focused { "▍ " } else { "  " };
            let (suffix, name_style) = match e.kind {
                EntryKind::Dir => ("/", s.title()),
                EntryKind::Symlink => ("@", s.secondary()),
                EntryKind::File | EntryKind::Other => ("", s.secondary()),
            };
            let name = format!("{}{suffix}", display_safe(&e.name, 256));
            let name = fmt::truncate(&name, name_w.saturating_sub(1));
            let size = if e.kind == EntryKind::Dir {
                String::new()
            } else {
                human_size(e.size)
            };
            let mut spans = vec![
                Span::styled(marker, s.accent()),
                Span::styled(format!("{name:<name_w$}"), name_style),
                Span::styled(format!("{size:>size_w$}"), s.muted()),
            ];
            if show_date {
                spans.push(Span::styled(
                    format!(
                        "{:>date_w$}",
                        e.modified.map(short_date).unwrap_or_default()
                    ),
                    s.muted(),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let highlight = if look.focused {
        s.selected()
    } else {
        ratatui::style::Style::new()
    };
    frame.render_stateful_widget(
        List::new(items).block(block).highlight_style(highlight),
        area,
        &mut pane.list,
    );
}

/// Error, loading and empty states; `None` when there are rows to show.
fn placeholder(
    pane: &Pane,
    look: PaneLook<'_>,
    s: Styles,
    tick: u64,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    if let Some(err) = &pane.error {
        let mut lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                fmt::truncate(&err.message, width.saturating_mul(2)),
                s.status(s.theme.status.error),
            )),
        ];
        if let Some(hint) = &err.hint {
            lines.push(Line::from(Span::styled(hint.clone(), s.secondary())));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(" Ctrl-R ", s.key_hint()),
            Span::styled(" retry  ", s.muted()),
            Span::styled(" Backspace ", s.key_hint()),
            Span::styled(" parent", s.muted()),
        ]));
        return Some(lines);
    }
    if pane.loading && pane.entries.is_empty() {
        return Some(vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(super::spinner(tick), s.accent()),
                Span::styled(format!("  {}", look.loading_text), s.secondary()),
            ]),
        ]);
    }
    if pane.visible(look.hidden).next().is_none() {
        let only_hidden = !pane.entries.is_empty();
        return Some(vec![
            Line::from(""),
            Line::from(Span::styled(
                if only_hidden {
                    "Only hidden files here"
                } else {
                    "Empty folder"
                },
                s.secondary(),
            )),
            Line::from(if only_hidden {
                vec![
                    Span::styled(".", s.accent()),
                    Span::styled(" show them", s.muted()),
                ]
            } else {
                vec![
                    Span::styled("n", s.accent()),
                    Span::styled(" new folder", s.muted()),
                ]
            }),
        ]);
    }
    None
}

/// `12 Mar 14:05` this year, `12 Mar 2023` otherwise (UTC; display only).
fn short_date(at: OffsetDateTime) -> String {
    let month = at.month().to_string();
    let month = month.get(..3).unwrap_or(&month).to_owned();
    if at.year() == OffsetDateTime::now_utc().year() {
        format!(
            "{:>2} {month} {:02}:{:02}",
            at.day(),
            at.hour(),
            at.minute()
        )
    } else {
        format!("{:>2} {month} {}", at.day(), at.year())
    }
}

/// Keeps the end of a path: `…/deploy/releases`.
fn tail(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max || max < 2 {
        return text.to_owned();
    }
    let kept: String = text.chars().skip(count - (max - 1)).collect();
    format!("…{kept}")
}

fn render_queue(view: &FilesView, s: Styles, tick: u64, frame: &mut Frame<'_>, area: Rect) {
    let running = view
        .transfers
        .iter()
        .filter(|t| matches!(t.status, TransferStatus::Running | TransferStatus::Queued))
        .count();
    let title = if running > 0 {
        format!(" Transfers · {running} running ")
    } else {
        " Transfers ".to_owned()
    };
    let width = usize::from(area.width.saturating_sub(4));
    let skip = view.transfers.len().saturating_sub(QUEUE_ROWS);
    let lines: Vec<Line<'_>> = view
        .transfers
        .iter()
        .skip(skip)
        .map(|t| transfer_line(t, s, tick, width))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.border())
                .title(Line::from(Span::styled(title, s.secondary())))
                .padding(Padding::horizontal(1))
                .style(s.surface()),
        ),
        area,
    );
}

fn transfer_line(t: &TransferRow, s: Styles, tick: u64, width: usize) -> Line<'static> {
    let status = s.theme.status;
    let arrow = match t.direction {
        Direction::Download => "↓",
        Direction::Upload => "↑",
    };
    // Right part first: it decides how much room the name gets.
    let mut right: Vec<Span<'static>> = Vec::new();
    match &t.status {
        TransferStatus::Queued => {
            right.push(Span::styled(super::spinner(tick), s.accent()));
            right.push(Span::styled(" starting", s.muted()));
        }
        TransferStatus::Running => {
            let fraction = t.total.filter(|n| *n > 0).map(|total| {
                #[allow(clippy::cast_precision_loss)] // display only
                let f = t.done as f64 / total as f64;
                f.clamp(0.0, 1.0)
            });
            if let Some(f) = fraction {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=BAR
                let filled = (f * f64::from(BAR)).round() as usize;
                right.push(Span::styled("━".repeat(filled), s.accent()));
                right.push(Span::styled(
                    "─".repeat(usize::from(BAR).saturating_sub(filled)),
                    s.border(),
                ));
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=100
                let pct = (f * 100.0).floor() as u8;
                right.push(Span::styled(format!(" {pct:>3}%"), s.secondary()));
            } else {
                right.push(Span::styled(super::spinner(tick), s.accent()));
                right.push(Span::styled(
                    format!(" {}", human_size(t.done)),
                    s.secondary(),
                ));
            }
            let rate = t.rate();
            if rate > 0.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // display only
                let per_sec = rate as u64;
                right.push(Span::styled(
                    format!("  {}/s", human_size(per_sec)),
                    s.muted(),
                ));
                if let Some(total) = t.total {
                    #[allow(clippy::cast_precision_loss)] // display only
                    let left = total.saturating_sub(t.done) as f64 / rate;
                    right.push(Span::styled(format!("  {}", eta(left)), s.muted()));
                }
            }
        }
        TransferStatus::Done => {
            right.push(Span::styled("✓ ", s.status(status.connected)));
            right.push(Span::styled(
                human_size(t.total.unwrap_or(t.done)),
                s.muted(),
            ));
        }
        TransferStatus::Skipped => right.push(Span::styled("skipped", s.muted())),
        TransferStatus::Cancelled => right.push(Span::styled("cancelled", s.muted())),
        TransferStatus::Failed(message) => {
            right.push(Span::styled("✕ ", s.status(status.error)));
            right.push(Span::styled(
                fmt::truncate(message, 36),
                s.status(status.error),
            ));
        }
    }
    let right_w: usize = right.iter().map(Span::width).sum();
    let name_w = width.saturating_sub(right_w + 4).max(4);
    let name = fmt::truncate(&display_safe(&t.name, 256), name_w);
    let mut spans = vec![
        Span::styled(format!("{arrow} "), s.accent()),
        Span::styled(format!("{name:<name_w$}  "), s.secondary()),
    ];
    spans.extend(right);
    Line::from(spans)
}

/// `42 s`, `3 min`, `1 h 20`.
fn eta(seconds: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // display only
    let secs = seconds.max(0.0).round() as u64;
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h {:02}", secs / 3600, (secs % 3600) / 60),
    }
}

/// The current question, else the last error, else nothing.
fn render_footer(view: &FilesView, s: Styles, frame: &mut Frame<'_>, area: Rect) {
    let width = usize::from(area.width);
    let key = |k: &str| Span::styled(format!(" {k} "), s.key_hint());
    let primary = |k: &str| Span::styled(format!(" {k} "), s.key_primary());
    let label = |t: &str| Span::styled(format!(" {t}  "), s.muted());
    let line = match &view.prompt {
        Some(FilesPrompt::NewFolder { name, .. }) => Line::from(vec![
            Span::styled(" New folder  ", s.secondary()),
            Span::styled(name.clone(), s.title()),
            Span::styled("▏  ", s.accent()),
            primary("↵"),
            label("create"),
            key("Esc"),
            label("cancel"),
        ]),
        Some(FilesPrompt::Rename { name, .. }) => Line::from(vec![
            Span::styled(" Rename to  ", s.secondary()),
            Span::styled(display_safe(name, 128).into_owned(), s.title()),
            Span::styled("▏  ", s.accent()),
            primary("↵"),
            label("rename"),
            key("Esc"),
            label("cancel"),
        ]),
        Some(FilesPrompt::Delete { name, .. }) => Line::from(vec![
            Span::styled(" Delete ", s.status(s.theme.status.warning)),
            Span::styled(
                fmt::truncate(&display_safe(name, 128), width.saturating_sub(40)),
                s.title(),
            ),
            Span::styled("?  ", s.status(s.theme.status.warning)),
            primary("y"),
            label("delete"),
            key("n"),
            label("keep"),
        ]),
        Some(FilesPrompt::Conflict { name, .. }) => Line::from(vec![
            Span::styled(
                fmt::truncate(&display_safe(name, 128), width.saturating_sub(58)),
                s.title(),
            ),
            Span::styled(" exists  ", s.status(s.theme.status.warning)),
            primary("k"),
            label("keep both"),
            key("o"),
            label("overwrite"),
            key("s"),
            label("skip"),
            key("Esc"),
        ]),
        None => match &view.notice {
            Some(err) => {
                let mut spans = vec![
                    Span::styled(" ✕ ", s.status(s.theme.status.error)),
                    Span::styled(err.message.clone(), s.status(s.theme.status.error)),
                ];
                if let Some(hint) = &err.hint {
                    spans.push(Span::styled(format!("  {hint}"), s.secondary()));
                }
                Line::from(spans)
            }
            None => Line::from(Span::styled(
                format!(
                    " {}{}",
                    if view.hidden {
                        "hidden files shown · "
                    } else {
                        ""
                    },
                    "c copies the selection to the other pane"
                ),
                s.muted(),
            )),
        },
    };
    frame.render_widget(Paragraph::new(line).style(s.base()), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eta_and_tail() {
        assert_eq!(eta(4.4), "4 s");
        assert_eq!(eta(125.0), "2 min");
        assert_eq!(eta(4_980.0), "1 h 23");
        assert_eq!(tail("/srv/app/releases", 9), "…releases");
        assert_eq!(tail("/srv", 9), "/srv");
    }
}
