//! The "Recordings" panel and the player (phase 6, session recording).
//!
//! Panel: one row per file, newest first: a mark (`●` in `status-error`
//! while the file is being written, `▲` in `status-warning` for a file
//! that is not a valid recording), the alias, start time, duration and
//! size (tabular). The selected file's name is shown under the list.
//!
//! Player: a title row (the header title, untrusted, always followed by
//! the file name), the recorded grid, then a progress row (time / total,
//! a gauge in the accent, the speed). Only theme tokens (`Styles`).

use portico_core::recording::RecordingEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph};
use time::OffsetDateTime;

use crate::app::App;
use crate::keys::Load;
use crate::player::{Player, PlayerState, clock, speed_label};
use crate::recordings::RecordingsPanel;
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Columns of a row: mark + bar, start, duration, size.
const MARK_WIDTH: usize = 3;
const START_WIDTH: usize = 14;
const DURATION_WIDTH: usize = 9;
const SIZE_WIDTH: usize = 10;

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let tick = app.tick;
    let dialog_open = !app.dialogs.is_empty();
    // Files being written right now (their names, when the event said).
    let active: Vec<String> = app.recording.values().flatten().cloned().collect();
    let Some(panel) = &mut app.recordings_panel else {
        return;
    };
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(100);
    let height = screen.height.saturating_sub(2).min(30);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let count = panel
        .entries
        .ready()
        .map(|e| format!("· {} ", e.len()))
        .unwrap_or_default();
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Recordings ", s.title()),
            Span::styled(count, s.muted()),
        ]))
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    if panel.confirm.is_none() && !dialog_open {
        block =
            block.title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned());
    }
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = usize::from(inner.width);

    let bottom = footer(panel, s, w, dialog_open);
    let bottom_height = u16::try_from(bottom.len()).unwrap_or(4);
    let [list_area, foot] =
        Layout::vertical([Constraint::Min(2), Constraint::Length(bottom_height)]).areas(inner);
    frame.render_widget(Paragraph::new(bottom), foot);

    if let Some(state) = list_state(panel, s, tick, w) {
        frame.render_widget(state, list_area);
        return;
    }
    let selected = panel.list.selected();
    let items: Vec<ListItem<'static>> = panel
        .entries
        .ready()
        .map(|entries| {
            entries
                .iter()
                .enumerate()
                .map(|(i, e)| row(e, active.contains(&e.name), selected == Some(i), s, w))
                .collect()
        })
        .unwrap_or_default();
    frame.render_stateful_widget(List::new(items), list_area, &mut panel.list);
}

/// Under the list: the selected file's name, the delete question, the
/// last result, then the keys (none behind a dialog).
fn footer(panel: &RecordingsPanel, s: Styles, w: usize, dialog_open: bool) -> Vec<Line<'static>> {
    let mut bottom: Vec<Line<'static>> = Vec::new();
    if let Some(entry) = panel.selected() {
        bottom.push(Line::from(""));
        bottom.push(Line::from(vec![
            Span::styled("file  ", s.muted()),
            Span::styled(
                fmt::truncate(&entry.name, w.saturating_sub(6)),
                s.secondary(),
            ),
        ]));
        if !entry.valid {
            for l in fmt::wrap(
                "Not a valid asciicast v2 recording: it cannot be played, it can be deleted.",
                w,
            ) {
                bottom.push(Line::from(Span::styled(
                    l,
                    s.status(s.theme.status.warning),
                )));
            }
        }
    }
    if let Some(confirm) = &panel.confirm {
        bottom.push(Line::from(""));
        bottom.push(Line::from(vec![
            Span::styled("▲ ", s.status(s.theme.status.warning)),
            Span::styled(
                format!(
                    "Delete {}?",
                    fmt::truncate(&confirm.name, w.saturating_sub(12))
                ),
                s.title(),
            ),
        ]));
        bottom.push(Line::from(Span::styled(
            "The file is removed from the recordings folder.",
            s.secondary(),
        )));
    }
    if let Some(n) = &panel.notice {
        bottom.push(Line::from(""));
        bottom.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
    }
    if !dialog_open {
        bottom.push(Line::from(""));
        let hints = panel.hints();
        let primary = hints.first().is_some_and(|(k, _)| *k == "↵");
        bottom.extend(form::hints(s, &hints, primary, w));
    }
    bottom
}

/// Loading, error and empty states in place of the list.
fn list_state(
    panel: &RecordingsPanel,
    s: Styles,
    tick: u64,
    w: usize,
) -> Option<Paragraph<'static>> {
    match &panel.entries {
        Load::Loading => Some(Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Loading recordings…", s.muted()),
        ]))),
        Load::Failed(e) => Some(Paragraph::new(form::notice(
            s,
            false,
            &e.message,
            e.hint.as_deref(),
            w,
        ))),
        Load::Ready(all) if all.is_empty() => {
            let mut lines = vec![Line::from(Span::styled(
                "No recordings yet.",
                s.secondary(),
            ))];
            let text = format!(
                "In a terminal, Ctrl-Space R records what the session shows. Files are kept in {}.",
                panel.folder
            );
            lines.extend(
                fmt::wrap(&text, w)
                    .into_iter()
                    .map(|l| Line::from(Span::styled(l, s.muted()))),
            );
            Some(Paragraph::new(lines))
        }
        Load::Ready(_) => None,
    }
}

