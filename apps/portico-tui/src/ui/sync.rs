//! The Sync panel, the "Set up sync" wizard, and the sync item of the
//! status bar (phase 11, S1c).
//!
//! - **Status item**: the monochrome glyph `⇅` and a label (`Synced 2 min
//!   ago`, `Syncing…`, `Waiting for laptop (3f9c0ae1)`, `Sync folder
//!   unavailable`, `Sync locked`, `Sync off`) in secondary text; conflicts,
//!   things to review and alerts are counted in status colours only.
//! - **Panel**: one tab row (counts in status colours), the tab's body,
//!   then the last result and the keys. Received hosts show the current
//!   and received destination side by side, every value whole (wrapped at
//!   `.` and `-`, never cut); received snippets show the whole command,
//!   scrollable, its line and character counts above it. The view records
//!   when the last line of the selected item was on screen (`seen_end`):
//!   only then is `y` accepted (A1). Values from other devices go through
//!   `display_safe`, or `display_cells` where a tab must stay visible (a
//!   muted `→`, A2): no control or bidi character reaches the terminal.
//! - **Wizard**: one step at a time, its keys under it; the step number
//!   has no total (it depends on the folder).
//!
//! - **S2c-1**: the rotation dialogs, the adoption and the rotation
//!   conflict ([`rotate`]), newer device files under the alert, old keys
//!   in this keychain (Devices), values from forgotten devices (Received),
//!   "Leave the device list" (Turn sync off), the re-join review.
//!
//! At 82 columns or less, the panel takes the full width. Only theme
//! tokens (`Styles`).

mod rotate;

use std::time::Instant;

use portico_core::sync::{
    AlertKind, ConflictSide, FolderProbe, PassphraseStrength, PendingHostChange, SyncHostValue,
    SyncStatus, SyncValue, WaitReason,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use secrecy::ExposeSecret;
use time::OffsetDateTime;

use crate::app::App;
use crate::form::{Cell, display_cells, display_safe};
use crate::keys::Load;
use crate::sync_panel::{
    Choice, GLYPH, PanelMode, PassField, PassForm, PassKind, ReceivedItem, SyncData, SyncPanel,
    SyncTab, TABS, Tone, alert_text, choices, folder_reason, key_entry_label, status_label,
    subject_label,
};
use crate::sync_wizard::{
    BULK, BULK_NOTE, JOIN_TTL_SECS, RECOVERY, REJOIN_NOTE, SAVED, SAVED_PROBLEM, Step, SyncWizard,
};
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
    received_label_ids(change, true)
}

