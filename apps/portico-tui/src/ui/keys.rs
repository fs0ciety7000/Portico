//! Keys overlay: local key pairs, then `ssh-agent` identities (two-line
//! rows: name, type and badges; fingerprint and comment), the vault status
//! and the keys that work. Sub-modes (generation form, lifetime choice,
//! removal confirmation, host picker, copy-id progress) replace the list.

use portico_core::AgentKey;
use portico_core::keys::LocalKey;
use portico_core::vault::VaultStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph};

use crate::app::App;
use crate::copy_job::{CopyJob, Step};
use crate::keys::{
    CertBadge, GenField, GenForm, HostPicker, KeysPanel, LIFETIMES, Load, Mode, Notice, Section,
    algorithm_label, cert_badge, file_name, short_fingerprint,
};
use crate::theme::Styles;
use crate::ui::overlays::ToastBox;
use crate::ui::{fmt, form, spinner, status};

/// What the panel needs from the app besides itself.
struct Context {
    s: Styles,
    tick: u64,
    vault: Option<VaultStatus>,
    job: Option<CopyJob>,
    /// Status label of the copy job's session (`Connecting · bastion (1/2)`).
    job_status: Option<String>,
    hints: Vec<(&'static str, &'static str)>,
    /// A dialog is over the panel: its keys do not work, no hints.
    dialog_open: bool,
}

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let job = app.copy_job.clone();
    let job_status = job
        .as_ref()
        .map(|j| status::status(app.session_by_id(j.session), s).label);
    let copy = app.copy_state();
    let vault = app.vault;
    let tick = app.tick;
    let dialog_open = !app.dialogs.is_empty();
    let Some(panel) = &mut app.keys_panel else {
        return;
    };
    let ctx = Context {
        s,
        tick,
        vault,
        job,
        job_status,
        hints: if dialog_open {
            Vec::new()
        } else {
            panel.hints(vault, copy)
        },
        dialog_open,
    };
    draw(panel, &ctx, frame, band);
}

/// The framed panel, its bottom clear of the toast `band`.
fn frame_panel(panel: &KeysPanel, ctx: &Context, frame: &mut Frame<'_>, band: u16) -> Rect {
    let s = ctx.s;
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(100);
    let height = screen.height.saturating_sub(2).min(32);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let title = match &panel.mode {
        Mode::Generate(_) => vec![
            Span::styled(" Keys ", s.title()),
            Span::styled("· new key pair ", s.muted()),
        ],
        Mode::PickHost(p) => vec![Span::styled(
            format!(
                " Copy {} to… ",
                fmt::truncate(&file_name(&p.key.private_path), 48)
            ),
            s.title(),
        )],
        Mode::Copy => vec![Span::styled(
            ctx.job.as_ref().map_or_else(
                || " Copy ".to_owned(),
                |j| {
                    format!(
                        " Copy {} to {} ",
                        fmt::truncate(&j.key_name, 40),
                        fmt::truncate(&j.alias, 32)
                    )
                },
            ),
            s.title(),
        )],
        _ => vec![
            Span::styled(" Keys ", s.title()),
            Span::styled("· ~/.ssh and ssh-agent ", s.muted()),
        ],
    };
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(title))
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    // `Esc close` only where Esc closes (elsewhere it goes back).
    if matches!(panel.mode, Mode::Browse) && !ctx.dialog_open {
        block =
            block.title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned());
    }
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn draw(panel: &mut KeysPanel, ctx: &Context, frame: &mut Frame<'_>, band: u16) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(100);
    // Borders and horizontal padding.
    let w = usize::from(width.saturating_sub(6));

    if matches!(
        panel.mode,
        Mode::Browse | Mode::Lifetime { .. } | Mode::ConfirmRemove { .. }
    ) {
        browse(panel, ctx, frame, band, w);
        return;
    }
    let inner = frame_panel(panel, ctx, frame, band);
    let w = usize::from(inner.width);
    match &mut panel.mode {
        Mode::Generate(gen_form) => {
            let lines = generate_lines(gen_form, ctx, w);
            render_with_footer(
                frame,
                inner,
                Paragraph::new(lines),
                &footer(ctx, None, w, true),
            );
        }
        Mode::PickHost(picker) => render_picker(frame, inner, picker, ctx),
        Mode::Copy => {
            let lines = copy_lines(ctx, w);
            let failed = ctx
                .job
                .as_ref()
                .is_some_and(|j| matches!(j.step, Step::Failed(_)));
            render_with_footer(
                frame,
                inner,
                Paragraph::new(lines),
                &footer(ctx, None, w, failed),
            );
        }
        _ => {}
    }
}

