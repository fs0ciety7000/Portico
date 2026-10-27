//! The Sync panel, the "Set up sync" wizard, and the sync item of the
//! status bar (phase 11, S1c).
//!
//! - **Status item**: the monochrome glyph `⇅` and a label (`Synced 2 min
//!   ago`, `Syncing…`, `Waiting for laptop (3f9c0ae1)`, `Sync folder
//!   unavailable`, `Sync locked`, `Sync off`) in secondary text; conflicts,
//!   things to review and alerts are counted in status colours only.
//! - **Panel**: one tab row (counts in status colours), the tab's body,
//!   then the last result and the keys. Received hosts show the current
//!   and received destination side by side, every value whole (wrapped,
//!   never cut); received snippets show the whole command, scrollable.
//!   Values from other devices go through `display_safe` (no control or
//!   bidi character reaches the terminal).
//! - **Wizard**: one step at a time, its keys under it.
//!
//! At 82 columns or less, the panel takes the full width. Only theme
//! tokens (`Styles`).

use std::time::Instant;

use portico_core::sync::{
    ConflictSide, FolderProbe, PassphraseStrength, PendingHostChange, SyncHostValue, SyncStatus,
    SyncValue,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use secrecy::ExposeSecret;
use time::OffsetDateTime;

use crate::app::App;
use crate::form::display_safe;
use crate::keys::Load;
use crate::sync_panel::{
    Choice, GLYPH, PanelMode, PassField, PassForm, PassKind, SyncData, SyncPanel, SyncTab, TABS,
    Tone, alert_text, choices, folder_reason, status_label, subject_label,
};
use crate::sync_wizard::{BULK, BULK_NOTE, JOIN_TTL_SECS, RECOVERY, Step, SyncWizard};
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Widest panel.
const PANEL_MAX_WIDTH: u16 = 100;
/// At this screen width or less, the panel takes the full width.
const FULL_WIDTH_AT: u16 = 82;
/// Label column of the panel's rows and forms.
const LABEL: usize = 14;
/// Rows of a list above the selected item's details (hosts, snippets).
const LIST_ROWS: usize = 4;
/// Cells of the strength meter.
const METER_CELLS: usize = 12;
/// Bits that fill the meter.
const METER_FULL_BITS: u32 = 90;

/// The style of a status tone (meaning only).
fn tone_style(s: Styles, tone: Tone) -> Style {
    match tone {
        Tone::Neutral => s.secondary(),
        Tone::Busy => s.status(s.theme.status.connecting),
        Tone::Warning => s.status(s.theme.status.warning),
        Tone::Error => s.status(s.theme.status.error),
    }
}

fn warning(s: Styles) -> Style {
    s.status(s.theme.status.warning)
}

fn error(s: Styles) -> Style {
    s.status(s.theme.status.error)
}

fn ok(s: Styles) -> Style {
    s.status(s.theme.status.connected)
}

// ── status bar item ──────────────────────────────────────────────────────

/// `⇅ Synced 2 min ago · 2 conflicts · 1 to review ` (`None` until the
/// engine answered). `quiet`: sync is off and nothing waits (shown only
/// when there is room).
pub(crate) fn status_spans(app: &App, max_label: usize) -> Option<(Vec<Span<'static>>, bool)> {
    let s = app.styles();
    let status = app.sync.status()?;
    let (label, tone) = status_label(status, OffsetDateTime::now_utc());
    let mut spans = vec![
        Span::styled(format!("{GLYPH} "), s.secondary()),
        Span::styled(fmt::truncate(&label, max_label), tone_label_style(s, tone)),
    ];
    if let SyncStatus::Alert { .. } | SyncStatus::Error { .. } = status {
        spans.insert(0, Span::styled("▲ ", error(s)));
    }
    if app.sync.conflicts > 0 {
        spans.push(Span::styled(" · ", s.muted()));
        spans.push(Span::styled(
            plural(app.sync.conflicts, "conflict", "conflicts"),
            warning(s),
        ));
    }
    let review = app.sync.to_review();
    if review > 0 {
        spans.push(Span::styled(" · ", s.muted()));
        spans.push(Span::styled(format!("{review} to review"), warning(s)));
    }
    spans.push(Span::raw(" "));
    let quiet = matches!(status, SyncStatus::Disabled) && review == 0;
    Some((spans, quiet))
}

/// The label stays secondary unless the state itself is a warning or an
/// error (then its status colour).
fn tone_label_style(s: Styles, tone: Tone) -> Style {
    match tone {
        Tone::Neutral | Tone::Busy => s.secondary(),
        other => tone_style(s, other),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

// ── host list: badge, details line, banner ───────────────────────────────

/// `changed by laptop (3f9c0ae1)` / `new from laptop (3f9c0ae1)`.
pub(crate) fn received_label(change: &PendingHostChange) -> String {
    let devices = if change.devices.is_empty() {
        "another device".to_owned()
    } else {
        change.devices.join(", ")
    };
    let verb = if change.new { "new from" } else { "changed by" };
    display_safe(&format!("{verb} {devices}"))
}

/// The banner under the details: what waits for this device.
pub(crate) fn banner_lines(app: &App, w: usize) -> Vec<Line<'static>> {
    let s = app.styles();
    let hosts = app.sync.pending.len();
    let snippets = app.sync.snippets.len();
    if hosts + snippets == 0 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    if hosts > 0 {
        parts.push(plural(hosts, "received host", "received hosts"));
    }
    if snippets > 0 {
        parts.push(plural(snippets, "received snippet", "received snippets"));
    }
    let text = format!(
        "{} wait for your confirmation. Nothing runs or is written before.",
        parts.join(" and ")
    );
    let mut lines: Vec<Line<'static>> = fmt::wrap(&text, w)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, s.secondary())))
        .collect();
    let key = app.keymap.shown("hosts.sync", 1);
    if !key.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(format!(" {key} "), s.key_hint()),
            Span::styled(" review", s.muted()),
        ]));
    }
    lines
}

// ── panel ────────────────────────────────────────────────────────────────

