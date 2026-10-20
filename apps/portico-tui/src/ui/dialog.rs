//! Modal rendering for prompts and the changed-key alert.

use portico_core::events::PromptKind;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::dialog::{ChangedKeyAlert, Dialog, PromptDialog};
use crate::theme::Styles;

/// Fixed mask: shows that something was typed, never how much.
const MASK: &str = "••••••••";

pub(crate) fn render(dialog: &Dialog, s: Styles, frame: &mut Frame<'_>) {
    match dialog {
        Dialog::Prompt(p) => render_prompt(p, s, frame),
        Dialog::ChangedKey(a) => render_changed(a, s, frame),
    }
}

/// `/home/me/.ssh/x` → `~/.ssh/x` (display only).
fn display_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", path.get(home.len()..).unwrap_or_default())
        }
        _ => path.to_owned(),
    }
}

/// Rows needed by `lines` once wrapped inside a modal of `width` columns
/// (borders + horizontal padding of 2 on each side + top padding).
fn wrapped_height(lines: &[Line<'_>], width: u16) -> u16 {
    let inner = usize::from(width.saturating_sub(6)).max(1);
    let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner).max(1)).sum();
    u16::try_from(rows).unwrap_or(u16::MAX).saturating_add(3)
}

fn modal(frame: &mut Frame<'_>, width: u16, height: u16) -> Rect {
    let area = frame.area();
    let [v] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [h] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    frame.render_widget(Clear, h);
    h
}

fn kv(s: Styles, k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<13}"), s.muted()),
        Span::styled(v, s.title()),
    ])
}

fn footer(s: Styles, hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, label) in hints {
        spans.push(Span::styled(format!(" {k} "), s.key_hint()));
        spans.push(Span::styled(format!(" {label}   "), s.muted()));
    }
    Line::from(spans)
}

#[allow(clippy::too_many_lines)] // one layout per prompt kind; mostly formatting
fn render_prompt(p: &PromptDialog, s: Styles, frame: &mut Frame<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (title, hints): (String, Vec<(&str, &str)>) = match &p.kind {
        PromptKind::UnknownHostKey {
            host,
            port,
            algorithm,
            fingerprint,
            other_algorithms,
        } => host_key_lines(
            p,
            s,
            &mut lines,
            host,
            *port,
            algorithm,
            fingerprint,
            other_algorithms,
        ),
        kind => {
            let title = match kind {
                PromptKind::Password { user, host } => format!(" Password · {user}@{host} "),
                PromptKind::KeyPassphrase { key_path } => {
                    lines.push(Line::from(Span::styled(
                        format!("Unlock {}", display_path(key_path)),
                        s.secondary(),
                    )));
                    lines.push(Line::from(""));
                    " Key passphrase ".to_owned()
                }
                PromptKind::KeyboardInteractive {
                    name, instructions, ..
                } => {
                    for text in [name, instructions]
                        .into_iter()
                        .filter(|t| !t.trim().is_empty())
                    {
                        lines.push(Line::from(Span::styled(
                            text.trim().to_owned(),
                            s.secondary(),
                        )));
                    }
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    format!(
                        " {} ",
                        p.alias
                            .clone()
                            .unwrap_or_else(|| "Authentication".to_owned())
                    )
                }
                _ => " Question ".to_owned(),
            };
            for (i, field) in p.fields.iter().enumerate() {
                let focused = p.focus == i;
                let shown = if field.masked {
                    if field.value.is_empty() {
                        String::new()
                    } else {
                        MASK.to_owned()
                    }
                } else {
                    field.value.to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<13}", field.label), s.muted()),
                    Span::styled(shown, s.title()),
                    Span::styled(if focused { "▏" } else { "" }, s.accent()),
                ]));
            }
            if p.can_remember() {
                lines.push(Line::from(""));
                let mark = if p.remember { "■" } else { "□" };
                let style = if p.focus == p.fields.len() {
                    s.accent()
                } else {
                    s.secondary()
                };
                lines.push(Line::from(Span::styled(
                    format!("{mark} Remember in the system keyring"),
                    style,
                )));
            }
            (
                title,
                vec![("↵", "submit"), ("Tab", "next"), ("Esc", "cancel")],
            )
        }
    };
    lines.push(Line::from(""));
    lines.push(footer(s, &hints));

    let area = modal(frame, 74, wrapped_height(&lines, 74));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.border_focus())
                .title(Line::from(Span::styled(title, s.title())))
                .padding(Padding::new(2, 2, 1, 0))
                .style(s.overlay()),
        ),
        area,
    );
}