/// The lists, with the footer under them: the inline question, the
/// notice (when it fits without squeezing the list, else as a toast under
/// the panel), the public key line, the vault and the hints.
fn browse(panel: &mut KeysPanel, ctx: &Context, frame: &mut Frame<'_>, band: u16, w: usize) {
    let s = ctx.s;
    let screen = frame.area();
    let notice = panel.notice.clone();
    let footer_with = |panel: &KeysPanel, notice: Option<&Notice>| {
        let mut extra = Vec::new();
        mode_lines(panel, s, w, &mut extra);
        if let Some(n) = notice {
            extra.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
        }
        if let Some(line) = &panel.public_line {
            extra.extend(
                // Cut at the edge, not at the space: the type and the key
                // stay one selectable run.
                line.chars()
                    .collect::<Vec<_>>()
                    .chunks(w.max(1))
                    .take(4)
                    .map(|c| Line::from(Span::styled(c.iter().collect::<String>(), s.title()))),
            );
        }
        extra.push(vault_line(ctx));
        let modal = !matches!(panel.mode, Mode::Browse);
        footer(ctx, Some(extra), w, modal)
    };
    let rows = list_rows(panel, ctx, w);
    // Rows inside the frame (borders and top padding), for a toast band.
    let inner_height = |band: u16| {
        let width = screen.width.saturating_sub(2).min(100);
        let height = screen.height.saturating_sub(2).min(32);
        usize::from(
            crate::ui::panel_rect(screen, width, height, band)
                .height
                .saturating_sub(3),
        )
    };
    let mut band = band;
    let mut overflow = None;
    let lines = match &notice {
        Some(n) => {
            let with = footer_with(panel, Some(n));
            if rows.height() + with.len() <= inner_height(band) {
                with
            } else {
                // The list keeps its room: the notice goes under the panel.
                let kind = if n.ok {
                    crate::app::ToastKind::Info
                } else {
                    crate::app::ToastKind::Error
                };
                let toast = ToastBox::new(s, kind, &n.message, n.hint.as_deref(), screen.width);
                band = band.max(toast.height);
                overflow = Some(toast);
                footer_with(panel, None)
            }
        }
        None => footer_with(panel, None),
    };
    let inner = frame_panel(panel, ctx, frame, band);
    let footer_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let [list_area, footer_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(inner);
    render_rows(panel, rows, s, frame, list_area);
    frame.render_widget(Paragraph::new(lines), footer_area);
    if let Some(toast) = overflow {
        let zone = Rect {
            y: screen.bottom().saturating_sub(1 + band),
            height: band,
            ..screen
        };
        toast.draw(s, frame, zone);
    }
}

/// `content` on top, `footer` lines at the bottom (the content gives way).
fn render_with_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    content: Paragraph<'_>,
    footer: &[Line<'static>],
) {
    let height = u16::try_from(footer.len()).unwrap_or(u16::MAX);
    let [top, bottom] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(area);
    frame.render_widget(content, top);
    frame.render_widget(Paragraph::new(footer.to_vec()), bottom);
}

/// A blank line, `extra`, then the key hints (`Esc close` sits in the
/// border).
fn footer(
    ctx: &Context,
    extra: Option<Vec<Line<'static>>>,
    width: usize,
    modal: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    lines.extend(extra.unwrap_or_default());
    let hints: Vec<_> = ctx
        .hints
        .iter()
        .copied()
        .filter(|(k, l)| !(*k == "Esc" && *l == "close"))
        .collect();
    lines.extend(form::hints(ctx.s, &hints, modal, width));
    lines
}

/// The confirmation or lifetime question above the notice.
fn mode_lines(panel: &KeysPanel, s: Styles, width: usize, lines: &mut Vec<Line<'static>>) {
    let warning = s.status(s.theme.status.warning);
    match &panel.mode {
        Mode::ConfirmRemove { label, fingerprint } => {
            lines.push(Line::from(vec![
                Span::styled("▲ ", warning),
                Span::styled(
                    format!(
                        "Remove {} from the agent?",
                        fmt::truncate(label, width.saturating_sub(30))
                    ),
                    s.title(),
                ),
            ]));
            let has_file = panel.locals().iter().any(|k| k.fingerprint == *fingerprint);
            lines.push(Line::from(Span::styled(
                if has_file {
                    "The key file stays in ~/.ssh: a adds it back."
                } else {
                    "Only the agent's copy is removed."
                },
                s.secondary(),
            )));
        }
        Mode::Lifetime { key, choice } => {
            lines.push(Line::from(Span::styled(
                format!("Add {} to the agent for", file_name(&key.private_path)),
                s.title(),
            )));
            let mut spans = Vec::new();
            for (i, (label, _)) in LIFETIMES.iter().enumerate() {
                let style = if i == *choice {
                    s.key_primary()
                } else {
                    s.key_hint()
                };
                spans.push(Span::styled(format!(" {label} "), style));
                spans.push(Span::raw("  "));
            }
            lines.push(Line::from(spans));
        }
        _ => {}
    }
}

fn vault_line(ctx: &Context) -> Line<'static> {
    let s = ctx.s;
    let st = s.theme.status;
    let (dot, color, state, note) = match ctx.vault {
        None => return Line::from(Span::styled("Secrets · checking…", s.muted())),
        Some(VaultStatus::NotInUse) => {
            return Line::from(vec![
                Span::styled("Secrets ", s.muted()),
                Span::styled("· system keyring", s.secondary()),
            ]);
        }
        Some(VaultStatus::Absent) => (
            "○",
            st.idle,
            "No vault yet",
            "no keyring here: create one to remember secrets",
        ),
        Some(VaultStatus::Locked) => ("●", st.warning, "locked", "remembered secrets unavailable"),
        Some(VaultStatus::Unlocked) => ("●", st.connected, "unlocked", "secrets can be remembered"),
    };
    Line::from(vec![
        Span::styled("Vault ", s.muted()),
        Span::styled(format!("{dot} "), s.status(color)),
        Span::styled(state, s.secondary()),
        Span::styled(format!("  · {note}"), s.muted()),
    ])
}