pub(crate) fn render_panel(app: &mut App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let tick = app.tick;
    let now = Instant::now();
    let dialog_open = !app.dialogs.is_empty();
    let data = &app.sync;
    let Some(panel) = &mut app.sync_panel else {
        return;
    };
    let screen = frame.area();
    let width = if screen.width <= FULL_WIDTH_AT {
        screen.width
    } else {
        screen.width.saturating_sub(2).min(PANEL_MAX_WIDTH)
    };
    let height = screen.height.saturating_sub(2).min(34);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let title = data
        .status()
        .map(|st| status_label(st, OffsetDateTime::now_utc()));
    let mut title_spans = vec![Span::styled(" Sync ", s.title())];
    if let Some((label, tone)) = title {
        title_spans.push(Span::styled(format!("· {GLYPH} "), s.muted()));
        title_spans.push(Span::styled(format!("{label} "), tone_label_style(s, tone)));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(title_spans))
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = usize::from(inner.width);

    let mut foot: Vec<Line<'static>> = Vec::new();
    if let Some(words) = panel.busy {
        foot.push(Line::from(vec![
            Span::styled(format!("{} ", spinner(tick)), tone_style(s, Tone::Busy)),
            Span::styled(words, s.secondary()),
        ]));
    }
    if let Some(n) = &panel.notice {
        foot.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
    }
    if !dialog_open {
        let hints = panel.hints(data, now);
        if !foot.is_empty() {
            foot.push(Line::from(""));
        }
        foot.extend(form::hints(s, &hints, true, w));
    }
    let foot_height = u16::try_from(foot.len()).unwrap_or(4);
    let [tabs_area, body_area, foot_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(foot_height),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(tab_row(panel, data, s, w)), tabs_area);
    frame.render_widget(Paragraph::new(foot), foot_area);

    let body_height = usize::from(body_area.height);
    let (lines, focus_line) = body_lines(panel, data, s, tick, w);
    // The selected item stays in view; long text scrolls (`PgDn`).
    let total = lines.len();
    let max_first = total.saturating_sub(body_height);
    panel.max_scroll = u16::try_from(max_first).unwrap_or(u16::MAX);
    let mut first = usize::from(panel.scroll).min(max_first);
    if let Some(focus) = focus_line
        && (focus < first || focus >= first + body_height)
    {
        first = focus.saturating_sub(1).min(max_first);
    }
    let shown: Vec<Line<'static>> = lines.into_iter().skip(first).take(body_height).collect();
    frame.render_widget(Paragraph::new(shown), body_area);
    if first + body_height < total {
        let more = format!(" ↓ {} more ", total - first - body_height);
        let x = body_area
            .right()
            .saturating_sub(u16::try_from(more.chars().count()).unwrap_or(0));
        let y = body_area.bottom().saturating_sub(1);
        frame.render_widget(
            Paragraph::new(Span::styled(more, s.muted())),
            Rect {
                x,
                y,
                width: body_area.right().saturating_sub(x),
                height: 1,
            },
        );
    }
}

/// `Overview  Conflicts 2  Issues  Hosts 1  Snippets`, then a rule.
fn tab_row(panel: &SyncPanel, data: &SyncData, s: Styles, w: usize) -> Vec<Line<'static>> {
    let count = |tab: SyncTab| -> usize {
        match tab {
            SyncTab::Overview => 0,
            SyncTab::Conflicts => panel.conflicts.ready().map_or(data.conflicts, Vec::len),
            SyncTab::Issues => panel.issues.ready().map_or(0, Vec::len),
            SyncTab::Hosts => data.pending.len(),
            SyncTab::Snippets => data.snippets.len(),
        }
    };
    let full: usize = TABS.iter().map(|t| t.title().chars().count() + 6).sum();
    let mut spans = Vec::new();
    for (i, tab) in TABS.iter().enumerate() {
        let title = if full > w { tab.short() } else { tab.title() };
        let selected = *tab == panel.tab;
        let style = if selected {
            s.accent().add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            s.muted()
        };
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            if selected { "▍" } else { " " }.to_owned(),
            s.accent(),
        ));
        spans.push(Span::styled(title.to_owned(), style));
        let n = count(*tab);
        if n > 0 {
            spans.push(Span::styled(format!(" {n}"), warning(s)));
        }
    }
    vec![
        Line::from(spans),
        Line::from(Span::styled("─".repeat(w), s.border())),
    ]
}

/// The body of the tab (or of the open form), and the line to keep in view.
fn body_lines(
    panel: &SyncPanel,
    data: &SyncData,
    s: Styles,
    tick: u64,
    w: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    match &panel.mode {
        PanelMode::Browse | PanelMode::Skip { .. } => {}
        PanelMode::Unlock(f) => return (unlock_lines(f, s, tick, w), None),
        PanelMode::Change(f) => return (change_lines(f, s, tick, w), None),
        PanelMode::Reset { form, confirm } => {
            return (reset_lines(form, confirm.is_some(), s, tick, w), None);
        }
        PanelMode::Rename { name, problem } => {
            let mut lines = vec![
                Line::from(Span::styled("Rename this device", s.title())),
                Line::from(""),
            ];
            lines.extend(fmt::wrap(
                "Other devices show the new name with this device's id, so a copy can never pass for it.",
                w,
            )
            .into_iter()
            .map(|l| Line::from(Span::styled(l, s.secondary()))));
            lines.push(Line::from(""));
            lines.push(form::input(
                s,
                "Name",
                LABEL,
                name,
                true,
                w.saturating_sub(LABEL).min(48),
            ));
            if let Some(e) = problem {
                lines.extend(form::problem(s, LABEL, &e.message, w));
            }
            return (lines, None);
        }
        PanelMode::Disable { options, focus } => {
            return (disable_lines(*options, *focus, s, w), None);
        }
    }
    let mut lines = match panel.tab {
        SyncTab::Overview => overview_lines(panel, data, s, tick, w),
        SyncTab::Conflicts => return conflict_lines(panel, s, tick, w),
        SyncTab::Issues => return issue_lines(panel, s, tick, w),
        SyncTab::Hosts => return host_lines(panel, data, s, tick, w),
        SyncTab::Snippets => snippet_lines(panel, data, s, tick, w),
    };
    if let PanelMode::Skip { label, .. } = &panel.mode {
        lines.push(Line::from(""));
        lines.push(question(s, &format!("Skip {label} on this device?")));
        lines.extend(paragraph(
            s.secondary(),
            "It stays as it is here and is no longer applied on this device. The other devices keep it.",
            w,
        ));
    }
    (lines, None)
}

fn paragraph(style: Style, text: &str, w: usize) -> Vec<Line<'static>> {
    fmt::wrap(text, w)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, style)))
        .collect()
}

fn question(s: Styles, text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("▲ ", warning(s)),
        Span::styled(text.to_owned(), s.title()),
    ])
}

fn loading(s: Styles, tick: u64, text: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", spinner(tick)), tone_style(s, Tone::Busy)),
        Span::styled(text.to_owned(), s.muted()),
    ])
}

/// `Label          value`.
fn row(s: Styles, label: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<LABEL$}"), s.muted())];
    spans.extend(value);
    Line::from(spans)
}

/// `when` as `2 min ago` (local clock), or `never`.
fn ago(when: Option<i64>) -> String {
    when.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
        .map_or_else(
            || "never".to_owned(),
            |at| crate::app_update::ago(at, OffsetDateTime::now_utc()),
        )
}

const OFF_TEXT: &str = "Portico can keep your hosts, tags, notes, snippets and settings in step across your devices, through a folder you already sync: iCloud Drive, Dropbox, OneDrive, Syncthing, a git repository…";
const PRIVATE_TEXT: &str = "Only encrypted files leave this machine. Private keys and known_hosts are never synced, and Portico itself sends nothing over the network.";

