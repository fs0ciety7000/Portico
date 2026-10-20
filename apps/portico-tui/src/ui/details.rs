//! Main pane: effective configuration of the selected host.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph, Wrap};

use crate::app::App;
use crate::theme::Styles;

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
        status_line(s),
        Line::from(""),
    ];

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
        row("Identity", f.display().to_string());
    }
    for f in &r.certificate_files {
        row("Certificate", f.display().to_string());
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
        format!("{}:{}", host.source.file.display(), host.source.line),
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

fn status_line(s: Styles) -> Line<'static> {
    let idle = s.theme.status.idle;
    Line::from(vec![
        Span::styled("● ", s.status(idle)),
        Span::styled("Disconnected", s.secondary()),
        Span::styled("   Enter to connect", s.muted()),
    ])
}
