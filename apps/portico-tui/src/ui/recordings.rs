//! The "Recordings" panel and the player (phase 6, session recording).
//!
//! Panel: one row per file, newest first: a mark (`●` in `status-error`
//! while the file is being written, `▲` in `status-warning` for a file
//! that is not a valid recording), the alias, start (local time, 24 h),
//! duration (`m:ss` / `h:mm:ss`) and size, right-aligned. The selected
//! file's name is shown under the list; the keys are in the status bar
//! only. At 82 columns or less the panel takes the whole width (no strip
//! of the terminal behind shows through its margins).
//!
//! Player: a title row (the header title, untrusted, always followed by
//! the file name, then the state and speed), the recorded grid, then a
//! progress row (time / total, a gauge in the accent, what does not fit).
//! On a short screen (24 rows) the title row goes: the file name, state
//! and speed move into the progress row, and the grid gets the row back.
//! The accent is kept for the gauge; the state reads in secondary text.
//! Only theme tokens (`Styles`).

use portico_core::recording::RecordingEntry;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph};

use crate::app::App;
use crate::keys::Load;
use crate::player::{Player, PlayerState, clock, speed_label};
use crate::recordings::RecordingsPanel;
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Columns of a row: bar + mark, then the gaps before duration and size.
const MARK_WIDTH: usize = 3;
const DURATION_WIDTH: usize = 9;
const SIZE_WIDTH: usize = 10;
/// Widest panel.
const PANEL_MAX_WIDTH: u16 = 100;
/// At this screen width or less, the panel takes the full width.
const FULL_WIDTH_AT: u16 = 82;
/// A body this short (a 24-row screen) gets the compact player.
const COMPACT_PLAYER_HEIGHT: u16 = 22;
/// Smallest progress gauge worth drawing.
const MIN_GAUGE: usize = 12;
/// The file name in the compact player's row: between these (it starts
/// with the alias and the date, what tells files apart).
const NAME_MIN: usize = 12;
const NAME_MAX: usize = 28;

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let tick = app.tick;
    let Some(panel) = &mut app.recordings_panel else {
        return;
    };
    let screen = frame.area();
    let width = if screen.width <= FULL_WIDTH_AT {
        screen.width
    } else {
        screen.width.saturating_sub(2).min(PANEL_MAX_WIDTH)
    };
    let height = screen.height.saturating_sub(2).min(30);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let count = panel
        .entries
        .ready()
        .filter(|e| !e.is_empty())
        .map(|e| format!("· {} ", e.len()))
        .unwrap_or_default();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Recordings ", s.title()),
            Span::styled(count, s.muted()),
        ]))
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = usize::from(inner.width);

    let bottom = footer(panel, s, w);
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
            let labels: Vec<String> = entries.iter().map(|e| started_label(e.started)).collect();
            let start_width = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0) + 2;
            entries
                .iter()
                .zip(labels)
                .enumerate()
                .map(|(i, (e, start))| {
                    let cols = Columns {
                        start,
                        start_width,
                        width: w,
                    };
                    row(
                        e,
                        panel.active.contains(&e.name),
                        selected == Some(i),
                        &cols,
                        s,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    frame.render_stateful_widget(List::new(items), list_area, &mut panel.list);
}

/// Under the list: the selected file's name, why it cannot be played or
/// deleted, the delete question, the last result. The keys are in the
/// status bar.
fn footer(panel: &RecordingsPanel, s: Styles, w: usize) -> Vec<Line<'static>> {
    let warning = s.status(s.theme.status.warning);
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
                bottom.push(Line::from(Span::styled(l, warning)));
            }
        }
    }
    if let Some((message, hint)) = panel.refused {
        bottom.push(Line::from(""));
        bottom.push(Line::from(vec![
            Span::styled("▲ ", warning),
            Span::styled(message, warning),
        ]));
        bottom.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(hint, s.secondary()),
        ]));
    }
    if let Some(confirm) = &panel.confirm {
        bottom.push(Line::from(""));
        bottom.push(Line::from(vec![
            Span::styled("▲ ", warning),
            Span::styled(
                format!(
                    "Delete {}?",
                    fmt::truncate(&confirm.name, w.saturating_sub(12))
                ),
                s.title(),
            ),
        ]));
        for l in fmt::wrap(
            "The file is removed from the recordings folder. This cannot be undone.",
            w,
        ) {
            bottom.push(Line::from(Span::styled(l, s.secondary())));
        }
    }
    if let Some(n) = &panel.notice {
        bottom.push(Line::from(""));
        bottom.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
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
            lines.extend(
                fmt::wrap(
                    "In a terminal, Ctrl-Space R records what the session shows. Files are kept in",
                    w,
                )
                .into_iter()
                .map(|l| Line::from(Span::styled(l, s.muted()))),
            );
            // The folder on its own line, its middle cut when long.
            lines.push(Line::from(Span::styled(
                fmt::truncate_middle(&panel.folder, w),
                s.secondary(),
            )));
            Some(Paragraph::new(lines))
        }
        Load::Ready(_) => None,
    }
}