#[allow(clippy::too_many_lines)] // one block per status
fn overview_lines(
    panel: &SyncPanel,
    data: &SyncData,
    s: Styles,
    tick: u64,
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(overview) = &data.overview else {
        lines.push(loading(s, tick, "Reading the sync state…"));
        return lines;
    };
    match &overview.status {
        SyncStatus::Disabled => {
            lines.push(Line::from(Span::styled(
                "Sync is off on this device.",
                s.title(),
            )));
            lines.push(Line::from(""));
            lines.extend(paragraph(s.secondary(), OFF_TEXT, w));
            lines.push(Line::from(""));
            lines.extend(paragraph(s.secondary(), PRIVATE_TEXT, w));
            if !data.snippets.is_empty() {
                lines.push(Line::from(""));
                lines.extend(paragraph(
                    warning(s),
                    &format!(
                        "{} changed by sync still wait for your review (Received snippets): they do not run before.",
                        plural(data.snippets.len(), "snippet", "snippets")
                    ),
                    w,
                ));
            }
            return lines;
        }
        SyncStatus::Locked => {
            lines.push(Line::from(Span::styled("Sync is locked.", s.title())));
            lines.push(Line::from(""));
            lines.extend(paragraph(
                s.secondary(),
                "The key is not remembered on this device: type the passphrase to unlock sync. Nothing is synced meanwhile; your local data stays as it is.",
                w,
            ));
            lines.push(Line::from(""));
        }
        _ => {}
    }
    let (label, tone) = status_label(&overview.status, OffsetDateTime::now_utc());
    lines.push(row(
        s,
        "Status",
        vec![
            Span::styled(format!("{GLYPH} "), s.secondary()),
            Span::styled(label, tone_label_style(s, tone)),
        ],
    ));
    if let Some(folder) = &overview.folder {
        let shown = fmt::display_path(&folder.to_string_lossy());
        lines.push(row(
            s,
            "Folder",
            vec![Span::styled(
                fmt::truncate_middle(&shown, w.saturating_sub(LABEL)),
                s.primary(),
            )],
        ));
    }
    lines.push(row(
        s,
        "Key",
        vec![Span::styled(
            if overview.remember_key {
                "remembered on this device"
            } else {
                "not remembered: the passphrase is asked after each restart"
            },
            s.secondary(),
        )],
    ));
    // What the status means, and what to do.
    let explain: Option<(String, &'static str, Style)> = match &overview.status {
        SyncStatus::FolderUnavailable { reason } => {
            let (what, hint) = folder_reason(*reason);
            Some((what.to_owned(), hint, warning(s)))
        }
        SyncStatus::Alert { kind, device } => {
            let (what, hint) = alert_text(*kind, device.as_deref().map(display_safe).as_deref());
            Some((what, hint, error(s)))
        }
        SyncStatus::Error { summary } => Some((
            fmt::sentence(&summary.message),
            "Sync is paused on this device; your local data stays as it is.",
            error(s),
        )),
        SyncStatus::Waiting { .. } => Some((
            "Some data is not in the folder yet.".to_owned(),
            "It usually arrives on its own once your sync app has finished.",
            s.secondary(),
        )),
        _ => None,
    };
    if let Some((what, hint, style)) = explain {
        lines.push(Line::from(""));
        for (i, l) in fmt::wrap(&what, w.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { "▲ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(lead, style),
                Span::styled(l, style),
            ]));
        }
        for l in fmt::wrap(hint, w.saturating_sub(2)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(l, s.secondary()),
            ]));
        }
    }
    lines.push(Line::from(""));
    match &panel.devices {
        Load::Loading => lines.push(loading(s, tick, "Reading the devices…")),
        Load::Failed(e) if !matches!(overview.status, SyncStatus::Locked) => {
            lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
        }
        Load::Failed(_) => {}
        Load::Ready(devices) => {
            lines.push(Line::from(vec![
                Span::styled("Devices", s.title()),
                Span::styled(format!(" · {}", devices.len()), s.muted()),
            ]));
            let label_w = devices
                .iter()
                .map(|d| display_safe(&d.label).chars().count())
                .max()
                .unwrap_or(0)
                .min(w.saturating_sub(30).max(12));
            for d in devices {
                let label = fmt::truncate(&display_safe(&d.label), label_w);
                let mut spans = vec![
                    Span::styled(
                        if d.this_device { "● " } else { "○ " },
                        if d.this_device { s.accent() } else { s.muted() },
                    ),
                    Span::styled(format!("{label:<label_w$}"), s.primary()),
                    Span::raw("  "),
                ];
                if d.this_device {
                    spans.push(Span::styled("this device", s.accent()));
                } else {
                    spans.push(Span::styled(
                        format!("published {}", ago(d.last_seen.or(d.published_at))),
                        s.muted(),
                    ));
                }
                if d.stale {
                    spans.push(Span::styled(" · inactive", warning(s)));
                }
                if d.revoked {
                    spans.push(Span::styled(" · removed", s.muted()));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    lines
}

fn strength_line(strength: Option<PassphraseStrength>, s: Styles) -> Line<'static> {
    let Some(st) = strength else {
        return Line::from(vec![
            Span::styled(format!("{:<LABEL$}", "Strength"), s.muted()),
            Span::styled(
                format!(
                    "{}  at least {} bits",
                    "▱".repeat(METER_CELLS),
                    portico_core::sync::passphrase::MIN_ESTIMATED_BITS
                ),
                s.muted(),
            ),
        ]);
    };
    let filled = usize::try_from(st.estimated_bits.min(METER_FULL_BITS))
        .unwrap_or(0)
        .saturating_mul(METER_CELLS)
        / usize::try_from(METER_FULL_BITS).unwrap_or(1);
    let style = if st.acceptable { ok(s) } else { warning(s) };
    let words = if st.acceptable {
        "strong enough".to_owned()
    } else {
        format!(
            "too weak, {} needed",
            portico_core::sync::passphrase::MIN_ESTIMATED_BITS
        )
    };
    Line::from(vec![
        Span::styled(format!("{:<LABEL$}", "Strength"), s.muted()),
        Span::styled("▰".repeat(filled), style),
        Span::styled("▱".repeat(METER_CELLS.saturating_sub(filled)), s.muted()),
        Span::styled(format!("  {} bits · ", st.estimated_bits), s.secondary()),
        Span::styled(words, style),
    ])
}

/// The fields of a passphrase form.
#[allow(clippy::too_many_lines)] // one block per field, in order
fn pass_fields(f: &PassForm, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let input_w = w.saturating_sub(LABEL).min(52);
    let mut lines = Vec::new();
    let focused = f.focused();
    let problem_of = |field: PassField| {
        f.problem
            .as_ref()
            .filter(|(pf, ..)| *pf == field)
            .map(|(_, m, h)| (m.clone(), h.clone()))
    };
    let push_problem = |lines: &mut Vec<Line<'static>>, field: PassField| {
        if let Some((message, hint)) = problem_of(field) {
            lines.extend(form::problem(s, LABEL, &message, w));
            if let Some(hint) = hint {
                lines.extend(
                    fmt::wrap(&hint, w.saturating_sub(LABEL).max(10))
                        .into_iter()
                        .map(|l| {
                            Line::from(vec![
                                Span::raw(" ".repeat(LABEL)),
                                Span::styled(l, s.secondary()),
                            ])
                        }),
                );
            }
        }
    };
    for field in f.fields() {
        let on = focused == Some(*field);
        match field {
            PassField::Old => {
                let label = match f.kind {
                    PassKind::Change => "Current",
                    _ => "Passphrase",
                };
                lines.push(form::secret(
                    s,
                    label,
                    LABEL,
                    !f.old.is_empty(),
                    on,
                    input_w,
                ));
                push_problem(&mut lines, *field);
            }
            PassField::New => {
                let label = if f.kind == PassKind::Create {
                    "Passphrase"
                } else {
                    "New"
                };
                if let Some(text) = f.generated_text() {
                    // Shown once, whole, in the accent: it is to be written down.
                    let words = text.expose_secret().to_owned();
                    let label_style = if on { s.accent() } else { s.muted() };
                    let mut first = true;
                    for l in fmt::wrap(&words, w.saturating_sub(LABEL)) {
                        let lead = if first { label } else { "" };
                        first = false;
                        lines.push(Line::from(vec![
                            Span::styled(format!("{lead:<LABEL$}"), label_style),
                            Span::styled(l, s.title()),
                        ]));
                    }
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(LABEL)),
                        Span::styled("Generated: 6 random words. Shown only here.", s.muted()),
                    ]));
                } else {
                    lines.push(form::secret(
                        s,
                        label,
                        LABEL,
                        !f.new.is_empty(),
                        on,
                        input_w,
                    ));
                }
                lines.push(strength_line(f.strength, s));
                push_problem(&mut lines, *field);
            }
            PassField::Confirm => {
                if f.generated.is_some() {
                    lines.push(form::toggle(
                        s,
                        "",
                        LABEL,
                        f.wrote_down,
                        "I wrote it down",
                        on,
                    ));
                } else {
                    lines.push(form::secret(
                        s,
                        "Confirm",
                        LABEL,
                        !f.confirm.is_empty(),
                        on,
                        input_w,
                    ));
                }
                push_problem(&mut lines, *field);
            }
            PassField::Name => {
                lines.push(Line::from(""));
                lines.push(form::input(s, "Device name", LABEL, &f.name, on, input_w));
                push_problem(&mut lines, *field);
            }
            PassField::Remember => {
                lines.push(form::toggle(
                    s,
                    "",
                    LABEL,
                    f.remember,
                    "Remember the key on this device",
                    on,
                ));
                let note = "Off on a shared computer: sync then asks for the passphrase after each restart.";
                for l in fmt::wrap(note, w.saturating_sub(LABEL).max(10)) {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(LABEL)),
                        Span::styled(l, s.muted()),
                    ]));
                }
            }
        }
    }
    if f.busy {
        lines.push(Line::from(""));
        lines.push(loading(
            s,
            tick,
            "Deriving the key: this takes a few seconds on purpose…",
        ));
    }
    lines
}

