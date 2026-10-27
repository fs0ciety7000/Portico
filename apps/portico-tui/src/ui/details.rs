//! Main pane: effective configuration of the selected host.
//!
//! Sensitive values (commands that run locally or remotely, see
//! [`SENSITIVE_KEYWORDS`]) are masked here as everywhere: they can be
//! revealed in the session logs' Effective config tab, never copied.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Wrap};

use portico_core::effective::SENSITIVE_KEYWORDS;
use portico_core::events::SessionPhase;
use portico_core::hosts::{HostEntry, HostOrigin};

use crate::app::App;
use crate::host_form::{origin_label, origin_with_source};
use crate::theme::Styles;
use crate::ui::fmt::{display_path, sentence};

/// Label column of the rows.
const LABEL: usize = 14;

/// How `ssh_config` spells a sensitive keyword (the core keeps them lowercase).
fn keyword_label(keyword: &str) -> &str {
    match keyword {
        "proxycommand" => "ProxyCommand",
        "localcommand" => "LocalCommand",
        "remotecommand" => "RemoteCommand",
        "knownhostscommand" => "KnownHostsCommand",
        other => other,
    }
}

/// `Portico · e edits`, `Portico · from Termix` (shared with the GUI; one
/// row at 80 columns), `~/.ssh/config · read-only`, `Imported · read-only`.
fn origin_text(app: &App, host: &HostEntry) -> String {
    match host.origin {
        HostOrigin::Portico if host.imported_from.is_some() => {
            origin_with_source(host.origin, host.imported_from)
        }
        HostOrigin::Portico => format!(
            "{} · {} edits",
            origin_label(host.origin),
            app.keymap.shown("hosts.edit", 1)
        ),
        other => format!("{} · read-only", origin_label(other)),
    }
}

/// The `Include` banner (under the details): what to add to
/// `~/.ssh/config`, that Portico never edits it, and its keys.
fn include_lines(app: &App, s: Styles, w: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = crate::ui::fmt::wrap(crate::app_hosts::INCLUDE_BANNER, w)
        .into_iter()
        .map(|l| Line::from(Span::styled(l, s.secondary())))
        .collect();
    // The line itself, cut nowhere (wrapped by characters if it must).
    let line: Vec<char> = app.include.line.chars().collect();
    for chunk in line.chunks(w.max(1)) {
        lines.push(Line::from(Span::styled(
            chunk.iter().collect::<String>(),
            s.title(),
        )));
    }
    lines.push(Line::from(Span::styled(
        "Portico never edits that file.",
        s.muted(),
    )));
    let mut keys = Vec::new();
    for (id, label) in [
        ("hosts.copy_include", "copy"),
        ("hosts.hide_include", "hide"),
    ] {
        let key = app.keymap.shown(id, 1);
        if !key.is_empty() {
            keys.push(Span::styled(format!(" {key} "), s.key_hint()));
            keys.push(Span::styled(format!(" {label}   "), s.muted()));
        }
    }
    lines.push(Line::from(keys));
    lines
}

/// The `Include` banner at the bottom of `area`, when shown; returns what
/// is left for the details.
fn render_banner(app: &App, frame: &mut Frame<'_>, area: Rect) -> Rect {
    let s = app.styles();
    if app.include_banner() {
        let w = usize::from(area.width.saturating_sub(4));
        let lines = include_lines(app, s, w);
        let height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        let [details, banner] = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Min(6),
            ratatui::layout::Constraint::Length(height),
        ])
        .areas(area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(s.border())
                    .title(Line::from(Span::styled(" ssh · scp · git ", s.secondary())))
                    .padding(Padding::horizontal(1))
                    .style(s.surface()),
            ),
            banner,
        );
        details
    } else {
        area
    }
}