// ── lists ─────────────────────────────────────────────────────────────────

fn header(s: Styles, title: &str, count: Option<usize>, active: bool) -> ListItem<'static> {
    let style = if active { s.accent() } else { s.muted() };
    let mut spans = vec![Span::styled(
        title.to_owned(),
        style.add_modifier(ratatui::style::Modifier::BOLD),
    )];
    if let Some(n) = count {
        spans.push(Span::styled(format!("  {n}"), s.muted()));
    }
    ListItem::new(Line::from(spans))
}

/// Loading / error / empty rows of a section.
fn state_items<T>(
    s: Styles,
    tick: u64,
    load: &Load<Vec<T>>,
    empty: [&str; 2],
    width: usize,
) -> Vec<ListItem<'static>> {
    match load {
        Load::Loading => vec![ListItem::new(Line::from(vec![
            Span::styled(
                format!("  {} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Loading…", s.muted()),
        ]))],
        Load::Failed(e) => {
            let lines = form::notice(
                s,
                false,
                &e.message,
                e.hint.as_deref(),
                width.saturating_sub(2),
            );
            vec![ListItem::new(Text::from(
                lines
                    .into_iter()
                    .map(|mut l| {
                        l.spans.insert(0, Span::raw("  "));
                        l
                    })
                    .collect::<Vec<_>>(),
            ))]
        }
        Load::Ready(v) if v.is_empty() => vec![ListItem::new(Text::from(vec![
            Line::from(Span::styled(format!("  {}", empty[0]), s.secondary())),
            Line::from(Span::styled(format!("  {}", empty[1]), s.muted())),
        ]))],
        Load::Ready(_) => Vec::new(),
    }
}