fn unlock_lines(f: &PassForm, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Unlock sync", s.title())),
        Line::from(""),
    ];
    lines.extend(paragraph(
        s.secondary(),
        "Type the sync passphrase. It opens the key for this session; whether it is remembered is the Key option of the Overview.",
        w,
    ));
    lines.push(Line::from(""));
    lines.extend(pass_fields(f, s, tick, w));
    lines
}

/// F2, word for word as in the GUI.
pub(crate) const CHANGE_NOTE: &str = "Changing the passphrase does not protect against one that already leaked: the data key stays the same. If it leaked, disable sync and create a new sync folder.";

fn change_lines(f: &PassForm, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Change the passphrase", s.title())),
        Line::from(""),
    ];
    lines.extend(paragraph(warning(s), CHANGE_NOTE, w));
    lines.push(Line::from(""));
    lines.extend(pass_fields(f, s, tick, w));
    lines
}

/// Reset from this device, word for word as in the GUI.
pub(crate) const RESET_NOTE: &str = "For a lost passphrase: this device holds the key, so it can set a new passphrase without the old one. Every other device shows a blocking alert until someone acknowledges it there.";

fn reset_lines(
    f: &PassForm,
    confirming: bool,
    s: Styles,
    tick: u64,
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Reset the passphrase", s.title())),
        Line::from(""),
    ];
    lines.extend(paragraph(s.secondary(), RESET_NOTE, w));
    lines.push(Line::from(""));
    lines.extend(pass_fields(f, s, tick, w));
    if confirming {
        lines.push(Line::from(""));
        lines.push(question(s, "Reset the sync passphrase from this device?"));
        lines.extend(paragraph(
            s.secondary(),
            "The other devices will ask to acknowledge it. y resets, n keeps the current one.",
            w,
        ));
    }
    lines
}

fn disable_lines(
    options: portico_core::sync::DisableOptions,
    focus: usize,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled("Disable sync on this device", s.title())),
        Line::from(""),
    ];
    lines.extend(paragraph(
        s.secondary(),
        "Your local hosts, snippets and settings stay on this device. The other devices keep syncing.",
        w,
    ));
    lines.push(Line::from(""));
    lines.push(form::toggle(
        s,
        "",
        2,
        options.forget_key,
        "Forget the key on this device",
        focus == 0,
    ));
    lines.push(form::toggle(
        s,
        "",
        2,
        options.remove_my_file,
        "Remove this device's file from the sync folder",
        focus == 1,
    ));
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "Received snippets you have not confirmed stay blocked after this.",
        w,
    ));
    lines
}

// ── conflicts ────────────────────────────────────────────────────────────

/// One field of a value: (label, text). Host fields always listed, so
/// the sides line up.
fn value_fields(value: &SyncValue) -> Vec<(&'static str, String)> {
    let opt = |v: &Option<String>| v.as_deref().map_or_else(|| "—".to_owned(), display_safe);
    match value {
        SyncValue::Host(h) => host_fields(h),
        SyncValue::Text(t) => vec![("Value", display_safe(t))],
        SyncValue::Bool(b) => vec![("Value", if *b { "yes" } else { "no" }.to_owned())],
        SyncValue::Tags(tags) => vec![(
            "Tags",
            if tags.is_empty() {
                "—".to_owned()
            } else {
                tags.iter()
                    .map(|t| format!("#{}", display_safe(t)))
                    .collect::<Vec<_>>()
                    .join(" ")
            },
        )],
        SyncValue::Snippet(sn) => vec![
            ("Name", display_safe(&sn.name)),
            ("Command", sn.command.clone()),
            ("Description", opt(&sn.description)),
            (
                "When sent",
                if sn.run {
                    "runs (adds ↵)"
                } else {
                    "pasted only"
                }
                .to_owned(),
            ),
        ],
        SyncValue::Deleted => vec![("Value", "deleted".to_owned())],
        _ => vec![("Value", "a value this version cannot show".to_owned())],
    }
}