/// [`received_label`], with or without the device ids (`changed by
/// laptop`: on a narrow screen, B15).
pub(crate) fn received_label_ids(change: &PendingHostChange, ids: bool) -> String {
    let devices = if change.devices.is_empty() {
        "another device".to_owned()
    } else if ids {
        change.devices.join(", ")
    } else {
        change
            .devices
            .iter()
            .map(|d| device_name(d))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let verb = if change.new { "new from" } else { "changed by" };
    display_safe(&format!("{verb} {devices}"))
}

/// `laptop (3f9c0ae1)` → `laptop` (the core's label: name, then the id
/// prefix in parentheses).
fn device_name(label: &str) -> &str {
    label
        .rsplit_once(" (")
        .filter(|(_, id)| {
            id.strip_suffix(')')
                .is_some_and(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map_or(label, |(name, _)| name)
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

#[allow(clippy::too_many_lines)] // layout, then body (marks `seen_end`), then keys
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

    let foot_lines = |panel: &SyncPanel| {
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
        foot
    };
    let foot_height = u16::try_from(foot_lines(panel).len()).unwrap_or(4);
    let [tabs_area, body_area, foot_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(foot_height),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(tab_row(panel, data, s, w)), tabs_area);

    let body_height = usize::from(body_area.height);
    let Body {
        lines,
        focus: focus_line,
        end,
    } = body_lines(panel, data, s, tick, w);
    // The selected item stays in view; long text scrolls (`PgDn`).
    let total = lines.len();
    let max_first = total.saturating_sub(body_height);
    panel.max_scroll = u16::try_from(max_first).unwrap_or(u16::MAX);
    let scroll = match &panel.mode {
        PanelMode::Rotate(flow) => flow.scroll,
        _ => panel.scroll,
    };
    let mut first = usize::from(scroll).min(max_first);
    if let Some(focus) = focus_line
        && (focus < first || focus >= first + body_height)
    {
        first = focus.saturating_sub(1).min(max_first);
    }
    // The last line of the received item was on screen: it can be decided.
    if let Some((end_line, hash)) = end
        && end_line >= first
        && end_line < first + body_height
    {
        panel.seen_end = Some(hash);
    }
    let shown: Vec<Line<'static>> = lines.into_iter().skip(first).take(body_height).collect();
    frame.render_widget(Paragraph::new(shown), body_area);
    // The keys after the body: `y` shows as soon as the end is on screen.
    frame.render_widget(Paragraph::new(foot_lines(panel)), foot_area);
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

/// `Overview  Conflicts 2  Received 3  Issues  Devices`, then a rule.
fn tab_row(panel: &SyncPanel, data: &SyncData, s: Styles, w: usize) -> Vec<Line<'static>> {
    let count = |tab: SyncTab| -> usize {
        match tab {
            SyncTab::Overview | SyncTab::Devices => 0,
            SyncTab::Conflicts => panel.conflicts.ready().map_or(data.conflicts, Vec::len),
            SyncTab::Issues => panel.issues.ready().map_or(0, Vec::len),
            SyncTab::Received => data.to_review(),
        }
    };
    let mut spans = Vec::new();
    for (i, tab) in TABS.iter().enumerate() {
        let title = tab.title();
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

/// The body of the tab (or of the open form).
struct Body {
    lines: Vec<Line<'static>>,
    /// The line to keep in view (the selected choice or issue).
    focus: Option<usize>,
    /// The last line of the selected received item, and its hash.
    end: Option<(usize, String)>,
}

impl Body {
    fn plain(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            focus: None,
            end: None,
        }
    }

    fn focused((lines, focus): (Vec<Line<'static>>, Option<usize>)) -> Self {
        Self {
            lines,
            focus,
            end: None,
        }
    }
}

fn body_lines(panel: &SyncPanel, data: &SyncData, s: Styles, tick: u64, w: usize) -> Body {
    match &panel.mode {
        PanelMode::Browse | PanelMode::Skip { .. } | PanelMode::Ask { .. } => {}
        PanelMode::Rotate(flow) => return Body::focused(rotate::rotate_lines(flow, s, tick, w)),
        PanelMode::Adopt(f) => {
            return form_body(f, rotate::adopt_lines(f, data.status(), s, tick, w));
        }
        PanelMode::UseOther(f) => return form_body(f, rotate::use_other_lines(f, s, tick, w)),
        PanelMode::Unlock(f) => return Body::plain(unlock_lines(f, s, tick, w)),
        PanelMode::Change(f) => return Body::plain(change_lines(f, s, tick, w)),
        PanelMode::Reset { form, confirm } => {
            return Body::plain(reset_lines(form, confirm.is_some(), s, tick, w));
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
            return Body::plain(lines);
        }
        PanelMode::Disable {
            options,
            focus,
            join_again,
        } => {
            return Body::plain(disable_lines(*options, *focus, *join_again, s, w));
        }
    }
    let (mut lines, focus) = match panel.tab {
        SyncTab::Overview => (overview_lines(panel, data, s, tick, w), None),
        SyncTab::Conflicts => return Body::focused(conflict_lines(panel, s, tick, w)),
        SyncTab::Received => return received_lines(panel, data, s, tick, w),
        SyncTab::Devices => device_lines(panel, data, s, tick, w),
        // The question of "Skip on this device" follows the issues.
        SyncTab::Issues => issue_lines(panel, s, tick, w),
    };
    if let PanelMode::Skip { label, .. } = &panel.mode {
        lines.push(Line::from(""));
        let asked = lines.len();
        lines.push(question(s, &format!("Skip {label} on this device?")));
        lines.extend(paragraph(
            s.secondary(),
            "It stays as it is here and is no longer applied on this device. The other devices keep it.",
            w,
        ));
        // The question stays in view.
        return Body::focused((lines, Some(asked)));
    }
    if let PanelMode::Ask { ask, .. } = &panel.mode {
        lines.extend(rotate::ask_lines(ask, s, w));
        let end = lines.len().saturating_sub(1);
        return Body::focused((lines, Some(end)));
    }
    Body::focused((lines, focus))
}

/// A form's body: on its last field, its end stays in view (80×24).
fn form_body(f: &PassForm, lines: Vec<Line<'static>>) -> Body {
    let last = f.fields().len().saturating_sub(1);
    let focus = (f.focus == last).then(|| lines.len().saturating_sub(1));
    Body::focused((lines, focus))
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
                        "{} changed by sync still wait for your review (Received): they do not run before.",
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
    // The devices, counted (listed in the Devices tab).
    if let Some(devices) = panel.devices.ready() {
        let inactive = devices.iter().filter(|d| d.stale).count();
        let mut value = vec![Span::styled(
            plural(devices.len(), "device", "devices"),
            s.secondary(),
        )];
        if inactive > 0 {
            value.push(Span::styled(format!(" · {inactive} inactive"), s.muted()));
        }
        lines.push(row(s, "Devices", value));
    }
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
        SyncStatus::Waiting {
            reason: WaitReason::NeedsNewKeys,
            device,
        } => Some((
            format!(
                "{} still uses the previous keys.",
                device
                    .as_deref()
                    .map_or_else(|| "A device".to_owned(), display_safe)
            ),
            "Type the new passphrase on that device. Its changes wait meanwhile; nothing of it is merged.",
            warning(s),
        )),
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            ..
        } => Some((
            "Finishing the key rotation…".to_owned(),
            "The new keys are saved on this device: Portico writes them to the folder as soon as it answers. If the key file was replaced meanwhile, b restores it.",
            warning(s),
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
    if let SyncStatus::Alert {
        kind: AlertKind::KeysRotatedElsewhere,
        ..
    } = &overview.status
    {
        lines.extend(rotate::newer_lines(
            panel.newer_files(),
            panel.newer_index,
            s,
            w,
        ));
    }
    if let SyncStatus::Alert {
        kind: AlertKind::RevokedWrites,
        ..
    } = &overview.status
        && !data.held.is_empty()
    {
        lines.push(Line::from(""));
        lines.extend(paragraph(
            warning(s),
            &format!(
                "{} held on this device: decide each in Received.",
                plural(data.held.len(), "value", "values")
            ),
            w,
        ));
    }
    lines
}

/// The Devices tab: every device of the sync, this one marked, then the
/// old keys in this keychain; the selected row is marked.
#[allow(clippy::too_many_lines)] // devices, then old keys
fn device_lines(
    panel: &SyncPanel,
    data: &SyncData,
    s: Styles,
    tick: u64,
    w: usize,
) -> (Vec<Line<'static>>, Option<usize>) {
    let mut lines = Vec::new();
    let mut focus = None;
    let locked = matches!(data.status(), Some(SyncStatus::Locked));
    let rows = panel.device_rows();
    let selected = panel.device.min(rows.len().saturating_sub(1));
    let browsing = matches!(panel.mode, PanelMode::Browse);
    match &panel.devices {
        _ if matches!(data.status(), Some(SyncStatus::Disabled)) => {
            lines.push(Line::from(Span::styled(
                "Sync is off on this device.",
                s.secondary(),
            )));
            lines.extend(paragraph(
                s.muted(),
                "Once sync is on, every device that shares the folder shows here, with when it last published.",
                w,
            ));
            return (lines, None);
        }
        Load::Loading => lines.push(loading(s, tick, "Reading the devices…")),
        Load::Failed(e) if !locked => {
            lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
        }
        Load::Failed(_) => {
            lines.extend(paragraph(
                s.secondary(),
                "Unlock sync to see the devices.",
                w,
            ));
        }
        Load::Ready(devices) => {
            let label_w = devices
                .iter()
                .map(|d| display_safe(&d.label).chars().count())
                .max()
                .unwrap_or(0)
                .min(w.saturating_sub(32).max(12));
            for (i, d) in devices.iter().enumerate() {
                let on = browsing && i == selected;
                if on {
                    focus = Some(lines.len());
                }
                let label = fmt::truncate(&display_safe(&d.label), label_w);
                // The accent marks this device's dot only; words stay text.
                let mut spans = vec![
                    Span::styled(if on { "▍" } else { " " }, s.accent()),
                    Span::styled(
                        if d.this_device { "● " } else { "○ " },
                        if d.this_device { s.accent() } else { s.muted() },
                    ),
                    Span::styled(
                        format!("{label:<label_w$}"),
                        if on { s.title() } else { s.primary() },
                    ),
                    Span::raw("  "),
                ];
                if d.this_device {
                    spans.push(Span::styled("this device", s.secondary()));
                } else {
                    spans.push(Span::styled(
                        format!("published {}", ago(d.last_seen.or(d.published_at))),
                        s.muted(),
                    ));
                }
                if d.stale {
                    spans.push(Span::styled(" · inactive", s.muted()));
                }
                if d.revoked {
                    spans.push(Span::styled(" · removed", s.muted()));
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.extend(paragraph(
                s.muted(),
                "A device is named with its id: a copy can never pass for another. Inactive: nothing published for a long time. Forget a lost device (f): the keys change, and it can no longer read what is synced next.",
                w,
            ));
        }
    }
    // Old keys in this keychain (§13.3.8): never the key in use.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Old keys in this keychain",
        s.title(),
    )));
    match &panel.keys {
        Load::Loading => lines.push(loading(s, tick, "Reading the keychain…")),
        Load::Failed(e) => lines.extend(paragraph(s.muted(), &fmt::sentence(&e.message), w)),
        Load::Ready(keys) if keys.is_empty() => {
            lines.extend(paragraph(s.muted(), "None.", w));
        }
        Load::Ready(keys) => {
            let devices = panel.devices.ready().map_or(0, Vec::len);
            for (i, entry) in keys.iter().enumerate() {
                let on = browsing && devices + i == selected;
                if on {
                    focus = Some(lines.len());
                }
                lines.push(Line::from(vec![
                    Span::styled(if on { "▍" } else { " " }, s.accent()),
                    Span::styled("  ", s.muted()),
                    Span::styled(
                        key_entry_label(entry),
                        if on { s.title() } else { s.primary() },
                    ),
                    Span::styled("  not in use", s.muted()),
                ]));
            }
        }
    }
    lines.extend(paragraph(
        s.muted(),
        "The key in use is not listed: Turn sync off forgets it. An old key opens only what was synced before the keys changed.",
        w,
    ));
    (lines, focus)
}

fn strength_line(strength: Option<PassphraseStrength>, s: Styles) -> Line<'static> {
    let Some(st) = strength else {
        return Line::from(vec![
            Span::styled(format!("{:<LABEL$} ", "Strength"), s.muted()),
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
        Span::styled(format!("{:<LABEL$} ", "Strength"), s.muted()),
        Span::styled("▰".repeat(filled), style),
        Span::styled("▱".repeat(METER_CELLS.saturating_sub(filled)), s.muted()),
        Span::styled(format!("  {} bits · ", st.estimated_bits), s.secondary()),
        Span::styled(words, style),
    ])
}

/// The recovery warning in a box, then "I saved it…" (Q1): before the
/// step that writes, in both create (a field of the form) and join (the
/// review).
fn saved_box(s: Styles, saved: bool, focused: bool, w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, l) in fmt::wrap(RECOVERY, w.saturating_sub(4))
        .into_iter()
        .enumerate()
    {
        lines.push(Line::from(vec![
            Span::styled(if i == 0 { "▲ " } else { "  " }, warning(s)),
            Span::styled(l, s.primary()),
        ]));
    }
    lines.push(Line::from(""));
    lines.extend(saved_check(s, saved, focused, w));
    lines
}

/// `□ I saved it. A lost passphrase cannot be recovered…`, wrapped.
fn saved_check(s: Styles, saved: bool, focused: bool, w: usize) -> Vec<Line<'static>> {
    let mark = if saved { "■" } else { "□" };
    let style = if focused { s.accent() } else { s.secondary() };
    fmt::wrap(SAVED, w.saturating_sub(4))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            Line::from(vec![
                Span::styled(
                    if i == 0 {
                        format!("{mark} ")
                    } else {
                        "  ".to_owned()
                    },
                    style,
                ),
                Span::styled(l, style),
            ])
        })
        .collect()
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
                    PassKind::Change | PassKind::Authorize => "Current",
                    PassKind::Adopt { .. } => "New",
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
                    for l in fmt::wrap(&words, w.saturating_sub(LABEL + 1)) {
                        let lead = if first { label } else { "" };
                        first = false;
                        lines.push(Line::from(vec![
                            Span::styled(format!("{lead:<LABEL$} "), label_style),
                            Span::styled(l, s.title()),
                        ]));
                    }
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(LABEL + 1)),
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
                    lines.push(form::toggle(s, "", LABEL, f.saved, "I wrote it down", on));
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
                // Under the toggle's text (after `■ `).
                let note = "Off on a shared computer: sync then asks for the passphrase after each restart.";
                for l in fmt::wrap(note, w.saturating_sub(LABEL + 3).max(10)) {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(LABEL + 3)),
                        Span::styled(l, s.muted()),
                    ]));
                }
            }
            PassField::Previous => {
                lines.push(form::secret(
                    s,
                    "Previous",
                    LABEL,
                    !f.previous.is_empty(),
                    on,
                    input_w,
                ));
                push_problem(&mut lines, *field);
            }
            PassField::DontKnow => {
                lines.push(form::toggle(
                    s,
                    "",
                    LABEL,
                    f.dont_know,
                    "I no longer know it",
                    on,
                ));
                let note =
                    "This device's key authorizes the rotation instead, after a confirmation.";
                for l in fmt::wrap(note, w.saturating_sub(LABEL + 3).max(10)) {
                    lines.push(Line::from(vec![
                        Span::raw(" ".repeat(LABEL + 3)),
                        Span::styled(l, s.muted()),
                    ]));
                }
            }
            PassField::Saved => {
                lines.push(Line::from(""));
                lines.extend(saved_box(s, f.saved, on, w));
                // Under the box's text, not in the label column.
                if let Some((message, _)) = problem_of(*field) {
                    lines.extend(form::problem(s, 2, &message, w));
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

/// "Leave the device list" (R18), word for word as in the GUI.
pub(crate) const LEAVE_NOTE: &str = "Leaves the list without changing the keys: unless its key is forgotten too, this device can still read the current sync until the next rotation. To really exclude a device, forget it from another device.";

fn disable_lines(
    options: portico_core::sync::DisableOptions,
    focus: usize,
    join_again: bool,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    let title = if join_again {
        "Turn sync off, then join again"
    } else {
        "Turn sync off on this device"
    };
    let mut lines = vec![Line::from(Span::styled(title, s.title())), Line::from("")];
    lines.extend(paragraph(
        s.secondary(),
        if join_again {
            "This device was removed from sync. Its local hosts, snippets and settings stay. Once sync is off, the wizard joins the folder again as a new device, with the current passphrase."
        } else {
            "Your local hosts, snippets and settings stay on this device. The other devices keep syncing."
        },
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
    if !join_again {
        lines.push(form::toggle(
            s,
            "",
            2,
            options.leave_roster,
            "Leave the device list",
            focus == 2,
        ));
        for l in fmt::wrap(LEAVE_NOTE, w.saturating_sub(5).max(10)) {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(l, s.muted()),
            ]));
        }
    }
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
        // The command keeps its tabs: `field_lines` shows them (A2).
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
/// wrapped by characters, tabs shown as a muted `→`: nothing is cut or
/// hidden.
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
        for row in cell_rows(text, room) {
            let lead = if first { *label } else { "" };
            first = false;
            let mut spans = vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(format!("{lead:<LABEL$}"), s.muted()),
            ];
            spans.extend(cell_spans(&row, style, s.muted()));
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// `text` (every line of it) as rows of at most `width` cells, tabs kept
/// (at least one row).
fn cell_rows(text: &str, width: usize) -> Vec<Vec<Cell>> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let cells = display_cells(line);
        if cells.is_empty() {
            rows.push(Vec::new());
        }
        rows.extend(cells.chunks(width.max(1)).map(<[Cell]>::to_vec));
    }
    rows
}