/// Badge spans, most important first. Saturated colours only say
/// something about validity (an expiring or expired certificate); "in
/// agent" is a fact, shown neutral.
fn badges(
    s: Styles,
    in_agent: bool,
    cert: Option<CertBadge>,
    public_only: bool,
) -> Vec<Span<'static>> {
    let st = s.theme.status;
    let mut spans = Vec::new();
    if in_agent {
        spans.push(Span::styled("in agent", s.secondary()));
    }
    match cert {
        Some(CertBadge::Valid(text)) => spans.push(Span::styled(text, s.secondary())),
        Some(CertBadge::Soon(text)) => spans.push(Span::styled(text, s.status(st.warning))),
        Some(CertBadge::Expired) => spans.push(Span::styled("cert · expired", s.status(st.error))),
        Some(CertBadge::NotYet) => {
            spans.push(Span::styled("cert · not yet valid", s.status(st.warning)));
        }
        None => {}
    }
    if public_only {
        spans.push(Span::styled("public only", s.muted()));
    }
    spans
}

/// How a row is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    No,
    /// The selection, keys go to it: accent bar.
    Focused,
    /// Still the selection while an inline question is asked about it:
    /// same background, muted bar.
    Held,
}

/// Two lines: `▍● name  TYPE      badges` / `  fingerprint  comment`.
#[allow(clippy::too_many_arguments)] // one row's parts, named at the call site
fn key_item(
    s: Styles,
    pick: Pick,
    dot: (&'static str, ratatui::style::Style),
    name: &str,
    algorithm: &str,
    badge_spans: Vec<Span<'static>>,
    fingerprint: &str,
    comment: &str,
    width: usize,
) -> ListItem<'static> {
    let marker = match pick {
        Pick::Focused => Span::styled("▍", s.accent()),
        Pick::Held => Span::styled("▍", s.muted()),
        Pick::No => Span::raw(" "),
    };
    let algo = algorithm_label(algorithm);
    let badge_width: usize = badge_spans.iter().map(|b| b.width() + 2).sum();
    let name_room = width
        .saturating_sub(4 + algo.chars().count() + 2 + badge_width)
        .max(8);
    let name = fmt::truncate(name, name_room);
    let used = 3 + name.chars().count() + 2 + algo.chars().count();
    let pad = width.saturating_sub(used + badge_width).max(2);
    let mut first = vec![
        marker,
        Span::styled(format!("{} ", dot.0), dot.1),
        Span::styled(name, s.title()),
        Span::styled(format!("  {algo}"), s.secondary()),
        Span::raw(" ".repeat(pad)),
    ];
    for b in badge_spans {
        first.push(b);
        first.push(Span::raw("  "));
    }
    // The whole fingerprint when the comment still fits beside it.
    let comment_need = comment.chars().count().min(20) + 2;
    let fp_room = width.saturating_sub(5 + comment_need).max(24);
    let fp = short_fingerprint(fingerprint, fp_room);
    let comment_room = width.saturating_sub(fp.chars().count() + 7);
    let comment = crate::form::display_safe(comment);
    let second = Line::from(vec![
        Span::raw("   "),
        Span::styled(fp, s.muted()),
        Span::styled(
            format!("  {}", fmt::truncate(&comment, comment_room)),
            s.secondary(),
        ),
    ]);
    let item = ListItem::new(Text::from(vec![Line::from(first), second]));
    if pick == Pick::No {
        item
    } else {
        item.style(s.selected())
    }
}

/// What a list row is, for the "n more" counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Header,
    Key,
    Other,
}