fn host_fields(h: &SyncHostValue) -> Vec<(&'static str, String)> {
    let opt = |v: &Option<String>| v.as_deref().map_or_else(|| "—".to_owned(), display_safe);
    vec![
        ("HostName", display_safe(&h.hostname)),
        ("User", opt(&h.user)),
        (
            "Port",
            h.port.map_or_else(|| "—".to_owned(), |p| p.to_string()),
        ),
        ("IdentityFile", opt(&h.identity_file)),
        ("ProxyJump", opt(&h.proxy_jump)),
    ]
}

/// A value's lines: fields that differ between the sides in the primary
/// text, the others muted. Multi-line values (commands) keep their lines,
/// wrapped by characters: nothing is cut.
fn field_lines(
    fields: &[(&'static str, String)],
    differs: &dyn Fn(&str) -> bool,
    indent: usize,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    let room = w.saturating_sub(indent + LABEL).max(8);
    let mut lines = Vec::new();
    for (label, text) in fields {
        let style = if differs(label) {
            s.primary()
        } else {
            s.muted()
        };
        let mut first = true;
        for chunk in text
            .split('\n')
            .flat_map(|l| chunks(&display_safe(l), room))
        {
            let lead = if first { *label } else { "" };
            first = false;
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(format!("{lead:<LABEL$}"), s.muted()),
                Span::styled(chunk, style),
            ]));
        }
    }
    lines
}

/// `text` cut into pieces of `width` characters (at least one piece).
fn chunks(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

fn side_head(side: &ConflictSide) -> String {
    let who = if side.devices.is_empty() {
        "unknown device".to_owned()
    } else {
        display_safe(&side.devices.join(", "))
    };
    let mine = if side.local { " · this device" } else { "" };
    format!("{who}{mine} · {}", ago(Some(side.at)))
}

#[allow(clippy::too_many_lines)] // the choices and their values
fn conflict_lines(
    panel: &SyncPanel,
    s: Styles,
    tick: u64,
    w: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    let list = match &panel.conflicts {
        Load::Loading => return (vec![loading(s, tick, "Reading the conflicts…")], None),
        Load::Failed(e) => {
            return (
                form::notice(s, false, &e.message, e.hint.as_deref(), w),
                None,
            );
        }
        Load::Ready(list) => list,
    };
    let Some(conflict) = panel.selected_conflict() else {
        let mut lines = vec![Line::from(Span::styled("No conflicts.", s.secondary()))];
        lines.extend(paragraph(
            s.muted(),
            "When two devices change the same thing at the same time, it shows here. Nothing is settled silently.",
            w,
        ));
        return (lines, None);
    };
    let position = panel.conflict.min(list.len().saturating_sub(1)) + 1;
    let mut lines = vec![Line::from(vec![
        Span::styled(display_safe(&subject_label(&conflict.subject)), s.title()),
        Span::styled(format!("  ·  {position} of {}", list.len()), s.muted()),
    ])];
    lines.push(Line::from(""));
    // Which fields differ between the sides.
    let all: Vec<Vec<(&'static str, String)>> = conflict
        .sides
        .iter()
        .map(|side| value_fields(&side.value))
        .collect();
    let differs = |label: &str| {
        let values: Vec<Option<&String>> = all
            .iter()
            .map(|f| f.iter().find(|(l, _)| *l == label).map(|(_, v)| v))
            .collect();
        values.windows(2).any(|p| p.first() != p.get(1))
    };
    let options = choices(conflict);
    let selected = panel.choice.min(options.len().saturating_sub(1));
    let mut focus = None;
    for (i, choice) in options.iter().enumerate() {
        let on = i == selected;
        if on {
            focus = Some(lines.len());
        }
        let mark = if on { "▍" } else { " " };
        let radio = if on { "● " } else { "○ " };
        let head_style = if on { s.title() } else { s.secondary() };
        let head = match choice {
            Choice::Union => "Union of all sides".to_owned(),
            Choice::KeepBoth { .. } => "Keep both".to_owned(),
            Choice::Pick { index } => conflict
                .sides
                .iter()
                .find(|side| side.index == *index)
                .map_or_else(
                    || "A side".to_owned(),
                    |side| format!("Use {}", side_head(side)),
                ),
        };
        lines.push(Line::from(vec![
            Span::styled(mark, s.accent()),
            Span::styled(radio, if on { s.accent() } else { s.muted() }),
            Span::styled(fmt::truncate(&head, w.saturating_sub(3)), head_style),
        ]));
        match choice {
            Choice::Union => {
                let mut union: Vec<String> = Vec::new();
                for side in &conflict.sides {
                    if let SyncValue::Tags(tags) = &side.value {
                        for t in tags {
                            if !union.contains(t) {
                                union.push(t.clone());
                            }
                        }
                    }
                }
                let text = union
                    .iter()
                    .map(|t| format!("#{}", display_safe(t)))
                    .collect::<Vec<_>>()
                    .join(" ");
                lines.extend(field_lines(&[("Tags", text)], &|_| true, 3, s, w));
            }
            Choice::KeepBoth { keep } => {
                let keeper = conflict
                    .sides
                    .iter()
                    .find(|side| side.index == *keep)
                    .map_or_else(String::new, side_head);
                lines.extend(paragraph(
                    s.muted(),
                    &format!(
                        "   The version of {keeper} keeps its name; every other version becomes a copy. Copies from other devices wait for your review before they run."
                    ),
                    w,
                ));
            }
            Choice::Pick { index } => {
                if let Some(fields) = conflict
                    .sides
                    .iter()
                    .position(|side| side.index == *index)
                    .and_then(|p| all.get(p))
                {
                    lines.extend(field_lines(fields, &differs, 3, s, w));
                }
            }
        }
        lines.push(Line::from(""));
    }
    (lines, focus)
}

// ── issues ───────────────────────────────────────────────────────────────

fn issue_lines(
    panel: &SyncPanel,
    s: Styles,
    tick: u64,
    w: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    let list = match &panel.issues {
        Load::Loading => return (vec![loading(s, tick, "Reading the issues…")], None),
        Load::Failed(e) => {
            return (
                form::notice(s, false, &e.message, e.hint.as_deref(), w),
                None,
            );
        }
        Load::Ready(list) => list,
    };
    if list.is_empty() {
        let mut lines = vec![Line::from(Span::styled("No issues.", s.secondary()))];
        lines.extend(paragraph(
            s.muted(),
            "A record that cannot be applied here (an alias already in ~/.ssh/config, a missing jump host or key…) shows here with what to do.",
            w,
        ));
        return (lines, None);
    }
    let selected = panel.issue.min(list.len().saturating_sub(1));
    let mut lines = Vec::new();
    let mut focus = None;
    for (i, issue) in list.iter().enumerate() {
        let on = i == selected;
        if on {
            focus = Some(lines.len());
        }
        let (icon, style) = if issue.warning {
            ("▲ ", warning(s))
        } else {
            ("✕ ", error(s))
        };
        let subject = issue
            .subject
            .as_ref()
            .map(|sub| format!("{} · ", display_safe(&subject_label(sub))))
            .unwrap_or_default();
        let message = fmt::sentence(&display_safe(&issue.summary.message));
        let mark = if on { "▍" } else { " " };
        for (j, l) in fmt::wrap(&format!("{subject}{message}"), w.saturating_sub(3))
            .into_iter()
            .enumerate()
        {
            lines.push(Line::from(vec![
                Span::styled(if j == 0 { mark } else { " " }, s.accent()),
                Span::styled(if j == 0 { icon } else { "  " }, style),
                Span::styled(l, if on { s.title() } else { s.primary() }),
            ]));
        }
        if let Some(hint) = &issue.summary.hint {
            for l in fmt::wrap(&display_safe(hint), w.saturating_sub(3)) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(l, s.secondary()),
                ]));
            }
        }
        let mut meta = vec![
            Span::raw("   "),
            Span::styled(issue.summary.code.clone(), s.muted()),
        ];
        if issue.skippable {
            meta.push(Span::styled(" · can be skipped on this device", s.muted()));
        }
        lines.push(Line::from(meta));
        lines.push(Line::from(""));
    }
    (lines, focus)
}

