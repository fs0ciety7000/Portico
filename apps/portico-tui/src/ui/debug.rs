//! Session logs overlay: sessions (left column, or a one-line selector
//! below [`NARROW`] columns), then the Logs / Effective config tabs.
//! Keys are in the status bar and in `?`.
//!
//! Colour carries meaning only: WARN and ERROR levels take their status
//! colour, INFO is secondary and DEBUG / TRACE muted; messages are body
//! text, only an ERROR's message is emphasised.

use portico_core::effective::{EffectiveConfig, OptionOrigin};
use portico_core::logs::{LogLevel, LogRecord, SessionLogInfo, SessionOutcome};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use time::OffsetDateTime;

use crate::app::App;
use crate::debug::{DebugPanel, DebugTab, NARROW, is_sensitive, origin_label, window};
use crate::form::display_safe;
use crate::keys::Load;
use crate::theme::Styles;
use crate::ui::fmt::{self, display_path};
use crate::ui::{form, spinner, status};

/// Width of the session column.
const LEFT: u16 = 26;
/// Indent of wrapped log rows (`HH:MM:SS LEVEL `).
const LOG_INDENT: usize = 15;
/// Key column of the config.
const KEY_COL: usize = 22;
/// From this width, an option and its origin share one row.
const WIDE: usize = 72;
/// What a masked value shows (same words as the GUI).
const HIDDEN_NOTE: &str = "  hidden · s reveals · never copied";
/// After a revealed value: it still never leaves the screen.
const REVEALED_NOTE: &str = "  · never copied";

type Live = [(portico_core::ids::SessionId, portico_theme::Rgb)];

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let tick = app.tick;
    // Colour of live sessions comes from the app's view of them.
    let live: Vec<(portico_core::ids::SessionId, portico_theme::Rgb)> = app
        .sessions
        .values()
        .map(|v| (v.id, status::status(Some(v), s).color))
        .collect();
    let Some(panel) = &mut app.debug_panel else {
        return;
    };
    let screen = frame.area();
    // A full view: nothing of the terminal behind shows around it.
    let body = Rect {
        y: screen.y.saturating_add(1),
        height: screen.height.saturating_sub(2),
        ..screen
    };
    frame.render_widget(Clear, body);
    frame.render_widget(Block::new().style(s.base()), body);
    let width = screen.width.saturating_sub(2).min(140);
    let height = screen.height.saturating_sub(2);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(Span::styled(" Session logs ", s.title())))
        .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
        .padding(Padding::new(1, 1, 0, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if screen.width < NARROW {
        // One column: the session is a selector line above the tabs.
        let picker = selector(panel, &live, s, usize::from(inner.width));
        detail(panel, Some(picker), s, tick, frame, inner);
        return;
    }
    let left_width = LEFT.min(inner.width / 3);
    let [left, sep, right] = Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Length(1),
        Constraint::Min(10),
    ])
    .areas(inner);
    sessions(panel, &live, s, frame, left);
    let rule: Vec<Line<'_>> = (0..sep.height)
        .map(|_| Line::from(Span::styled("│", s.border())))
        .collect();
    frame.render_widget(Paragraph::new(rule), sep);
    let right = Rect {
        x: right.x.saturating_add(1),
        width: right.width.saturating_sub(1),
        ..right
    };
    detail(panel, None, s, tick, frame, right);
}

fn hms(at: OffsetDateTime) -> String {
    format!("{:02}:{:02}:{:02}", at.hour(), at.minute(), at.second())
}

fn outcome_color(info: &SessionLogInfo, live: &Live, s: Styles) -> portico_theme::Rgb {
    let st = s.theme.status;
    match &info.outcome {
        None => live
            .iter()
            .find(|(id, _)| *id == info.id)
            .map_or(st.connected, |(_, c)| *c),
        Some(SessionOutcome::Failed { error }) if error.code != "host_key_rejected" => st.error,
        Some(_) => st.idle,
    }
}

/// `◂ ● lab · 22:23 (1/3) ▸`: the selected session on one line; the
/// arrows are lit when there is a session that way (`[` `]`, `←` `→`).
fn selector(panel: &DebugPanel, live: &Live, s: Styles, w: usize) -> Line<'static> {
    let Some(info) = panel.session() else {
        return Line::from(Span::styled(
            fmt::truncate(
                "No session yet. Connect to a host: its records show here.",
                w,
            ),
            s.muted(),
        ));
    };
    let n = panel.sessions.len();
    let arrow = |on: bool| if on { s.accent() } else { s.border() };
    let time = format!("{:02}:{:02}", info.started.hour(), info.started.minute());
    let count = format!(" ({}/{n})", panel.selected + 1);
    let tag = if info.probe { " · probe" } else { "" };
    let alias_room = w.saturating_sub(4 + 2 + 3 + time.len() + tag.len() + count.len() + 2);
    Line::from(vec![
        Span::styled("◂ ", arrow(panel.selected > 0)),
        Span::styled("● ", s.status(outcome_color(info, live, s))),
        Span::styled(
            fmt::truncate(&display_safe(&info.alias), alias_room.max(4)),
            s.title(),
        ),
        Span::styled(format!(" · {time}"), s.secondary()),
        Span::styled(tag, s.muted()),
        Span::styled(count, s.muted()),
        Span::styled(" ▸", arrow(panel.selected + 1 < n)),
    ])
}