/// Both sections as list rows, with what scrolling needs to know.
struct Rows {
    items: Vec<ListItem<'static>>,
    kinds: Vec<Row>,
    /// Row of the selection, if the current section has one.
    selected: Option<usize>,
    /// Topmost row that must stay in view with the selection: its
    /// section header when it is the section's first row (or when the
    /// section has no rows).
    anchor: usize,
}

impl Rows {
    fn height(&self) -> usize {
        self.items.iter().map(ListItem::height).sum()
    }
}

fn list_rows(panel: &KeysPanel, ctx: &Context, w: usize) -> Rows {
    let s = ctx.s;
    let held = if matches!(panel.mode, Mode::Browse) {
        Pick::Focused
    } else {
        Pick::Held
    };
    let mut rows = Rows {
        items: Vec::new(),
        kinds: Vec::new(),
        selected: None,
        anchor: 0,
    };
    let push = |rows: &mut Rows, item, kind| {
        rows.items.push(item);
        rows.kinds.push(kind);
    };

    let local_count = panel.local.ready().map(Vec::len);
    push(
        &mut rows,
        header(
            s,
            "LOCAL KEYS",
            local_count,
            panel.section == Section::Local,
        ),
        Row::Header,
    );
    for item in state_items(
        s,
        ctx.tick,
        &panel.local,
        [
            "No key pairs in ~/.ssh yet.",
            "Press n to generate one (Ed25519 is recommended).",
        ],
        w,
    ) {
        push(&mut rows, item, Row::Other);
    }
    for (i, key) in panel.locals().iter().enumerate() {
        let pick = if panel.section == Section::Local && panel.local_sel == i {
            rows.selected = Some(rows.items.len());
            if i > 0 {
                rows.anchor = rows.items.len();
            }
            held
        } else {
            Pick::No
        };
        push(&mut rows, local_item(panel, key, pick, s, w), Row::Key);
    }

    push(&mut rows, ListItem::new(Line::from("")), Row::Other);
    let agent_header = rows.items.len();
    if panel.section == Section::Agent {
        rows.anchor = agent_header;
    }
    let agent_count = panel.agent.ready().map(Vec::len);
    push(
        &mut rows,
        header(s, "SSH AGENT", agent_count, panel.section == Section::Agent),
        Row::Header,
    );
    for item in state_items(
        s,
        ctx.tick,
        &panel.agent,
        [
            "The agent holds no keys.",
            "Select a local key and press a to add it.",
        ],
        w,
    ) {
        push(&mut rows, item, Row::Other);
    }
    for (i, agent) in panel.agents().iter().enumerate() {
        let pick = if panel.section == Section::Agent && panel.agent_sel == i {
            rows.selected = Some(rows.items.len());
            if i > 0 {
                rows.anchor = rows.items.len();
            }
            held
        } else {
            Pick::No
        };
        push(&mut rows, agent_item(panel, agent, pick, s, w), Row::Key);
    }
    rows
}

/// First row to show so that `anchor..=bottom` is in view, starting from
/// the previous `offset` (scrolls as little as possible).
fn scroll_offset(
    heights: &[usize],
    view: usize,
    offset: usize,
    anchor: usize,
    bottom: usize,
) -> usize {
    let span = |from: usize, to: usize| -> usize {
        heights.iter().take(to.saturating_add(1)).skip(from).sum()
    };
    let mut offset = offset.min(anchor);
    while offset < bottom && span(offset, bottom) > view {
        offset += 1;
    }
    // No blank space under the last row while rows are hidden above.
    let last = heights.len().saturating_sub(1);
    while offset > 0 && span(offset - 1, last) <= view {
        offset -= 1;
    }
    offset
}

