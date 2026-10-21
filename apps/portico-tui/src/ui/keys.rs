//! Keys overlay: local key pairs, then `ssh-agent` identities (two-line
//! rows: name, type and badges; fingerprint and comment), the vault status
//! and the keys that work. Sub-modes (generation form, lifetime choice,
//! removal confirmation, host picker, copy-id progress) replace the list.

use portico_core::AgentKey;
use portico_core::keys::LocalKey;
use portico_core::vault::VaultStatus;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph};

use crate::app::App;
use crate::copy_job::{CopyJob, Step};
use crate::keys::{
    CertBadge, GenField, GenForm, HostPicker, KeysPanel, LIFETIMES, Load, Mode, Section,
    algorithm_label, cert_badge, file_name, short_fingerprint,
};
use crate::theme::Styles;
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
}

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let job = app.copy_job.clone();
    let job_status = job
        .as_ref()
        .map(|j| status::status(app.session_by_id(j.session), s).label);
    let copy = app.copy_state();
    let vault = app.vault;
    let tick = app.tick;
    let Some(panel) = &mut app.keys_panel else {
        return;
    };
    let ctx = Context {
        s,
        tick,
        vault,
        job,
        job_status,
        hints: panel.hints(vault, copy),
    };
    draw(panel, &ctx, frame);
}

fn draw(panel: &mut KeysPanel, ctx: &Context, frame: &mut Frame<'_>) {
    let s = ctx.s;
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(100);
    let height = screen.height.saturating_sub(2).min(32);
    let [v] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(screen);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(v);
    let subtitle = match &panel.mode {
        Mode::Generate(_) => "· new key pair".to_owned(),
        Mode::PickHost(p) => format!("· install {}", file_name(&p.key.private_path)),
        Mode::Copy => ctx
            .job
            .as_ref()
            .map(|j| format!("· {} → {}", j.key_name, j.alias))
            .unwrap_or_default(),
        _ => "· ~/.ssh and ssh-agent".to_owned(),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Keys ", s.title()),
            Span::styled(format!("{} ", fmt::truncate(&subtitle, 60)), s.muted()),
        ]))
        .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
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
            render_with_footer(
                frame,
                inner,
                Paragraph::new(lines),
                &footer(ctx, None, w, false),
            );
        }
        _ => {
            let mut extra = Vec::new();
            mode_lines(panel, s, w, &mut extra);
            if let Some(notice) = &panel.notice {
                extra.extend(form::notice(
                    s,
                    notice.ok,
                    &notice.message,
                    notice.hint.as_deref(),
                    w,
                ));
            }
            if let Some(line) = &panel.public_line {
                extra.extend(
                    // Cut at the edge, not at the space: the type and the
                    // key stay one selectable run.
                    line.chars()
                        .collect::<Vec<_>>()
                        .chunks(w.max(1))
                        .take(4)
                        .map(|c| Line::from(Span::styled(c.iter().collect::<String>(), s.title()))),
                );
            }
            extra.push(vault_line(ctx));
            let modal = !matches!(panel.mode, Mode::Browse);
            let lines = footer(ctx, Some(extra), w, modal);
            let footer_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let [list_area, footer_area] =
                Layout::vertical([Constraint::Min(2), Constraint::Length(footer_height)])
                    .areas(inner);
            render_lists(panel, ctx, frame, list_area);
            frame.render_widget(Paragraph::new(lines), footer_area);
        }
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
            "not created",
            "no keyring here: create it to remember secrets",
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