fn sessions(panel: &DebugPanel, live: &Live, s: Styles, frame: &mut Frame<'_>, area: Rect) {
    let w = usize::from(area.width);
    let mut lines = vec![
        Line::from(Span::styled("Sessions", s.secondary())),
        Line::from(""),
    ];
    if panel.sessions.is_empty() {
        for l in fmt::wrap(
            "No session yet. Connect to a host: its records show here.",
            w,
        ) {
            lines.push(Line::from(Span::styled(l, s.muted())));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return;
    }
    let room = usize::from(area.height).saturating_sub(2);
    // Keep the selection in view.
    let first = panel.selected.saturating_sub(room.saturating_sub(1));
    for (i, info) in panel.sessions.iter().enumerate().skip(first).take(room) {
        let selected = i == panel.selected;
        let time = hms(info.started);
        let tag = if info.probe { " probe" } else { "" };
        let alias_room = w.saturating_sub(time.len() + tag.len() + 5);
        let alias = fmt::truncate(&display_safe(&info.alias), alias_room);
        let pad = w.saturating_sub(alias.chars().count() + tag.len() + time.len() + 4);
        let bg = if selected { s.selected() } else { Style::new() };
        let mark = if selected {
            Span::styled("▍", s.accent().patch(bg))
        } else {
            Span::raw(" ")
        };
        lines.push(Line::from(vec![
            mark,
            Span::styled("● ", s.status(outcome_color(info, live, s)).patch(bg)),
            Span::styled(
                alias,
                if selected { s.title() } else { s.secondary() }.patch(bg),
            ),
            Span::styled(tag, s.muted().patch(bg)),
            Span::styled(" ".repeat(pad), bg),
            Span::styled(time, s.muted().patch(bg)),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// `Logs │ Effective config`, the level or alias on the right; the second
/// tab is just `Config` when the full name does not fit.
fn tabs_line(panel: &DebugPanel, s: Styles, w: usize) -> Line<'static> {
    let tab = |name: &str, on: bool| {
        if on {
            Span::styled(format!(" {name} "), s.title().patch(s.selected()))
        } else {
            Span::styled(format!(" {name} "), s.muted())
        }
    };
    let right = match panel.tab {
        DebugTab::Logs => match panel.min_level {
            LogLevel::Trace | LogLevel::Debug => "all levels".to_owned(),
            other => format!("≥ {}", other.as_str().to_lowercase()),
        },
        DebugTab::Config => panel
            .alias()
            .map(|a| fmt::truncate(&display_safe(&a), 32))
            .unwrap_or_default(),
    };
    let long = " Logs │ Effective config ".chars().count() + right.chars().count() + 1;
    let config = if long <= w {
        "Effective config"
    } else {
        "Config"
    };
    let mut spans = vec![
        tab("Logs", panel.tab == DebugTab::Logs),
        Span::styled("│", s.border()),
        tab(config, panel.tab == DebugTab::Config),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    let pad = w.saturating_sub(used + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, s.muted()));
    Line::from(spans)
}

fn detail(
    panel: &mut DebugPanel,
    selector: Option<Line<'static>>,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let w = usize::from(area.width);
    let mut head = Vec::new();
    if let Some(line) = selector {
        head.push(line);
        head.push(Line::from(""));
    }
    head.push(tabs_line(panel, s, w));
    if panel.tab == DebugTab::Logs && (panel.filtering || !panel.filter.is_empty()) {
        head.push(form::input(
            s,
            "Filter",
            8,
            &panel.filter,
            panel.filtering,
            w.saturating_sub(8).min(40),
        ));
    }
    head.push(Line::from(""));
    let foot: Vec<Line<'static>> = panel
        .notice
        .as_ref()
        .map(|n| {
            let mut l = vec![Line::from("")];
            l.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
            l
        })
        .unwrap_or_default();
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(u16::try_from(head.len()).unwrap_or(3)),
        Constraint::Min(1),
        Constraint::Length(u16::try_from(foot.len()).unwrap_or(0)),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(head), top);
    frame.render_widget(Paragraph::new(foot), bottom);
    let rows = usize::from(body.height);
    let lines = match panel.tab {
        DebugTab::Logs => logs_view(panel, s, w, rows),
        DebugTab::Config => {
            let all = config_lines(panel, s, tick, w);
            let max = all.len().saturating_sub(rows);
            panel.clamp_scroll(usize::MAX, max);
            all.into_iter()
                .skip(panel.config_scroll)
                .take(rows)
                .collect()
        }
    };
    frame.render_widget(Paragraph::new(lines), body);
}

/// The records that fit `rows`, whole (the view starts on a record), with
/// a last row saying how to follow again when scrolled up.
fn logs_view(panel: &mut DebugPanel, s: Styles, w: usize, rows: usize) -> Vec<Line<'static>> {
    let records = match log_records(panel, s, w) {
        Ok(records) => records,
        Err(empty) => return empty,
    };
    let heights: Vec<usize> = records.iter().map(Vec::len).collect();
    let (mut start, mut end, mut scroll) = window(&heights, panel.scroll, rows);
    if scroll > 0 && rows > 1 {
        // Room for the "newer records" row.
        (start, end, scroll) = window(&heights, panel.scroll, rows - 1);
    }
    panel.clamp_scroll(scroll, usize::MAX);
    let room = if scroll > 0 {
        rows.saturating_sub(1)
    } else {
        rows
    };
    let mut shown: Vec<Line<'static>> = records
        .into_iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .flatten()
        .take(room)
        .collect();
    // Older records above: the newest stay against the bottom.
    if start > 0 && shown.len() < room {
        let pad = vec![Line::from(""); room - shown.len()];
        shown.splice(0..0, pad);
    }
    if scroll > 0 {
        let what = if scroll == 1 { "record" } else { "records" };
        shown.push(Line::from(Span::styled(
            format!("↓ {scroll} newer {what} · G follows"),
            s.muted(),
        )));
    }
    shown
}

/// The level's colour: status colours for WARN and ERROR only.
fn level_style(level: LogLevel, s: Styles) -> Style {
    let st = s.theme.status;
    match level {
        LogLevel::Trace | LogLevel::Debug => s.muted(),
        LogLevel::Info => s.secondary(),
        LogLevel::Warn => s.status(st.warning),
        LogLevel::Error => s.status(st.error),
    }
}

/// Every visible record, wrapped under its time and level, or the empty
/// state.
fn log_records(
    panel: &DebugPanel,
    s: Styles,
    w: usize,
) -> Result<Vec<Vec<Line<'static>>>, Vec<Line<'static>>> {
    let visible = panel.visible();
    if visible.is_empty() {
        let (a, b) = if panel.logs.is_empty() {
            match panel.session() {
                Some(_) => ("No record yet for this session.", ""),
                None => (
                    "No session selected.",
                    "Connect to a host, then come back with l.",
                ),
            }
        } else {
            (
                "No record matches.",
                "Esc clears the filter · v changes the level.",
            )
        };
        return Err(vec![
            Line::from(Span::styled(a, s.secondary())),
            Line::from(Span::styled(b, s.muted())),
        ]);
    }
    let room = w.saturating_sub(LOG_INDENT).max(10);
    Ok(visible
        .into_iter()
        .map(|record| record_lines(record, s, room))
        .collect())
}

fn record_lines(record: &LogRecord, s: Styles, room: usize) -> Vec<Line<'static>> {
    let target = display_safe(&record.target);
    let body = format!("{target}: {}", display_safe(&record.message));
    let message_style = if record.level == LogLevel::Error {
        s.title()
    } else {
        s.primary()
    };
    let mut lines = Vec::new();
    for (i, part) in fmt::wrap(&body, room).into_iter().enumerate() {
        let mut spans = if i == 0 {
            vec![
                Span::styled(format!("{} ", hms(record.at)), s.muted()),
                Span::styled(
                    format!("{:<5} ", record.level.as_str()),
                    level_style(record.level, s),
                ),
            ]
        } else {
            vec![Span::raw(" ".repeat(LOG_INDENT))]
        };
        // The target (first words of the first row) is quieter.
        match part.strip_prefix(&format!("{target}:")) {
            Some(rest) if i == 0 => {
                spans.push(Span::styled(format!("{target}:"), s.secondary()));
                spans.push(Span::styled(rest.to_owned(), message_style));
            }
            _ => spans.push(Span::styled(part, message_style)),
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn config_lines(panel: &DebugPanel, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    match panel.config.as_ref().map(|(_, load)| load) {
        None if panel.alias().is_none() => vec![
            Line::from(Span::styled("No host selected.", s.secondary())),
            Line::from(Span::styled(
                "Select a host in the list, then l.",
                s.muted(),
            )),
        ],
        None | Some(Load::Loading) => vec![Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Reading the effective config…", s.muted()),
        ])],
        Some(Load::Failed(e)) => form::notice(s, false, &e.message, e.hint.as_deref(), w),
        Some(Load::Ready(config)) => ready_config(config, panel.reveal, s, w),
    }
}

fn ready_config(config: &EffectiveConfig, reveal: bool, s: Styles, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled("Route", s.secondary()))];
    if let Some(e) = &config.route_error {
        lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
    } else {
        // `You → bastion → lab`, then each hop's target.
        let chain: Vec<String> = config
            .route
            .iter()
            .map(|hop| display_safe(&hop.alias))
            .collect();
        if !chain.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  You → ", s.muted()),
                Span::styled(
                    fmt::truncate(&chain.join(" → "), w.saturating_sub(10)),
                    s.title(),
                ),
            ]));
        }
        let hops = config.route.len();
        for (i, hop) in config.route.iter().enumerate() {
            let label = if hops == 1 {
                "direct".to_owned()
            } else {
                format!("{}/{hops}", i + 1)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {label:<7}"), s.muted()),
                Span::styled(fmt::truncate(&display_safe(&hop.alias), 24), s.secondary()),
                Span::styled(
                    format!(
                        "  {}@{}:{}",
                        display_safe(&hop.user),
                        display_safe(&hop.hostname),
                        hop.port
                    ),
                    s.muted(),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Options", s.secondary())));
    for option in &config.options {
        option_lines(&mut lines, option, reveal, s, w);
    }
    warning_lines(&mut lines, config, s, w);
    lines
}

/// One option: masked when sensitive (unless revealed); its origin on
/// the same row when the pane is wide, under it otherwise.
fn option_lines(
    lines: &mut Vec<Line<'static>>,
    option: &portico_core::effective::EffectiveOption,
    reveal: bool,
    s: Styles,
    w: usize,
) {
    let origin = match &option.origin {
        OptionOrigin::Config { file, line } => format!("{}:{line}", display_path(file)),
        other => origin_label(other),
    };
    let sensitive = is_sensitive(option);
    let hidden = sensitive && !reveal;
    let value = if hidden {
        "•••".to_owned()
    } else {
        display_safe(&option.values.join(" "))
    };
    let note = match (sensitive, hidden) {
        (_, true) => HIDDEN_NOTE,
        (true, false) => REVEALED_NOTE,
        _ => "",
    };
    let note_width = note.chars().count();
    let value_style = if hidden { s.muted() } else { s.primary() };
    if w >= WIDE {
        // `key  value  …  origin` on one row.
        let origin = fmt::truncate(&origin, w.saturating_sub(KEY_COL + 24).max(8));
        let value_room = w.saturating_sub(KEY_COL + 2 + origin.chars().count() + note_width + 2);
        let value = fmt::truncate(&value, value_room.max(4));
        let pad = w.saturating_sub(
            KEY_COL + 2 + value.chars().count() + note_width + origin.chars().count(),
        );
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<KEY_COL$}", fmt::truncate(&option.key, KEY_COL - 1)),
                s.secondary(),
            ),
            Span::styled(value, value_style),
            Span::styled(note, s.muted()),
            Span::raw(" ".repeat(pad)),
            Span::styled(origin, s.muted()),
        ]));
    } else {
        // Narrow: the origin goes under the value.
        let key = fmt::truncate(&option.key, w.saturating_sub(10).max(8));
        let value_room = w.saturating_sub(key.chars().count() + 4 + note_width);
        lines.push(Line::from(vec![
            Span::styled(format!("  {key}  "), s.secondary()),
            Span::styled(fmt::truncate(&value, value_room.max(4)), value_style),
            Span::styled(note, s.muted()),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "      {}",
                fmt::truncate(&origin, w.saturating_sub(6).max(8))
            ),
            s.muted(),
        )));
    }
}

fn warning_lines(lines: &mut Vec<Line<'static>>, config: &EffectiveConfig, s: Styles, w: usize) {
    if !config.warnings.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Warnings", s.secondary())));
        for warning in &config.warnings {
            let text = format!(
                "{} ({}:{})",
                fmt::sentence(&display_safe(&warning.message)),
                display_path(&warning.file),
                warning.line
            );
            for (i, l) in fmt::wrap(&text, w.saturating_sub(4))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 { "  ⚠ " } else { "    " };
                lines.push(Line::from(vec![
                    Span::styled(lead, s.status(s.theme.status.warning)),
                    Span::styled(l, s.secondary()),
                ]));
            }
        }
    }
}