/// `22 Oct 14:03` (local time, see [`fmt::date_label`]).
fn started_label(at: Option<time::OffsetDateTime>) -> String {
    at.map_or_else(|| "—".to_owned(), fmt::date_label)
}

/// The text of a row's start column, and the column widths.
struct Columns {
    start: String,
    start_width: usize,
    width: usize,
}

/// `▍● web-01      22 Oct 14:03     1:23    1.4 MiB`.
fn row(
    entry: &RecordingEntry,
    writing: bool,
    selected: bool,
    cols: &Columns,
    s: Styles,
) -> ListItem<'static> {
    let warning = s.status(s.theme.status.warning);
    let (mark, mark_style) = if writing {
        ("●", s.status(s.theme.status.error))
    } else if !entry.valid {
        ("▲", warning)
    } else {
        (" ", s.muted())
    };
    let alias_width = cols
        .width
        .saturating_sub(MARK_WIDTH + cols.start_width + DURATION_WIDTH + SIZE_WIDTH)
        .max(6);
    let alias = fmt::truncate(&entry.alias, alias_width);
    let duration = match (entry.valid, entry.duration) {
        (true, Some(d)) => clock(d),
        _ => "—".to_owned(),
    };
    // An invalid file reads as a warning (it is not an error: it can be
    // deleted, and nothing failed).
    let alias_style = if entry.valid { s.title() } else { warning };
    let lead = if selected {
        Span::styled("▍", s.accent())
    } else {
        Span::raw(" ")
    };
    let start_width = cols.start_width;
    let line = Line::from(vec![
        lead,
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(format!("{alias:<alias_width$}"), alias_style),
        Span::styled(format!("{:>start_width$}", cols.start), s.secondary()),
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
    let compact = area.height <= COMPACT_PLAYER_HEIGHT;
    let (grid_area, progress) = if compact {
        let [grid_area, progress] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        (grid_area, progress)
    } else {
        let [title, grid_area, progress] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        render_title(player, s, tick, frame, title);
        (grid_area, progress)
    };

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
    let hidden = Hidden {
        rows: grid.rows.saturating_sub(grid_area.height),
        cols: grid.cols.saturating_sub(grid_area.width),
    };
    render_progress(player, compact, hidden, s, tick, frame, progress);
}

/// `▶ Playing` and its style: secondary text (the accent is the gauge's).
fn state_label(player: &Player, s: Styles, tick: u64) -> (String, Style) {
    match player.state() {
        PlayerState::Loading => (format!("{} Loading", spinner(tick)), s.muted()),
        PlayerState::Playing => ("▶ Playing".to_owned(), s.secondary()),
        PlayerState::Paused => ("❚❚ Paused".to_owned(), s.secondary()),
        PlayerState::Ended => ("■ Ended".to_owned(), s.muted()),
    }
}

fn render_title(player: &Player, s: Styles, tick: u64, frame: &mut Frame<'_>, area: Rect) {
    let width = usize::from(area.width);
    let (state, state_style) = state_label(player, s, tick);
    let speed = format!("  {} ", speed_label(player.speed()));
    let right_width = state.chars().count() + speed.chars().count() + 1;
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
    spans.push(Span::raw(" "));
    spans.push(Span::styled(state, state_style));
    spans.push(Span::styled(speed, s.muted()));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.chrome()), area);
}