/// `22 Oct 14:03` (UTC, display only).
fn started_label(at: Option<OffsetDateTime>) -> String {
    let Some(at) = at else {
        return "—".to_owned();
    };
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

/// `▍● web-01      22 Oct 14:03     1:23    1.4 MiB`.
fn row(
    entry: &RecordingEntry,
    writing: bool,
    selected: bool,
    s: Styles,
    w: usize,
) -> ListItem<'static> {
    let (mark, mark_style) = if writing {
        ("●", s.status(s.theme.status.error))
    } else if !entry.valid {
        ("▲", s.status(s.theme.status.warning))
    } else {
        (" ", s.muted())
    };
    let alias_width = w
        .saturating_sub(MARK_WIDTH + START_WIDTH + DURATION_WIDTH + SIZE_WIDTH)
        .max(6);
    let alias = fmt::truncate(&entry.alias, alias_width);
    let duration = match (entry.valid, entry.duration) {
        (true, Some(d)) => clock(d),
        _ => "—".to_owned(),
    };
    let alias_style = if entry.valid { s.title() } else { s.muted() };
    let lead = if selected {
        Span::styled("▍", s.accent())
    } else {
        Span::raw(" ")
    };
    let line = Line::from(vec![
        lead,
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(format!("{alias:<alias_width$}"), alias_style),
        Span::styled(
            format!("{:>START_WIDTH$}", started_label(entry.started)),
            s.secondary(),
        ),
        Span::styled(format!("{duration:>DURATION_WIDTH$}"), s.secondary()),
        Span::styled(
            format!("{:>SIZE_WIDTH$}", crate::files::human_size(entry.size)),
            s.muted(),
        ),
    ]);
    let item = ListItem::new(line);
    if selected {
        item.style(s.selected())
    } else {
        item
    }
}

// ── player ─────────────────────────────────────────────────────────────────

/// Draws the playback over the body.
pub(crate) fn render_player(
    player: &Player,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    frame.render_widget(Block::new().style(s.base()), area);
    let [title, grid_area, progress] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    render_title(player, s, tick, frame, title);

    let grid = player.size;
    let shown = Rect {
        width: grid.cols.min(grid_area.width),
        height: grid.rows.min(grid_area.height),
        ..grid_area
    };
    // A grid taller than the view keeps its cursor line in view.
    let cursor_line = u16::try_from(player.term().grid().cursor.point.line.0).unwrap_or(0);
    let skip = cursor_line
        .saturating_add(1)
        .saturating_sub(shown.height)
        .min(grid.rows.saturating_sub(shown.height));
    super::terminal::render_grid(
        player.term(),
        player.state() != PlayerState::Ended,
        skip,
        s,
        frame,
        shown,
    );
    let cropped = grid.cols > grid_area.width || grid.rows > grid_area.height;
    render_progress(player, cropped, s, frame, progress);
}

fn render_title(player: &Player, s: Styles, tick: u64, frame: &mut Frame<'_>, area: Rect) {
    let width = usize::from(area.width);
    let (state, state_style) = match player.state() {
        PlayerState::Loading => (format!("{} Loading", spinner(tick)), s.muted()),
        PlayerState::Playing => ("▶ Playing".to_owned(), s.accent()),
        PlayerState::Paused => ("❚❚ Paused".to_owned(), s.secondary()),
        PlayerState::Ended => ("■ Ended".to_owned(), s.muted()),
    };
    let right = format!(" {state}  {} ", speed_label(player.speed()));
    let right_width = right.chars().count();
    // The file name always shows: a third-party header title says anything.
    let name_room = width.saturating_sub(right_width + 13).max(8);
    let name = fmt::truncate(&player.name, name_room.min(48));
    let mut spans = vec![
        Span::styled("▍", s.accent()),
        Span::styled("Playback  ", s.title()),
    ];
    let used = 11 + name.chars().count() + right_width;
    if let Some(title) = &player.title {
        let room = width.saturating_sub(used + 5);
        if room >= 4 {
            spans.push(Span::styled(
                format!("“{}”  ", fmt::truncate(title, room)),
                s.secondary(),
            ));
        }
    }
    spans.push(Span::styled(name, s.muted()));
    let used: usize = spans.iter().map(Span::width).sum();
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + right_width)),
    ));
    spans.push(Span::styled(right, state_style));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.chrome()), area);
}

fn render_progress(player: &Player, cropped: bool, s: Styles, frame: &mut Frame<'_>, area: Rect) {
    let width = usize::from(area.width);
    let total = player.total.unwrap_or(player.position).max(player.position);
    let time = format!(" {} / {} ", clock(player.position), clock(total));
    let mut spans = vec![Span::styled(time.clone(), s.secondary())];
    let tail = if cropped {
        format!(" {}×{} · cropped ", player.size.cols, player.size.rows)
    } else {
        format!(" {}×{} ", player.size.cols, player.size.rows)
    };
    if let Some(error) = &player.error {
        // The reading stopped early: what came before was played.
        let room = width.saturating_sub(time.chars().count() + 3);
        spans.push(Span::styled(" ▲ ", s.status(s.theme.status.warning)));
        spans.push(Span::styled(
            fmt::truncate(&fmt::sentence(&error.message), room),
            s.status(s.theme.status.warning),
        ));
    } else {
        let gauge = width.saturating_sub(time.chars().count() + tail.chars().count() + 2);
        let ratio = if total > 0.0 {
            (player.position / total).clamp(0.0, 1.0)
        } else if player.state() == PlayerState::Ended {
            1.0
        } else {
            0.0
        };
        // Display only: a cell count within `gauge`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let done = ((gauge as f64) * ratio).round() as usize;
        let done = done.min(gauge);
        spans.push(Span::raw(" "));
        spans.push(Span::styled("━".repeat(done), s.accent()));
        spans.push(Span::styled("─".repeat(gauge - done), s.border()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(tail, s.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.chrome()), area);
}