/// A row of cells as spans: text in `style`, each tab a `→` in `tab`.
fn cell_spans(row: &[Cell], style: Style, tab: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut text = String::new();
    for cell in row {
        match cell {
            Cell::Char(c) => text.push(*c),
            Cell::Tab => {
                if !text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut text), style));
                }
                spans.push(Span::styled("→", tab));
            }
        }
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, style));
    }
    spans
}

/// `text` cut into pieces of `width` characters (at least one piece),
/// after a `.` or a `-` when one falls in the second half of a piece, so a
/// host name wraps where it reads (B15).
fn chunks(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start.saturating_add(width).min(chars.len());
        if end < chars.len()
            && let Some(cut) = chars
                .get(start..end)
                .and_then(|piece| piece.iter().rposition(|c| matches!(c, '.' | '-')))
                .filter(|cut| *cut + 1 > width / 2)
        {
            end = start + cut + 1;
        }
        out.push(chars.get(start..end).unwrap_or_default().iter().collect());
        start = end;
    }
    out
}

/// `On this device`, or the devices that wrote the side.
fn side_who(side: &ConflictSide) -> String {
    if side.local {
        "On this device".to_owned()
    } else if side.devices.is_empty() {
        "Another device".to_owned()
    } else {
        display_safe(&side.devices.join(", "))
    }
}