/// What of the recorded grid does not fit the view.
#[derive(Debug, Clone, Copy)]
struct Hidden {
    rows: u16,
    cols: u16,
}

impl Hidden {
    /// `2 rows hidden`, `1 row · 3 cols hidden`; `None` when all shows.
    fn label(self) -> Option<String> {
        let plural =
            |n: u16, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        match (self.rows, self.cols) {
            (0, 0) => None,
            (rows, 0) => Some(format!("{} hidden", plural(rows, "row", "rows"))),
            (0, cols) => Some(format!("{} hidden", plural(cols, "col", "cols"))),
            (rows, cols) => Some(format!(
                "{} · {} hidden",
                plural(rows, "row", "rows"),
                plural(cols, "col", "cols")
            )),
        }
    }
}

/// ` 0:05 / 0:11  ━━━━────  80×22 ` (compact: the file name first, then
/// the state and speed after the gauge). A reading error takes the
/// gauge's place: `▲ message · hint`, the hint when there is room.
fn render_progress(
    player: &Player,
    compact: bool,
    hidden: Hidden,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let width = usize::from(area.width);
    let total = player.total.unwrap_or(player.position).max(player.position);
    let time = format!(" {} / {} ", clock(player.position), clock(total));

    // Right side: state and speed (compact), then the size or what is
    // hidden (dropped first when short).
    let mut right: Vec<Span<'static>> = Vec::new();
    if compact {
        let (state, style) = state_label(player, s, tick);
        right.push(Span::raw(" "));
        right.push(Span::styled(state, style));
        right.push(Span::styled(
            format!("  {} ", speed_label(player.speed())),
            s.muted(),
        ));
    }
    let tail = match hidden.label() {
        Some(label) if compact => format!("· {label} "),
        Some(label) => format!(" {label} "),
        None if compact => String::new(),
        None => format!(" {}×{} ", player.size.cols, player.size.rows),
    };

    let name_width = |room: usize| {
        if compact {
            // `▍` + name + two spaces.
            player.name.chars().count().min(room) + 3
        } else {
            0
        }
    };
    let right_width: usize = right.iter().map(Span::width).sum();
    let fixed = time.chars().count() + right_width + 2;
    let tail = if fixed + name_width(NAME_MIN) + tail.chars().count() + MIN_GAUGE <= width {
        tail
    } else {
        String::new()
    };
    let fixed = fixed + tail.chars().count();
    // The name is kept short enough for a gauge that reads as one.
    let name_room = width
        .saturating_sub(fixed + MIN_GAUGE + 3)
        .clamp(NAME_MIN, NAME_MAX);

    let mut spans: Vec<Span<'static>> = Vec::new();
    if compact {
        spans.push(Span::styled("▍", s.accent()));
        spans.push(Span::styled(
            fmt::truncate(&player.name, name_room),
            s.secondary(),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(time, s.secondary()));
    let used: usize = spans.iter().map(Span::width).sum();
    let middle = width.saturating_sub(used + right_width + tail.chars().count());
    if let Some(error) = &player.error {
        // The reading stopped early: what came before was played.
        let warning = s.status(s.theme.status.warning);
        let message = fmt::sentence(&error.message);
        let room = middle.saturating_sub(3);
        let message = fmt::truncate(&message, room);
        let mut taken = 3 + message.chars().count();
        spans.push(Span::styled(" ▲ ", warning));
        spans.push(Span::styled(message, warning));
        if let Some(hint) = error.hint.as_deref() {
            let room = middle.saturating_sub(taken + 3);
            if room >= 12 {
                let hint = fmt::truncate(hint, room);
                taken += 3 + hint.chars().count();
                spans.push(Span::styled(" · ", s.muted()));
                spans.push(Span::styled(hint, s.secondary()));
            }
        }
        spans.push(Span::raw(" ".repeat(middle.saturating_sub(taken))));
    } else {
        let gauge = middle.saturating_sub(2);
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
    }
    spans.extend(right);
    spans.push(Span::styled(tail, s.muted()));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.chrome()), area);
}