// ── received hosts ───────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)] // the list, then the selected host's card
fn host_lines(
    panel: &SyncPanel,
    data: &SyncData,
    s: Styles,
    tick: u64,
    w: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    if data.pending.is_empty() {
        let mut lines = if data.stale_hosts {
            vec![loading(s, tick, "Reading the received hosts…")]
        } else {
            vec![Line::from(Span::styled(
                "No received host waits for you.",
                s.secondary(),
            ))]
        };
        lines.extend(paragraph(
            s.muted(),
            "When another device adds a host or changes where one connects, it waits here: nothing is written to hosts.conf before you confirm it on this device.",
            w,
        ));
        return (lines, None);
    }
    let selected = panel.host.min(data.pending.len().saturating_sub(1));
    let mut lines = Vec::new();
    // The list (a window around the selection).
    let first = selected.saturating_sub(LIST_ROWS.saturating_sub(1));
    for (i, change) in data.pending.iter().enumerate().skip(first).take(LIST_ROWS) {
        let on = i == selected;
        let alias = display_safe(&change.alias);
        let label = received_label(change);
        let alias_w = alias.chars().count().min(w / 2);
        lines.push(Line::from(vec![
            Span::styled(if on { "▍" } else { " " }, s.accent()),
            Span::styled(
                format!("{}  ", fmt::truncate(&alias, alias_w)),
                if on { s.title() } else { s.primary() },
            ),
            Span::styled(
                fmt::truncate(&label, w.saturating_sub(alias_w + 4)),
                warning(s),
            ),
        ]));
    }
    if data.pending.len() > LIST_ROWS {
        lines.push(Line::from(Span::styled(
            format!("  {} of {}", selected + 1, data.pending.len()),
            s.muted(),
        )));
    }
    lines.push(Line::from(Span::styled("─".repeat(w), s.border())));
    let Some(change) = data.pending.get(selected) else {
        return (lines, None);
    };
    let focus = Some(lines.len());
    lines.push(Line::from(vec![
        Span::styled(display_safe(&change.alias), s.title()),
        Span::styled(format!("  {}", received_label(change)), warning(s)),
    ]));
    if let Some(from) = &change.renamed_from {
        lines.push(Line::from(Span::styled(
            format!("Renamed from {} on the other device", display_safe(from)),
            s.secondary(),
        )));
    }
    if data.stale_hosts {
        lines.push(loading(s, tick, "Updating: it changed on another device…"));
    }
    lines.push(Line::from(""));
    // Current vs received, every value whole.
    let received = host_fields(&change.incoming);
    let current = change.current.as_ref().map(host_fields);
    let col = match &current {
        // Room for the gap and the `◂` mark of a changed value.
        Some(_) => w.saturating_sub(LABEL + 4) / 2,
        None => w.saturating_sub(LABEL),
    }
    .max(8);
    let mut head = vec![Span::raw(" ".repeat(LABEL))];
    if current.is_some() {
        head.push(Span::styled(
            format!("{:<col$}  ", "Now on this device"),
            s.muted(),
        ));
    }
    head.push(Span::styled(
        if change.new {
            "Received (new host)"
        } else {
            "Received"
        },
        s.muted(),
    ));
    lines.push(Line::from(head));
    for (i, (label, value)) in received.iter().enumerate() {
        let now = current
            .as_ref()
            .and_then(|c| c.get(i))
            .map(|(_, v)| v.clone());
        let differs = now.as_ref().is_some_and(|n| n != value) || change.new;
        let left = now.as_deref().map(|n| chunks(n, col)).unwrap_or_default();
        let right = chunks(value, col);
        let rows = left.len().max(right.len());
        for r in 0..rows {
            let mut spans = vec![Span::styled(
                format!("{:<LABEL$}", if r == 0 { *label } else { "" }),
                s.muted(),
            )];
            if current.is_some() {
                let piece = left.get(r).cloned().unwrap_or_default();
                spans.push(Span::styled(format!("{piece:<col$}  "), s.secondary()));
            }
            let piece = right.get(r).cloned().unwrap_or_default();
            spans.push(Span::styled(
                piece,
                if differs { s.title() } else { s.muted() },
            ));
            if r == 0 && differs && current.is_some() {
                spans.push(Span::styled(" ◂", warning(s)));
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "y writes this destination to hosts.conf on this device. n keeps it out here; the other devices keep it.",
        w,
    ));
    (lines, focus)
}

// ── received snippets ────────────────────────────────────────────────────

fn snippet_lines(
    panel: &SyncPanel,
    data: &SyncData,
    s: Styles,
    tick: u64,
    w: usize,
) -> Vec<Line<'static>> {
    if data.snippets.is_empty() {
        let mut lines = if data.stale_snippets {
            vec![loading(s, tick, "Reading the received snippets…")]
        } else {
            vec![Line::from(Span::styled(
                "No received snippet waits for you.",
                s.secondary(),
            ))]
        };
        lines.extend(paragraph(
            s.muted(),
            "A snippet added or changed on another device does not run here before you read its whole command and confirm it.",
            w,
        ));
        return lines;
    }
    let selected = panel.snippet.min(data.snippets.len().saturating_sub(1));
    let mut lines = Vec::new();
    let first = selected.saturating_sub(LIST_ROWS.saturating_sub(1));
    for (i, sn) in data.snippets.iter().enumerate().skip(first).take(LIST_ROWS) {
        let on = i == selected;
        lines.push(Line::from(vec![
            Span::styled(if on { "▍" } else { " " }, s.accent()),
            Span::styled(
                fmt::truncate(&display_safe(&sn.snippet.name), w.saturating_sub(16)),
                if on { s.title() } else { s.primary() },
            ),
            Span::styled("  not confirmed", warning(s)),
        ]));
    }
    if data.snippets.len() > LIST_ROWS {
        lines.push(Line::from(Span::styled(
            format!("  {} of {}", selected + 1, data.snippets.len()),
            s.muted(),
        )));
    }
    lines.push(Line::from(Span::styled("─".repeat(w), s.border())));
    let Some(sn) = data.snippets.get(selected) else {
        return lines;
    };
    lines.push(Line::from(Span::styled(
        display_safe(&sn.snippet.name),
        s.title(),
    )));
    if let Some(d) = &sn.snippet.description {
        lines.extend(paragraph(s.secondary(), &display_safe(d), w));
    }
    lines.push(Line::from(Span::styled(
        if sn.snippet.run {
            "Runs when sent: Portico types it, then ↵."
        } else {
            "Pasted only: Portico types it, you press ↵."
        },
        s.secondary(),
    )));
    if data.stale_snippets {
        lines.push(loading(s, tick, "Updating: it changed on another device…"));
    }
    lines.push(Line::from(""));
    // The whole command, as it will be typed: every line, wrapped by
    // characters, on a raised surface.
    let room = w.saturating_sub(2).max(8);
    for l in sn.snippet.command.split('\n') {
        for piece in chunks(&display_safe(l), room) {
            let pad = room.saturating_sub(piece.chars().count());
            lines.push(Line::from(vec![Span::styled(
                format!(" {piece}{} ", " ".repeat(pad)),
                s.key_hint(),
            )]));
        }
    }
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "y confirms this exact command on this device: it can run from now on. Any later change asks again.",
        w,
    ));
    lines
}