/// The rows from the scroll offset; when some are hidden, the last line
/// says how many keys are above and below (muted).
fn render_rows(panel: &mut KeysPanel, rows: Rows, s: Styles, frame: &mut Frame<'_>, area: Rect) {
    let heights: Vec<usize> = rows.items.iter().map(ListItem::height).collect();
    let avail = usize::from(area.height);
    if rows.height() <= avail {
        *panel.list.offset_mut() = 0;
        frame.render_widget(List::new(rows.items), area);
        return;
    }
    // One line for the indicator.
    let view = avail.saturating_sub(1).max(1);
    let bottom = rows.selected.unwrap_or(rows.anchor);
    let offset = scroll_offset(&heights, view, panel.list.offset(), rows.anchor, bottom);
    *panel.list.offset_mut() = offset;
    let mut used = 0;
    let shown = heights
        .iter()
        .skip(offset)
        .take_while(|h| {
            if used + **h > view {
                return false;
            }
            used += **h;
            true
        })
        .count();
    let keys_in = |range: &mut dyn Iterator<Item = &Row>| range.filter(|k| **k == Row::Key).count();
    let above = keys_in(&mut rows.kinds.iter().take(offset));
    let below = keys_in(&mut rows.kinds.iter().skip(offset + shown));
    let rows_below = rows.kinds.len().saturating_sub(offset + shown);
    let mut spans = Vec::new();
    if above > 0 {
        spans.push(Span::styled(format!("↑ {above} more"), s.muted()));
    }
    if below > 0 || (rows_below > 0 && above == 0) {
        if !spans.is_empty() {
            spans.push(Span::styled("   ", s.muted()));
        }
        spans.push(Span::styled(
            if below > 0 {
                format!("↓ {below} more")
            } else {
                "↓ more".to_owned()
            },
            s.muted(),
        ));
    }
    // The indicator right under the last row shown.
    let [list_area, more_area] = Layout::vertical([
        Constraint::Length(u16::try_from(used).unwrap_or(1)),
        Constraint::Length(1),
    ])
    .areas(area);
    let items: Vec<ListItem<'static>> = rows.items.into_iter().skip(offset).take(shown).collect();
    frame.render_widget(List::new(items), list_area);
    frame.render_widget(Paragraph::new(Line::from(spans)), more_area);
}

fn local_item(
    panel: &KeysPanel,
    key: &LocalKey,
    pick: Pick,
    s: Styles,
    w: usize,
) -> ListItem<'static> {
    let in_agent = panel.in_agent(key);
    // Neutral: green is for connection status only.
    let dot = if in_agent {
        ("●", s.secondary())
    } else {
        ("○", s.muted())
    };
    let cert = key
        .certificate
        .as_ref()
        .map(|c| cert_badge(c.validity_at(panel.now), c.valid_before));
    key_item(
        s,
        pick,
        dot,
        &file_name(&key.private_path),
        &key.algorithm,
        badges(s, in_agent, cert, !key.has_private),
        &key.fingerprint,
        &key.comment,
        w,
    )
}

fn agent_item(
    panel: &KeysPanel,
    agent: &AgentKey,
    pick: Pick,
    s: Styles,
    w: usize,
) -> ListItem<'static> {
    let name = panel.local_of(agent).map_or_else(
        || {
            if agent.comment.is_empty() {
                "(no comment)".to_owned()
            } else {
                agent.comment.clone()
            }
        },
        |k| file_name(&k.private_path),
    );
    let cert = agent
        .certificate
        .as_ref()
        .map(|c| cert_badge(c.validity_at(panel.now), c.valid_before));
    let comment = if panel.local_of(agent).is_some() {
        agent.comment.clone()
    } else {
        String::new()
    };
    key_item(
        s,
        pick,
        ("●", s.secondary()),
        &name,
        &agent.algorithm,
        badges(s, false, cert, false),
        &agent.fingerprint,
        &comment,
        w,
    )
}

// ── generation form ──────────────────────────────────────────────────────