/// `⇅ changed by laptop (3f9c0ae1) · S to review`: a destination received
/// through sync waits for this device (F1).
fn received_line(app: &App, alias: &str) -> Option<Line<'static>> {
    let s = app.styles();
    let change = app.sync.pending_for(alias)?;
    let key = app.keymap.shown("hosts.sync", 1);
    let mut spans = vec![
        Span::styled("⇅ ", s.status(s.theme.status.warning)),
        Span::styled(
            crate::ui::sync::received_label(change),
            s.status(s.theme.status.warning),
        ),
    ];
    if !key.is_empty() {
        spans.push(Span::styled(format!(" · {key} to review"), s.muted()));
    }
    Some(Line::from(spans))
}

/// The sync banner at the bottom of `area` when received hosts or
/// snippets wait for this device; returns what is left.
fn render_sync_banner(app: &App, frame: &mut Frame<'_>, area: Rect) -> Rect {
    let s = app.styles();
    let w = usize::from(area.width.saturating_sub(4));
    let lines = crate::ui::sync::banner_lines(app, w);
    if lines.is_empty() || area.height < 14 {
        return area;
    }
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let [details, banner] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(6),
        ratatui::layout::Constraint::Length(height),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.border())
                .title(Line::from(Span::styled(" Sync ", s.secondary())))
                .padding(Padding::horizontal(1))
                .style(s.surface()),
        ),
        banner,
    );
    details
}

/// Returns the area of the details (above the `Include` banner, if any).
#[allow(clippy::too_many_lines)] // one row per resolved option, in order
pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) -> Rect {
    let s = app.styles();
    let area = render_sync_banner(app, frame, area);
    let area = render_banner(app, frame, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border())
        .padding(Padding::new(2, 2, 1, 1))
        .style(s.surface());

    let Some(host) = app.selected_host() else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("Select a host", s.secondary())).centered(),
                Line::from(Span::styled("↑ ↓ to move · Enter to connect", s.muted())).centered(),
            ])
            .block(block),
            area,
        );
        return area;
    };

    let r = &host.resolved;
    let user = r.user.as_deref().unwrap_or("(local user)");
    let mut title = vec![Span::styled(host.alias.clone(), s.title())];
    if host.meta.favorite {
        title.push(Span::styled(" ★", s.accent()));
    }
    let mut lines = vec![
        Line::from(title),
        Line::from(vec![
            Span::styled(format!("{user}@{}", r.hostname), s.secondary()),
            Span::styled(format!(":{}", r.port), s.muted()),
        ]),
    ];
    lines.extend(received_line(app, &host.alias));
    lines.push(Line::from(""));
    lines.extend(status_lines(app, host, s));
    lines.push(Line::from(""));
    lines.extend(route_lines(app, host, s));

    let mut row = |label: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<LABEL$} "), s.muted()),
            Span::styled(value, s.secondary()),
        ]));
    };
    for f in &r.identity_files {
        row("Identity", display_path(&f.to_string_lossy()));
    }
    for f in &r.certificate_files {
        row("Certificate", display_path(&f.to_string_lossy()));
    }
    for f in &r.local_forwards {
        row("Forward -L", f.clone());
    }
    for f in &r.remote_forwards {
        row("Forward -R", f.clone());
    }
    if !host.meta.tags.is_empty() {
        row(
            "Tags",
            host.meta
                .tags
                .iter()
                .map(|t| format!("#{t}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    if let Some(group) = &host.meta.group {
        row("Group", crate::form::display_safe(group));
    }
    row("Origin", origin_text(app, host));
    row(
        "Defined in",
        format!(
            "{}:{}",
            display_path(&host.source.file.to_string_lossy()),
            host.source.line
        ),
    );
    if let Some(notes) = &host.meta.notes {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Notes", s.muted())));
        for line in notes.lines() {
            lines.push(Line::from(Span::styled(
                crate::form::display_safe(line),
                s.secondary(),
            )));
        }
    }
    for w in &r.warnings {
        lines.push(Line::from(Span::styled(
            format!("⚠ {}", sentence(&w.message)),
            s.status(s.theme.status.warning),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
    area
}

/// `Jump  You → bastion → web`, then the sensitive commands, masked (the
/// Effective config tab of the logs reveals them).
fn route_lines(app: &App, host: &HostEntry, s: Styles) -> Vec<Line<'static>> {
    let r = &host.resolved;
    let mut lines = Vec::new();
    if !r.proxy_jump.is_empty() {
        let chain: Vec<String> = r.proxy_jump.iter().map(|j| j.host.clone()).collect();
        lines.push(Line::from(vec![
            Span::styled(format!("{:<LABEL$} ", "Jump"), s.muted()),
            Span::styled(
                format!("You → {} → {}", chain.join(" → "), host.alias),
                s.secondary(),
            ),
        ]));
    }
    let reveal = format!(
        "  hidden · {} reveals in logs",
        app.keymap.shown("hosts.debug", 1)
    );
    for keyword in SENSITIVE_KEYWORDS {
        if r.options.contains_key(keyword) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<LABEL$} ", keyword_label(keyword)), s.muted()),
                Span::styled("•••", s.muted()),
                Span::styled(reveal.clone(), s.muted()),
            ]));
        }
    }
    lines
}