/// Badge spans, most important first.
fn badges(
    s: Styles,
    in_agent: bool,
    cert: Option<CertBadge>,
    public_only: bool,
) -> Vec<Span<'static>> {
    let st = s.theme.status;
    let mut spans = Vec::new();
    if in_agent {
        spans.push(Span::styled("in agent", s.status(st.connected)));
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

/// Two lines: `▍● name  TYPE      badges` / `  fingerprint  comment`.
#[allow(clippy::too_many_arguments)] // one row's parts, named at the call site
fn key_item(
    s: Styles,
    selected: bool,
    dot: (&'static str, ratatui::style::Style),
    name: &str,
    algorithm: &str,
    badge_spans: Vec<Span<'static>>,
    fingerprint: &str,
    comment: &str,
    width: usize,
) -> ListItem<'static> {
    let marker = if selected {
        Span::styled("▍", s.accent())
    } else {
        Span::raw(" ")
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
    if selected {
        item.style(s.selected())
    } else {
        item
    }
}

fn render_lists(panel: &mut KeysPanel, ctx: &Context, frame: &mut Frame<'_>, area: Rect) {
    let s = ctx.s;
    let w = usize::from(area.width);
    let browsing = matches!(panel.mode, Mode::Browse);
    let mut items: Vec<ListItem<'static>> = Vec::new();
    let mut selected_index = None;

    let locals = panel.locals();
    let local_count = panel.local.ready().map(Vec::len);
    items.push(header(
        s,
        "LOCAL KEYS",
        local_count,
        panel.section == Section::Local,
    ));
    items.extend(state_items(
        s,
        ctx.tick,
        &panel.local,
        [
            "No key pairs in ~/.ssh yet.",
            "Press n to generate one (Ed25519 is recommended).",
        ],
        w,
    ));
    for (i, key) in locals.iter().enumerate() {
        let selected = browsing && panel.section == Section::Local && panel.local_sel == i;
        if selected {
            selected_index = Some(items.len());
        }
        items.push(local_item(panel, key, selected, s, w));
    }

    items.push(ListItem::new(Line::from("")));
    let agent_count = panel.agent.ready().map(Vec::len);
    items.push(header(
        s,
        "SSH AGENT",
        agent_count,
        panel.section == Section::Agent,
    ));
    items.extend(state_items(
        s,
        ctx.tick,
        &panel.agent,
        [
            "The agent holds no keys.",
            "Select a local key and press a to add it.",
        ],
        w,
    ));
    for (i, agent) in panel.agents().iter().enumerate() {
        let selected = browsing && panel.section == Section::Agent && panel.agent_sel == i;
        if selected {
            selected_index = Some(items.len());
        }
        items.push(agent_item(panel, agent, selected, s, w));
    }

    // Keep the section header in view when the first row is selected.
    let select = selected_index.or(match panel.section {
        Section::Local => Some(0),
        Section::Agent => Some(items.len().saturating_sub(1)),
    });
    let mut state = ListState::default().with_offset(panel.list.offset());
    state.select(select);
    frame.render_stateful_widget(List::new(items), area, &mut state);
    *panel.list.offset_mut() = state.offset();
}

fn local_item(
    panel: &KeysPanel,
    key: &LocalKey,
    selected: bool,
    s: Styles,
    w: usize,
) -> ListItem<'static> {
    let in_agent = panel.in_agent(key);
    let dot = if in_agent {
        ("●", s.status(s.theme.status.connected))
    } else {
        ("○", s.muted())
    };
    let cert = key
        .certificate
        .as_ref()
        .map(|c| cert_badge(c.validity_at(panel.now), c.valid_before));
    key_item(
        s,
        selected,
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
    selected: bool,
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
        selected,
        ("●", s.status(s.theme.status.connected)),
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
    } else if f.passphrase.is_empty() {
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

fn copy_lines(ctx: &Context, w: usize) -> Vec<Line<'static>> {
    let s = ctx.s;
    let st = s.theme.status;
    let Some(job) = &ctx.job else {
        return vec![Line::from(Span::styled("No copy in progress.", s.muted()))];
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("Install ", s.secondary()),
        Span::styled(job.key_name.clone(), s.title()),
        Span::styled(" on ", s.secondary()),
        Span::styled(job.alias.clone(), s.title()),
        Span::styled(" and check that it logs in.", s.secondary()),
    ])];
    lines.push(Line::from(""));
    // Steps: done ✓, current spinner, pending ○, failed ✕.
    let current = job.step.stage();
    let failed = matches!(job.step, Step::Failed(_));
    let mut spans = Vec::new();
    for (i, label) in ["Connecting", "Installing", "Verifying"].iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", s.muted()));
        }
        let (mark, style) = if failed && i == job.failed_at {
            ("✕".to_owned(), s.status(st.error))
        } else if failed && i > job.failed_at {
            ("○".to_owned(), s.muted())
        } else if i < current || matches!(job.step, Step::Done(_)) {
            ("✓".to_owned(), s.status(st.connected))
        } else if i == current {
            (spinner(ctx.tick).to_owned(), s.status(st.connecting))
        } else {
            ("○".to_owned(), s.muted())
        };
        let text_style = if i == current && !failed {
            s.title()
        } else {
            s.secondary()
        };
        spans.push(Span::styled(format!("{mark} "), style));
        spans.push(Span::styled((*label).to_owned(), text_style));
    }
    lines.push(Line::from(spans));
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
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "To use it by default:",
                s.secondary(),
            )));
            for l in fmt::wrap(&job.config_advice(), w.saturating_sub(2)) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(l, s.title()),
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