fn generate_lines(f: &GenForm, ctx: &Context, w: usize) -> Vec<Line<'static>> {
    let s = ctx.s;
    let lw = 12;
    let field_w = w.saturating_sub(lw).min(40);
    let mut lines = vec![
        Line::from(Span::styled("A new key pair in ~/.ssh", s.secondary())),
        Line::from(""),
        form::input(s, "Name", lw, &f.name, f.focus == GenField::Name, field_w),
    ];
    if let Some(p) = f.name_problem() {
        lines.extend(form::problem(s, lw, &p, w));
    }
    let choice = f.choice();
    lines.push(form::select(
        s,
        "Type",
        lw,
        choice.label,
        choice.note,
        f.focus == GenField::Algorithm,
    ));
    lines.push(form::input(
        s,
        "Comment",
        lw,
        &f.comment,
        f.focus == GenField::Comment,
        field_w,
    ));
    if let Some(p) = f.comment_problem() {
        lines.extend(form::problem(s, lw, p, w));
    }
    lines.push(form::secret(
        s,
        "Passphrase",
        lw,
        !f.passphrase.is_empty(),
        f.focus == GenField::Passphrase,
        field_w,
    ));
    if let Some(p) = f.passphrase_problem() {
        lines.extend(form::problem(s, lw, p, w));
        if f.retype() && f.passphrase.is_empty() {
            lines.push(Line::from(vec![
                Span::raw(" ".repeat(lw)),
                Span::styled(
                    fmt::truncate(
                        "It was cleared. Or Ctrl-N: no passphrase.",
                        w.saturating_sub(lw),
                    ),
                    s.secondary(),
                ),
            ]));
        }
    }
    let mut rule = crate::ui::dialog::passphrase_rule(s, f.passphrase_progress());
    rule.spans.insert(0, Span::raw(" ".repeat(lw)));
    lines.push(rule);
    lines.push(form::secret(
        s,
        "Confirm",
        lw,
        !f.confirm.is_empty(),
        f.focus == GenField::Confirm,
        field_w,
    ));
    if let Some(p) = f.confirm_problem() {
        lines.extend(form::problem(s, lw, p, w));
    } else if f.passphrase.is_empty() && !f.retype() {
        // A soft warning, not an error: an unencrypted key is a choice.
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(lw)),
            Span::styled(
                fmt::truncate(
                    "No passphrase: anyone who copies the file can use the key.",
                    w.saturating_sub(lw),
                ),
                s.status(s.theme.status.warning),
            ),
        ]));
    }
    if let Some(busy) = &f.busy {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(ctx.tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled(busy.clone(), s.secondary()),
        ]));
    }
    if let Some(e) = &f.error {
        lines.push(Line::from(""));
        lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
    }
    lines
}

// ── host picker ──────────────────────────────────────────────────────────