/// `this device's` / `desk (77a1b2c3)'s` (as in "Keep …").
fn side_owner(side: &ConflictSide) -> String {
    if side.local {
        "this device's".to_owned()
    } else {
        format!("{}'s", side_who(side))
    }
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
            Choice::Union => "Union of all tags".to_owned(),
            Choice::KeepBoth { .. } => "Keep both".to_owned(),
            Choice::Pick { index } => conflict
                .sides
                .iter()
                .find(|side| side.index == *index)
                .map_or_else(
                    || "Keep a side".to_owned(),
                    |side| {
                        let deleted = if matches!(side.value, SyncValue::Deleted) {
                            " (delete it)"
                        } else {
                            ""
                        };
                        format!(
                            "Keep {}{deleted} · {}",
                            side_owner(side),
                            ago(Some(side.at))
                        )
                    },
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
                    .map_or_else(|| "one side's".to_owned(), side_owner);
                // Never capitalised: a device name stays as written.
                let keeper = keeper.replacen("this device's", "This device's", 1);
                lines.extend(paragraph(
                    s.muted(),
                    &format!(
                        "   {keeper} version keeps the id; every other version becomes a copy. Copies from other devices wait for your review before they run."
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

// ── received: hosts, then snippets ───────────────────────────────────────

/// The Received tab: one list (hosts waiting for this device's
/// confirmation, F1, then snippets, F8), then the selected item's card.
/// The card's last value line is `end`: the item is decided only once it
/// has been on screen (A1).
fn received_lines(panel: &SyncPanel, data: &SyncData, s: Styles, tick: u64, w: usize) -> Body {
    if data.to_review() == 0 {
        let mut lines = if data.stale() {
            vec![loading(s, tick, "Reading what was received…")]
        } else {
            vec![Line::from(Span::styled(
                "Nothing received waits for you.",
                s.secondary(),
            ))]
        };
        lines.extend(paragraph(
            s.muted(),
            "When another device adds a host or changes where one connects, it waits here: nothing is written to hosts.conf before you confirm it on this device. A snippet added or changed elsewhere does not run here before you read its whole command and confirm it.",
            w,
        ));
        return Body::plain(lines);
    }
    let total = data.to_review();
    let selected = panel.received.min(total.saturating_sub(1));
    let mut lines = Vec::new();
    // The list (a window around the selection), hosts then snippets.
    let first = selected.saturating_sub(LIST_ROWS.saturating_sub(1));
    for i in (first..total).take(LIST_ROWS) {
        let on = i == selected;
        let snippet_index = i.saturating_sub(data.pending.len());
        let (name, label) = match data.pending.get(i) {
            Some(change) => (display_safe(&change.alias), received_label(change)),
            None => match data.snippets.get(snippet_index) {
                Some(sn) => (
                    display_safe(&sn.snippet.name),
                    "snippet · not confirmed".to_owned(),
                ),
                None => match data
                    .held
                    .get(snippet_index.saturating_sub(data.snippets.len()))
                {
                    Some(held) => (
                        display_safe(&subject_label(&held.subject)),
                        held.device.as_deref().map_or_else(
                            || "held · unknown origin".to_owned(),
                            |d| format!("held · from {}", display_safe(d)),
                        ),
                    ),
                    None => continue,
                },
            },
        };
        let name_w = name.chars().count().min(w / 2);
        lines.push(Line::from(vec![
            Span::styled(if on { "▍" } else { " " }, s.accent()),
            Span::styled(
                format!("{}  ", fmt::truncate(&name, name_w)),
                if on { s.title() } else { s.primary() },
            ),
            Span::styled(
                fmt::truncate(&label, w.saturating_sub(name_w + 4)),
                warning(s),
            ),
        ]));
    }
    if total > LIST_ROWS {
        lines.push(Line::from(Span::styled(
            format!("  {} of {total}", selected + 1),
            s.muted(),
        )));
    }
    lines.push(Line::from(Span::styled("─".repeat(w), s.border())));
    let Some(item) = panel.selected_received(data) else {
        return Body::plain(lines);
    };
    let end = match item {
        ReceivedItem::Host(change) => host_card(change, data.stale_hosts, &mut lines, s, tick, w),
        ReceivedItem::Snippet(sn) => snippet_card(sn, data.stale_snippets, &mut lines, s, tick, w),
        ReceivedItem::Held(held) => held_card(held, data.stale_held, &mut lines, s, tick, w),
    };
    Body {
        lines,
        focus: None,
        end: Some((end, item.hash().to_owned())),
    }
}

/// The card of a received host: current vs received destination, every
/// value whole. Returns the index of its last value line.
fn host_card(
    change: &PendingHostChange,
    stale: bool,
    lines: &mut Vec<Line<'static>>,
    s: Styles,
    tick: u64,
    w: usize,
) -> usize {
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
    if stale {
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
            format!("{:<col$}  ", "On this device"),
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
            // The change mark sits on the label's line.
            let mark = r == 0 && differs && current.is_some();
            let mut spans = vec![Span::styled(
                format!("{:<LABEL$}", if r == 0 { *label } else { "" }),
                s.muted(),
            )];
            if current.is_some() {
                let piece = left.get(r).cloned().unwrap_or_default();
                spans.push(Span::styled(format!("{piece:<col$}  "), s.secondary()));
            }
            let piece = right.get(r).cloned().unwrap_or_default();
            let pad = col.saturating_sub(piece.chars().count());
            spans.push(Span::styled(
                piece,
                if differs { s.title() } else { s.muted() },
            ));
            if mark {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(" ◂", warning(s)));
            }
            lines.push(Line::from(spans));
        }
    }
    let end = lines.len().saturating_sub(1);
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "y writes this destination to hosts.conf on this device. n keeps it out here; the other devices keep it.",
        w,
    ));
    end
}

/// The card of a value from a forgotten device (or of unknown origin):
/// two explicit choices, nothing preselected, one value at a time (R3,
/// RR4). Returns the index of its last line before the keys.
fn held_card(
    held: &portico_core::sync::api::RevokedValueItem,
    stale: bool,
    lines: &mut Vec<Line<'static>>,
    s: Styles,
    tick: u64,
    w: usize,
) -> usize {
    lines.push(Line::from(vec![Span::styled(
        "From forgotten devices",
        s.muted(),
    )]));
    lines.push(Line::from(Span::styled(
        display_safe(&subject_label(&held.subject)),
        s.title(),
    )));
    let who = match &held.device {
        Some(device) => format!(
            "Written on this device by changes from {}, a removed device.",
            display_safe(device)
        ),
        None => "Of unknown origin: this device joined again without its previous keys.".to_owned(),
    };
    lines.extend(paragraph(s.secondary(), &who, w));
    if stale {
        lines.push(loading(s, tick, "Updating: it changed on another device…"));
    }
    lines.extend(paragraph(
        s.secondary(),
        "It is held out of sync on this device until you choose. Nothing is chosen for you.",
        w,
    ));
    let end = lines.len().saturating_sub(1);
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "m keeps it as this device's value and shares it. t takes the value the other devices hold, or removes it here if they hold none.",
        w,
    ));
    end
}