#[allow(clippy::too_many_arguments)] // the prompt's fields, destructured
fn host_key_lines(
    p: &PromptDialog,
    s: Styles,
    lines: &mut Vec<Line<'static>>,
    host: &str,
    port: u16,
    algorithm: &str,
    fingerprint: &str,
    other_algorithms: &[String],
) -> (String, Vec<(&'static str, &'static str)>) {
    let status = s.theme.status;
    let target = if port == 22 {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    lines.push(Line::from(Span::styled(
        format!("First connection to {target}."),
        s.title(),
    )));
    lines.push(Line::from(Span::styled(
        "Check the fingerprint with the server administrator before trusting it.",
        s.secondary(),
    )));
    lines.push(Line::from(""));
    lines.push(kv(s, "Key type", algorithm.to_owned()));
    lines.push(Line::from(vec![
        Span::styled(format!("{:<13}", "Fingerprint"), s.muted()),
        Span::styled(
            fingerprint.to_owned(),
            s.accent().add_modifier(Modifier::BOLD),
        ),
    ]));
    if other_algorithms.is_empty() {
        return (
            " New host key ".to_owned(),
            vec![("y", "trust & remember"), ("n", "refuse")],
        );
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "⚠ This host is already known with another key type ({}). This is unusual.",
            other_algorithms.join(", ")
        ),
        s.status(status.warning),
    )));
    lines.push(Line::from(vec![
        Span::styled("Type yes to trust it: ", s.secondary()),
        Span::styled(p.confirm.clone(), s.title()),
        Span::styled("▏", s.accent()),
    ]));
    (
        " Unusual host key ".to_owned(),
        vec![("yes ↵", "trust"), ("Esc", "refuse")],
    )
}

fn render_changed(a: &ChangedKeyAlert, s: Styles, frame: &mut Frame<'_>) {
    let error = s.theme.status.error;
    let target = if a.port == 22 {
        a.host.clone()
    } else {
        format!("{}:{}", a.host, a.port)
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("The key presented by {target} does not match known_hosts."),
            s.title(),
        )),
        Line::from(Span::styled(
            "Someone could be intercepting this connection (man-in-the-middle).",
            s.status(error),
        )),
        Line::from(""),
        kv(s, "Key type", a.algorithm.clone()),
        kv(s, "Expected", a.expected.clone()),
        Line::from(vec![
            Span::styled(format!("{:<13}", "Received"), s.muted()),
            Span::styled(
                a.fingerprint.clone(),
                s.status(error).add_modifier(Modifier::BOLD),
            ),
        ]),
        kv(
            s,
            "Recorded in",
            format!("{}:{}", display_path(&a.file), a.line),
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Portico refused to connect. If the change is legitimate, confirm the new key with the administrator, then remove the old line from known_hosts.",
            s.secondary(),
        )),
        Line::from(""),
        footer(s, &[("Esc", "close")]),
    ];
    let area = modal(frame, 80, wrapped_height(&lines, 80));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Thick)
                .border_style(s.status(error))
                .title(Line::from(Span::styled(
                    " ⚠ HOST KEY CHANGED ",
                    s.status(error).add_modifier(Modifier::BOLD),
                )))
                .padding(Padding::new(2, 2, 1, 0))
                .style(s.overlay()),
        ),
        area,
    );
}