/// Status, latency and what can be done next; a failure gets its message,
/// hint and remedies (retry, logs) on their own lines.
fn status_lines(app: &App, host: &HostEntry, s: Styles) -> Vec<Line<'static>> {
    let view = app.session_for(&host.alias);
    let st = super::status::status(view, s);
    let dot = if st.busy {
        super::spinner(app.tick)
    } else {
        "●"
    };
    let mut spans = vec![
        Span::styled(format!("{dot} "), s.status(st.color)),
        Span::styled(st.label, s.secondary()),
    ];
    if let Some(rtt) = st.latency {
        let style = if st.slow {
            s.status(s.theme.status.warning)
        } else {
            s.muted()
        };
        spans.push(Span::styled(" · ", s.muted()));
        spans.push(Span::styled(rtt, style));
    }
    let phase = view.map(|v| &v.phase);
    let logs = app.keymap.shown("hosts.debug", 1);
    // Picking the host of a new pane: `↵` splits (the only action then).
    let split_with = format!("split with {}", crate::ui::fmt::truncate(&host.alias, 24));
    let hint: Vec<(&str, &str)> = match phase {
        _ if app.pane_pick.is_some() => vec![("↵", split_with.as_str())],
        Some(SessionPhase::Connected) => vec![("↵", "open terminal"), ("d", "disconnect")],
        Some(_) if view.is_some_and(crate::app::SessionView::in_flight) => vec![("d", "cancel")],
        // What went wrong is in the session's records.
        Some(SessionPhase::Failed { .. }) => vec![("↵", "retry"), (logs.as_str(), "logs")],
        _ => vec![("↵", "connect")],
    };
    let mut lines = vec![Line::from(spans)];

    if let Some(SessionPhase::Failed { error }) = phase {
        lines.push(Line::from(""));
        let error_style = s.status(s.theme.status.error);
        match error.code.as_str() {
            // The blocking alert already showed the details: keep it short.
            "host_key_changed" => lines.push(Line::from(Span::styled(
                "Host key changed: possible man-in-the-middle.",
                error_style,
            ))),
            // A refusal the user chose is not shown as an error.
            "host_key_rejected" => lines.push(Line::from(Span::styled(
                sentence(&error.message),
                s.secondary(),
            ))),
            _ => {
                lines.push(Line::from(Span::styled(
                    sentence(&error.message),
                    error_style,
                )));
                if let Some(hint) = &error.hint {
                    lines.push(Line::from(Span::styled(hint.clone(), s.secondary())));
                }
            }
        }
        if error.code == "host_key_changed" {
            let r = &host.resolved;
            let target = if r.port == 22 {
                r.hostname.clone()
            } else {
                format!("[{}]:{}", r.hostname, r.port)
            };
            lines.push(Line::from(vec![
                Span::styled("Fix  ", s.muted()),
                Span::styled(format!("ssh-keygen -R '{target}'"), s.title()),
            ]));
        }
    }
    lines.push(Line::from(""));
    let mut keys = Vec::new();
    for (key, label) in hint {
        keys.push(Span::styled(format!(" {key} "), s.key_hint()));
        keys.push(Span::styled(format!(" {label}   "), s.muted()));
    }
    lines.push(Line::from(keys));
    lines
}