/// The card of a received snippet: the whole command as it will be typed
/// (every line, wrapped by characters, tabs shown), its counts above it.
/// Returns the index of its last command line.
fn snippet_card(
    sn: &portico_core::sync::UnconfirmedSnippet,
    stale: bool,
    lines: &mut Vec<Line<'static>>,
    s: Styles,
    tick: u64,
    w: usize,
) -> usize {
    lines.push(Line::from(Span::styled(
        display_safe(&sn.snippet.name),
        s.title(),
    )));
    // B14: who wrote it (the device ids as the core labels them).
    let who = if sn.devices.is_empty() {
        "another device".to_owned()
    } else {
        display_safe(&sn.devices.join(", "))
    };
    lines.extend(paragraph(
        s.secondary(),
        &format!("Changed by {who}. It runs only once you confirm this exact command here."),
        w,
    ));
    if let Some(d) = &sn.snippet.description {
        lines.extend(paragraph(s.secondary(), &display_safe(d), w));
    }
    if sn.snippet.run {
        lines.push(Line::from(vec![
            Span::styled("▲ ", warning(s)),
            Span::styled("Runs as soon as it is sent (Enter is added).", s.primary()),
        ]));
    } else {
        lines.push(Line::from(Span::styled(
            "Pasted only: Portico types it, you press ↵.",
            s.secondary(),
        )));
    }
    if stale {
        lines.push(loading(s, tick, "Updating: it changed on another device…"));
    }
    lines.push(Line::from(""));
    let command = &sn.snippet.command;
    let count = |n: usize, one: &str, many: &str| plural(n, one, many);
    lines.push(Line::from(Span::styled(
        format!(
            "{} · {}",
            count(command.split('\n').count(), "line", "lines"),
            count(command.chars().count(), "character", "characters")
        ),
        s.muted(),
    )));
    // On a raised surface; a tab is a muted `→`.
    let room = w.saturating_sub(2).max(8);
    for row in cell_rows(command, room) {
        let pad = room.saturating_sub(row.len());
        let mut spans = vec![Span::styled(" ", s.key_hint())];
        spans.extend(cell_spans(
            &row,
            s.key_hint(),
            s.key_hint().patch(s.muted()),
        ));
        spans.push(Span::styled(format!("{} ", " ".repeat(pad)), s.key_hint()));
        lines.push(Line::from(spans));
    }
    let end = lines.len().saturating_sub(1);
    lines.push(Line::from(""));
    lines.extend(paragraph(
        s.muted(),
        "y confirms this exact command on this device: it can run from now on. Any later change asks again.",
        w,
    ));
    end
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
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Set up sync ", s.title()),
            Span::styled(format!("· step {} ", step_index(wizard)), s.muted()),
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
    // A long step (the review, at 24 rows) keeps its end: the decision;
    // so does the passphrase step once "I saved it…" (its last field) has
    // the focus.
    let height = usize::from(body.height);
    let keep_end = match wizard.step {
        Step::Review => true,
        Step::Create => wizard.form.focused() == Some(PassField::Saved),
        _ => false,
    };
    let skip = if keep_end {
        lines.len().saturating_sub(height)
    } else {
        0
    };
    let total = lines.len();
    let shown: Vec<Line<'static>> = lines.into_iter().skip(skip).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);
    frame.render_widget(Paragraph::new(foot), foot_area);
    // More below (the recovery box under the fields at 80×24): say so.
    if skip + height < total {
        let more = format!(" ↓ {} more ", total - skip - height);
        let x = body
            .right()
            .saturating_sub(u16::try_from(more.chars().count()).unwrap_or(0));
        frame.render_widget(
            Paragraph::new(Span::styled(more, s.muted())),
            Rect {
                x,
                y: body.bottom().saturating_sub(1),
                width: body.right().saturating_sub(x),
                height: 1,
            },
        );
    }
}

