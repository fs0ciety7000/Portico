//! Main pane: effective configuration of the selected host.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Wrap};

use portico_core::events::SessionPhase;
use portico_core::hosts::HostEntry;

use crate::app::App;
use crate::theme::Styles;
use crate::ui::fmt::display_path;

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
    let mut lines = vec![
        Line::from(Span::styled(host.alias.clone(), s.title())),
        Line::from(vec![
            Span::styled(format!("{user}@{}", r.hostname), s.secondary()),
            Span::styled(format!(":{}", r.port), s.muted()),
        ]),
        Line::from(""),
    ];
    lines.extend(status_lines(app, host, s));
    lines.push(Line::from(""));

    let mut row = |label: &str, value: String| {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<14}"), s.muted()),
            Span::styled(value, s.secondary()),
        ]));
    };
    if !r.proxy_jump.is_empty() {
        let chain: Vec<String> = r.proxy_jump.iter().map(|j| j.host.clone()).collect();
        row("Jump", format!("{} → {}", chain.join(" → "), host.alias));
    }
    if let Some(cmd) = &r.proxy_command {
        row("ProxyCommand", cmd.clone());
    }
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
    row(
        "Defined in",
        format!(
            "{}:{}",
            display_path(&host.source.file.to_string_lossy()),
            host.source.line
        ),
    );
    for w in &r.warnings {
        lines.push(Line::from(Span::styled(
            format!("⚠ {}", w.message),
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

/// Status, latency and what can be done next; a failure gets its message,
/// hint and remedy on their own lines.
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
    let hint: &[(&str, &str)] = match phase {
        Some(SessionPhase::Connected) => &[("↵", "open terminal"), ("d", "disconnect")],
        Some(_) if view.is_some_and(crate::app::SessionView::in_flight) => &[("d", "cancel")],
        Some(SessionPhase::Failed { .. }) => &[("↵", "retry")],
        _ => &[("↵", "connect")],
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
                error.message.clone(),
                s.secondary(),
            ))),
            _ => {
                lines.push(Line::from(Span::styled(error.message.clone(), error_style)));
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