fn render_picker(frame: &mut Frame<'_>, area: Rect, picker: &mut HostPicker, ctx: &Context) {
    let s = ctx.s;
    let w = usize::from(area.width);
    let mut top = vec![
        Line::from(Span::styled(
            format!(
                "Install {} on which host?",
                file_name(&picker.key.public_path)
            ),
            s.secondary(),
        )),
        Line::from(""),
        form::input(
            s,
            "Host",
            6,
            &picker.query,
            true,
            w.saturating_sub(6).min(40),
        ),
        Line::from(""),
    ];
    let footer_lines = footer(ctx, None, w, true);
    if picker.hits.is_empty() {
        top.push(Line::from(Span::styled(
            if picker.has_hosts() {
                "No host matches. Ctrl-U clears the search."
            } else {
                "No hosts in ~/.ssh/config yet."
            },
            s.muted(),
        )));
        render_with_footer(frame, area, Paragraph::new(top), &footer_lines);
        return;
    }
    let top_height = u16::try_from(top.len()).unwrap_or(4);
    let footer_height = u16::try_from(footer_lines.len()).unwrap_or(2);
    let [head, list_area, foot] = Layout::vertical([
        Constraint::Length(top_height),
        Constraint::Min(1),
        Constraint::Length(footer_height),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(top), head);
    let selected = picker.list.selected();
    let items: Vec<ListItem<'static>> = picker
        .hits
        .iter()
        .enumerate()
        .filter_map(|(i, hit)| {
            let (alias, target) = picker.row(hit)?;
            let marker = if selected == Some(i) {
                Span::styled("▍", s.accent())
            } else {
                Span::raw(" ")
            };
            let alias = fmt::truncate(alias, 28);
            Some(ListItem::new(Line::from(vec![
                marker,
                Span::styled(format!("{alias:<28}"), s.title()),
                Span::styled(
                    format!("  {}", fmt::truncate(&target, w.saturating_sub(32))),
                    s.muted(),
                ),
            ])))
        })
        .collect();
    frame.render_stateful_widget(
        List::new(items).highlight_style(s.selected()),
        list_area,
        &mut picker.list,
    );
    frame.render_widget(Paragraph::new(footer_lines), foot);
}

// ── copy-id progress ─────────────────────────────────────────────────────

/// `✓ Connecting  ·  ⣾ Installing  ·  ○ Verifying` (`– Verifying` when it
/// is skipped: a `.pub` alone cannot log in).
fn step_line(ctx: &Context, job: &CopyJob) -> Line<'static> {
    let s = ctx.s;
    let st = s.theme.status;
    let current = job.step.stage();
    let failed = matches!(job.step, Step::Failed(_));
    let done = matches!(job.step, Step::Done(_));
    let mut spans = Vec::new();
    for (i, label) in ["Connecting", "Installing", "Verifying"].iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", s.muted()));
        }
        let skipped = i == 2 && job.unverified();
        let (mark, style) = if failed && i == job.failed_at {
            ("✕".to_owned(), s.status(st.error))
        } else if skipped && (done || !failed) {
            ("–".to_owned(), s.muted())
        } else if failed && i > job.failed_at {
            ("○".to_owned(), s.muted())
        } else if i < current || done {
            ("✓".to_owned(), s.status(st.connected))
        } else if i == current {
            (spinner(ctx.tick).to_owned(), s.status(st.connecting))
        } else {
            ("○".to_owned(), s.muted())
        };
        let text_style = if skipped {
            s.muted()
        } else if i == current && !failed {
            s.title()
        } else {
            s.secondary()
        };
        spans.push(Span::styled(format!("{mark} "), style));
        spans.push(Span::styled((*label).to_owned(), text_style));
    }
    Line::from(spans)
}

fn copy_lines(ctx: &Context, w: usize) -> Vec<Line<'static>> {
    let s = ctx.s;
    let Some(job) = &ctx.job else {
        return vec![Line::from(Span::styled("No copy in progress.", s.muted()))];
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Install ", s.secondary()),
        Span::styled(job.key_name.clone(), s.title()),
        Span::styled(" on ", s.secondary()),
        Span::styled(job.alias.clone(), s.title()),
        Span::styled(
            if job.unverified() {
                " (public key only: the login cannot be checked)."
            } else {
                " and check that it logs in."
            },
            s.secondary(),
        ),
    ])];
    lines.push(Line::from(""));
    lines.push(step_line(ctx, job));
    lines.push(Line::from(""));
    match &job.step {
        Step::Connecting => lines.push(Line::from(Span::styled(
            ctx.job_status
                .clone()
                .unwrap_or_else(|| "Connecting".into()),
            s.muted(),
        ))),
        Step::Installing => lines.push(Line::from(Span::styled(
            "Adding the public key to ~/.ssh/authorized_keys…",
            s.muted(),
        ))),
        Step::Verifying(_) => lines.push(Line::from(Span::styled(
            format!("Logging in again with {} only…", job.key_name),
            s.muted(),
        ))),
        Step::Done(_) => {
            if let Some(message) = job.success_message() {
                lines.extend(form::notice(s, true, &message, None, w));
            }
            if !job.unverified() {
                let (lead, line) = job.config_advice();
                lines.push(Line::from(""));
                for l in fmt::wrap(&lead, w) {
                    lines.push(Line::from(Span::styled(l, s.secondary())));
                }
                // The line to copy, as code: on the raised surface.
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!(" {} ", fmt::truncate(&line, w.saturating_sub(4))),
                        s.title().patch(s.elevated()),
                    ),
                ]));
            }
            lines.push(Line::from(Span::styled(
                "Portico never edits ~/.ssh/config.",
                s.muted(),
            )));
        }
        Step::Failed(error) => {
            lines.extend(form::notice(
                s,
                false,
                &error.message,
                error.hint.as_deref(),
                w,
            ));
        }
    }
    lines
}