/// The step's number: intro, folder, passphrase, (join) review. No total:
/// it depends on what the folder holds (B15).
fn step_index(wizard: &SyncWizard) -> usize {
    match wizard.step {
        Step::Intro => 1,
        Step::Folder => 2,
        Step::Review => 4,
        Step::Working(_) if wizard.preview.is_some() => 4,
        Step::Create | Step::Join | Step::Working(_) => 3,
    }
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
            let mut hints = vec![("↵", "create"), ("Tab", "next field")];
            match wizard.form.focused() {
                Some(PassField::New) if wizard.form.generated.is_some() => {
                    hints.extend([("c", "copy"), ("g", "another")]);
                }
                Some(PassField::Saved) => hints.push(("Space", "I saved it")),
                _ => hints.push(("Ctrl-G", "generate")),
            }
            hints.push(("Esc", "back"));
            hints
        }
        Step::Join => vec![("↵", "review"), ("Tab", "next field"), ("Esc", "back")],
        Step::Review if wizard.rejoin() => {
            vec![("↵", "join"), ("Space", "I saved it"), ("Esc", "back")]
        }
        Step::Review => vec![
            ("↵", "join"),
            ("Space", "I saved it"),
            ("j k", "choose"),
            ("Esc", "back"),
        ],
        Step::Working(_) => vec![],
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
            if let Some(note) = &wizard.note {
                lines.extend(paragraph(warning(s), note, w));
                lines.push(blank.clone());
            }
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
                    plural(preview.hosts, "host", "hosts"),
                    format!("{} tags, groups, notes", preview.host_meta),
                    plural(preview.snippets, "snippet", "snippets"),
                    plural(preview.settings, "setting", "settings"),
                ]
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
                let rejoin = wizard.rejoin();
                for (i, (_, label, note)) in BULK.iter().enumerate() {
                    let on = i == wizard.bulk;
                    let offered = !rejoin || i == 0;
                    let mut row = vec![
                        Span::styled(
                            if on { "▍● " } else { " ○ " },
                            if on { s.accent() } else { s.muted() },
                        ),
                        Span::styled(
                            *label,
                            if on {
                                s.title()
                            } else if offered {
                                s.secondary()
                            } else {
                                s.muted()
                            },
                        ),
                    ];
                    if !offered {
                        row.push(Span::styled("  not offered", s.muted()));
                    }
                    lines.push(Line::from(row));
                    if on && rejoin {
                        lines.extend(fmt::wrap(REJOIN_NOTE, w.saturating_sub(3)).into_iter().map(
                            |l| Line::from(vec![Span::raw("   "), Span::styled(l, s.secondary())]),
                        ));
                    } else if on {
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
                // Before the step that writes: the recovery box (Q1). Its
                // words carry the warning (the full text is on the first
                // device's passphrase step): the review fits 80×24.
                lines.push(blank.clone());
                lines.extend(saved_check(s, wizard.saved, true, w));
                if wizard.saved_problem {
                    lines.extend(form::problem(s, 2, SAVED_PROBLEM, w));
                }
            }
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