// ── wizard ───────────────────────────────────────────────────────────────

/// Widest wizard.
const WIZARD_MAX_WIDTH: u16 = 84;

pub(crate) fn render_wizard(app: &App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let Some(wizard) = &app.sync_wizard else {
        return;
    };
    let screen = frame.area();
    let width = if screen.width <= FULL_WIDTH_AT {
        screen.width
    } else {
        screen.width.saturating_sub(2).min(WIZARD_MAX_WIDTH)
    };
    let height = screen.height.saturating_sub(2).min(30);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let (index, total) = step_index(wizard);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Set up sync ", s.title()),
            Span::styled(format!("· {index} of {total} "), s.muted()),
        ]))
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = usize::from(inner.width);
    let hints = wizard_hints(wizard);
    let mut foot = Vec::new();
    if let Some(e) = &wizard.error {
        foot.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
        foot.push(Line::from(""));
    }
    if !hints.is_empty() && app.dialogs.is_empty() {
        foot.extend(form::hints(s, &hints, true, w));
    }
    let foot_height = u16::try_from(foot.len()).unwrap_or(3);
    let [body, foot_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(foot_height)]).areas(inner);
    let lines = wizard_lines(wizard, s, app.tick, w);
    // A long step (the review, at 24 rows) keeps its end: the decision.
    let height = usize::from(body.height);
    let skip = match wizard.step {
        Step::Review => lines.len().saturating_sub(height),
        _ => 0,
    };
    let shown: Vec<Line<'static>> = lines.into_iter().skip(skip).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);
    frame.render_widget(Paragraph::new(foot), foot_area);
}

/// `2 of 4`: intro, folder, passphrase (and review), recovery.
fn step_index(wizard: &SyncWizard) -> (usize, usize) {
    let total = if wizard.joining() { 5 } else { 4 };
    let index = match wizard.step {
        Step::Intro => 1,
        Step::Folder => 2,
        Step::Create | Step::Join => 3,
        Step::Review => 4,
        Step::Recovery | Step::Working(_) | Step::Done(_) => total,
    };
    (index, total)
}

fn wizard_hints(wizard: &SyncWizard) -> Vec<(&'static str, &'static str)> {
    match &wizard.step {
        Step::Intro => vec![("↵", "continue"), ("Esc", "cancel")],
        Step::Folder if wizard.probing => vec![],
        Step::Folder => {
            let enter = match wizard.current_probe() {
                Some(Ok(FolderProbe::Usable)) => "create here",
                Some(Ok(FolderProbe::ExistingBundle { .. })) => "join",
                Some(_) => "check again",
                None => "check folder",
            };
            vec![("↵", enter), ("Tab", "complete"), ("Esc", "back")]
        }
        Step::Create | Step::Join if wizard.form.busy => vec![],
        Step::Create => {
            let mut hints = vec![("↵", "continue"), ("Tab", "next field")];
            if wizard.form.generated.is_some() && wizard.form.focused() == Some(PassField::New) {
                hints.extend([("c", "copy"), ("g", "another")]);
            } else {
                hints.push(("Ctrl-G", "generate"));
            }
            hints.push(("Esc", "back"));
            hints
        }
        Step::Join => vec![("↵", "review"), ("Tab", "next field"), ("Esc", "back")],
        Step::Review => vec![("↵", "continue"), ("j k", "choose"), ("Esc", "back")],
        Step::Recovery if wizard.understood => vec![
            (
                "↵",
                if wizard.preview.is_some() {
                    "join"
                } else {
                    "create"
                },
            ),
            ("Space", "I understand"),
            ("Esc", "back"),
        ],
        Step::Recovery => vec![("Space", "I understand"), ("Esc", "back")],
        Step::Working(_) => vec![],
        Step::Done(_) => vec![("↵", "open the Sync panel")],
    }
}

