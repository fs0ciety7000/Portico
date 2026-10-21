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
use portico_core::hosts::HostEntry;

use crate::app::App;
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

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
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
        return;
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
        Line::from(""),
    ];
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
    let hint: Vec<(&str, &str)> = match phase {
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