#[allow(clippy::too_many_lines)] // one block per step
fn wizard_lines(wizard: &SyncWizard, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let blank = Line::from("");
    match &wizard.step {
        Step::Intro => {
            lines.push(Line::from(Span::styled(
                "Keep your hosts in step across your devices",
                s.title(),
            )));
            lines.push(blank.clone());
            lines.extend(paragraph(s.secondary(), OFF_TEXT, w));
            lines.push(blank.clone());
            lines.extend(paragraph(s.secondary(), PRIVATE_TEXT, w));
            lines.push(blank.clone());
            lines.extend(paragraph(
                s.muted(),
                "Synced: hosts created in Portico, tags, groups, notes and favorites, snippets, and settings such as the theme and keys. A passphrase protects it; a device that joins needs it once.",
                w,
            ));
        }
        Step::Folder => {
            lines.push(Line::from(Span::styled(
                "Choose the sync folder",
                s.title(),
            )));
            lines.push(blank.clone());
            lines.extend(paragraph(
                s.secondary(),
                "A folder your cloud or sync app keeps in step, for example ~/Dropbox/Portico. Portico creates a portico-sync folder inside.",
                w,
            ));
            lines.push(blank.clone());
            lines.push(form::input(
                s,
                "Folder",
                LABEL,
                &wizard.folder,
                !wizard.probing,
                w.saturating_sub(LABEL),
            ));
            for c in wizard.candidates.iter().take(4) {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(LABEL)),
                    Span::styled(fmt::truncate_middle(c, w.saturating_sub(LABEL)), s.muted()),
                ]));
            }
            if wizard.candidates.len() > 4 {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(LABEL)),
                    Span::styled(format!("+{} more", wizard.candidates.len() - 4), s.muted()),
                ]));
            }
            lines.push(blank.clone());
            if wizard.probing {
                lines.push(loading(s, tick, "Looking at the folder…"));
            } else if let Some(probe) = wizard.current_probe() {
                lines.extend(probe_lines(probe, s, w));
            }
        }
        Step::Create => {
            lines.push(Line::from(Span::styled(
                "First device: choose a passphrase",
                s.title(),
            )));
            lines.extend(paragraph(
                s.secondary(),
                "Every device that joins needs it. Generate one, or type a long one: Portico refuses a weak passphrase.",
                w,
            ));
            lines.push(blank.clone());
            lines.extend(pass_fields(&wizard.form, s, tick, w));
        }
        Step::Join => {
            lines.push(Line::from(Span::styled("Join this sync", s.title())));
            lines.extend(paragraph(
                s.secondary(),
                "Type the passphrase chosen on the first device. Nothing is written before you have seen what joining brings.",
                w,
            ));
            lines.push(blank.clone());
            lines.extend(pass_fields(&wizard.form, s, tick, w));
        }
        Step::Review => {
            if let Some((preview, at)) = &wizard.preview {
                lines.push(Line::from(Span::styled("Joining brings", s.title())));
                let counts = [
                    (preview.hosts, "host", "hosts"),
                    (preview.host_meta, "host detail", "host details"),
                    (preview.snippets, "snippet", "snippets"),
                    (preview.settings, "setting", "settings"),
                ]
                .iter()
                .map(|(n, one, many)| plural(*n, one, many))
                .collect::<Vec<_>>()
                .join(" · ");
                lines.push(Line::from(Span::styled(counts, s.primary())));
                let devices = preview
                    .devices
                    .iter()
                    .map(|d| display_safe(&d.label))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.extend(paragraph(s.muted(), &format!("From {devices}"), w));
                if preview.conflicts > 0 {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{} with what this device has",
                            plural(preview.conflicts, "conflict", "conflicts")
                        ),
                        warning(s),
                    )));
                }
                if preview.needs_newer > 0 {
                    lines.extend(paragraph(
                        s.secondary(),
                        &format!(
                            "{} only a newer Portico reads: kept and passed on, not applied.",
                            plural(preview.needs_newer, "record", "records")
                        ),
                        w,
                    ));
                }
                lines.push(blank.clone());
                lines.push(Line::from(Span::styled("Conflicts", s.title())));
                for (i, (_, label, note)) in BULK.iter().enumerate() {
                    let on = i == wizard.bulk;
                    lines.push(Line::from(vec![
                        Span::styled(
                            if on { "▍● " } else { " ○ " },
                            if on { s.accent() } else { s.muted() },
                        ),
                        Span::styled(*label, if on { s.title() } else { s.secondary() }),
                    ]));
                    if on {
                        lines.extend(fmt::wrap(note, w.saturating_sub(3)).into_iter().map(|l| {
                            Line::from(vec![Span::raw("   "), Span::styled(l, s.muted())])
                        }));
                    }
                }
                lines.extend(paragraph(s.secondary(), BULK_NOTE, w));
                let left = JOIN_TTL_SECS
                    .saturating_sub(Instant::now().saturating_duration_since(*at).as_secs())
                    / 60;
                lines.push(Line::from(Span::styled(
                    format!("This review is valid for {} more min.", left.max(1)),
                    s.muted(),
                )));
            }
        }
        Step::Recovery => {
            lines.push(question(s, "Before you go on"));
            lines.push(blank.clone());
            lines.extend(paragraph(s.primary(), RECOVERY, w));
            lines.push(blank.clone());
            lines.extend(paragraph(
                s.secondary(),
                "Keep the passphrase in your password manager. A device that remembers the key can reset a lost passphrase.",
                w,
            ));
            lines.push(blank.clone());
            lines.push(form::toggle(
                s,
                "",
                0,
                wizard.understood,
                "I understand",
                true,
            ));
        }
        Step::Working(words) => {
            lines.push(loading(s, tick, words));
            lines.push(blank.clone());
            lines.extend(paragraph(
                s.muted(),
                "Deriving the key takes a few seconds on purpose: it is what makes guessing the passphrase slow.",
                w,
            ));
        }
        Step::Done(status) => {
            lines.push(Line::from(vec![
                Span::styled("✓ ", ok(s)),
                Span::styled("Sync is on for this device.", s.title()),
            ]));
            lines.push(blank.clone());
            let (label, tone) = status_label(status, OffsetDateTime::now_utc());
            lines.push(row(
                s,
                "Status",
                vec![
                    Span::styled(format!("{GLYPH} "), s.secondary()),
                    Span::styled(label, tone_label_style(s, tone)),
                ],
            ));
            lines.push(blank.clone());
            lines.extend(paragraph(
                s.secondary(),
                "Received hosts and snippets wait for your confirmation in the Sync panel (S on the host list). The status bar shows how sync is doing.",
                w,
            ));
        }
    }
    lines
}

fn probe_lines(
    probe: &Result<FolderProbe, portico_core::ErrorSummary>,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    let mark = |icon: &'static str, style: Style, text: &str, hint: Option<&str>| {
        let mut lines: Vec<Line<'static>> = fmt::wrap(text, w.saturating_sub(2))
            .into_iter()
            .enumerate()
            .map(|(i, l)| {
                Line::from(vec![
                    Span::styled(if i == 0 { icon } else { "  " }, style),
                    Span::styled(l, s.title()),
                ])
            })
            .collect();
        if let Some(hint) = hint {
            lines.extend(
                fmt::wrap(hint, w.saturating_sub(2))
                    .into_iter()
                    .map(|l| Line::from(vec![Span::raw("  "), Span::styled(l, s.secondary())])),
            );
        }
        lines
    };
    match probe {
        Ok(FolderProbe::Usable) => mark(
            "✓ ",
            ok(s),
            "No Portico sync here yet.",
            Some("↵ creates one: this is the first device."),
        ),
        Ok(FolderProbe::ExistingBundle { devices }) => mark(
            "✓ ",
            ok(s),
            &format!(
                "A Portico sync with {}.",
                plural(*devices, "device", "devices")
            ),
            Some("↵ joins it with its passphrase."),
        ),
        Ok(FolderProbe::Incomplete) => mark(
            "▲ ",
            warning(s),
            "portico-sync is here, but not bundle.toml yet.",
            Some("Your sync app may still be downloading it: wait, then ↵ to look again."),
        ),
        Ok(FolderProbe::Missing) => mark(
            "✕ ",
            error(s),
            "This folder does not exist.",
            Some("Create it with your sync app first: Portico never creates it."),
        ),
        Ok(FolderProbe::Refused { summary }) | Err(summary) => mark(
            "✕ ",
            error(s),
            &fmt::sentence(&summary.message),
            summary.hint.as_deref(),
        ),
        Ok(_) => mark("▲ ", warning(s), "Portico cannot use this folder.", None),
    }
}
