//! Modal rendering for prompts and the changed-key alert.
//!
//! Contract (ui-design-premium): title says what and where, body explains,
//! actions sit bottom-right with the primary one last and highlighted.

use portico_core::events::PromptKind;
use portico_core::vault::MIN_PASSPHRASE_CHARS;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::dialog::{
    Answers, ChangedKeyAlert, ConfirmDialog, Dialog, Field, PromptDialog, SecretDialog,
    SecretPurpose,
};
use crate::theme::Styles;
use crate::ui::fmt::display_path;

/// Fixed mask: shows that something was typed, never how much.
const MASK: &str = "••••••••";
/// Width of an input field's surface.
const FIELD_WIDTH: usize = 34;
/// Modal widths.
const PROMPT_WIDTH: u16 = 74;
const ALERT_WIDTH: u16 = 80;

pub(crate) fn render(dialog: &Dialog, s: Styles, tick: u64, frame: &mut Frame<'_>) {
    match dialog {
        Dialog::Prompt(p) => render_prompt(p, s, frame),
        Dialog::ChangedKey(a) => render_changed(a, s, frame),
        Dialog::Secret(d) => render_secret(d, s, tick, frame),
        Dialog::Confirm(c) => render_confirm(c, s, frame),
    }
}

/// Width of a yes / no question.
const CONFIRM_WIDTH: u16 = 56;

/// Width of an armed confirmation (a longer explanation).
const ARMED_CONFIRM_WIDTH: u16 = 66;

/// Width of an information dialog with an action (the update view).
const INFO_CONFIRM_WIDTH: u16 = 74;

fn render_confirm(c: &ConfirmDialog, s: Styles, frame: &mut Frame<'_>) {
    let width = match c.answers {
        // A release page link fits on one line.
        Answers::Close => INFO_CONFIRM_WIDTH,
        Answers::YesNo if c.armed.is_some() => ARMED_CONFIRM_WIDTH,
        // A question that can wait (the update check) stays compact even
        // though it is armed.
        Answers::YesNo | Answers::YesNoLater { .. } => CONFIRM_WIDTH,
    }
    .min(frame.area().width);
    // Wrapped here, word by word, so the height is exact.
    let inner = usize::from(width.saturating_sub(6)).max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(subject) = &c.subject {
        lines.push(Line::from(Span::styled(
            crate::ui::fmt::truncate(subject, inner),
            s.title(),
        )));
        lines.push(Line::from(""));
    }
    lines.extend(
        c.body
            .split('\n')
            .flat_map(|paragraph| crate::ui::fmt::wrap(paragraph, inner))
            .map(|l| Line::from(Span::styled(l, s.secondary()))),
    );
    if let Some(note) = &c.note {
        lines.push(Line::from(""));
        lines.extend(
            crate::ui::fmt::wrap(note, inner)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, s.muted()))),
        );
    }
    lines.push(Line::from(""));
    // An armed question: `y` stays muted until it is taken (the app wakes
    // at that instant to redraw it in the accent; no ticking meanwhile).
    let mut keys = c.other_hints();
    if c.ready(std::time::Instant::now()) {
        keys.push(("y", c.confirm));
        lines.push(footer(s, &keys));
    } else {
        let mut spans = Vec::new();
        for (key, label) in keys {
            spans.push(Span::styled(format!(" {key} "), s.key_hint()));
            spans.push(Span::styled(format!(" {label}   "), s.muted()));
        }
        spans.push(Span::styled(" y ", s.key_hint()));
        spans.push(Span::styled(format!(" {}", c.confirm), s.muted()));
        lines.push(Line::from(spans).right_aligned());
    }
    let area = modal(frame, width, wrapped_height(&lines, width));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                // Accent for what needs an answer now; a question that can
                // be put off stays on the subtle border.
                .border_style(if matches!(c.answers, Answers::YesNoLater { .. }) {
                    s.border()
                } else {
                    s.border_focus()
                })
                .title(Line::from(Span::styled(
                    format!(" {} ", c.title),
                    s.title(),
                )))
                .padding(Padding::new(2, 2, 1, 0))
                .style(s.overlay()),
        ),
        area,
    );
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

fn kv(s: Styles, width: usize, k: &str, v: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:<width$}"), s.muted()),
        Span::styled(v, s.title()),
    ])
}

/// Actions, right-aligned; the last one is the primary action.
fn footer(s: Styles, actions: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    let last = actions.len().saturating_sub(1);
    for (i, (k, label)) in actions.iter().enumerate() {
        if i == last {
            spans.push(Span::styled(format!(" {k} "), s.key_primary()));
            spans.push(Span::styled(format!(" {label}"), s.accent()));
        } else {
            spans.push(Span::styled(format!(" {k} "), s.key_hint()));
            spans.push(Span::styled(format!(" {label}   "), s.muted()));
        }
    }
    Line::from(spans).right_aligned()
}

// One arm per prompt kind; splitting it would scatter the layout.
#[allow(clippy::too_many_lines)]
fn render_prompt(p: &PromptDialog, s: Styles, frame: &mut Frame<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let alias = p.alias.clone();
    let on = |what: &str| match &alias {
        Some(a) => format!(" {what} · {a} "),
        None => format!(" {what} "),
    };
    let (title, actions): (String, Vec<(&str, &str)>) = match &p.kind {
        PromptKind::UnknownHostKey {
            host,
            port,
            algorithm,
            fingerprint,
            other_algorithms,
            sends_remembered_password,
            ..
        } => {
            host_key_lines(
                p,
                s,
                &mut lines,
                &HostKeyShown {
                    host,
                    port: *port,
                    algorithm,
                    fingerprint,
                    other_algorithms,
                    sends_remembered_password: *sends_remembered_password,
                },
            );
            if other_algorithms.is_empty() {
                (
                    on("New host key"),
                    vec![("n / Esc", "refuse"), ("y", "trust & remember")],
                )
            } else {
                (
                    on("Unusual host key"),
                    vec![("Esc", "refuse"), ("yes ↵", "trust")],
                )
            }
        }
        PromptKind::Password {
            user,
            host,
            attempt,
            attempts,
        } => {
            lines.push(Line::from(Span::styled(
                format!("Sign in as {user} on {host}."),
                s.secondary(),
            )));
            retry_line(s, &mut lines, "password", *attempt, *attempts);
            lines.push(Line::from(""));
            field_lines(p, s, &mut lines);
            (on("Password"), secret_actions(p))
        }
        PromptKind::KeyPassphrase {
            key_path,
            attempt,
            attempts,
        } => {
            lines.push(Line::from(Span::styled(
                format!("Unlock {}", display_path(key_path)),
                s.secondary(),
            )));
            retry_line(s, &mut lines, "passphrase", *attempt, *attempts);
            lines.push(Line::from(""));
            field_lines(p, s, &mut lines);
            (on("Key passphrase"), secret_actions(p))
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
            field_lines(p, s, &mut lines);
            (
                on("Authentication"),
                vec![("Tab", "next"), ("Esc", "cancel"), ("↵", "submit")],
            )
        }
        _ => (" Question ".to_owned(), vec![("Esc", "cancel")]),
    };
    lines.push(Line::from(""));
    lines.push(footer(s, &actions));

    let area = modal(frame, PROMPT_WIDTH, wrapped_height(&lines, PROMPT_WIDTH));
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

fn secret_actions(p: &PromptDialog) -> Vec<(&'static str, &'static str)> {
    if p.can_remember() && p.focus == p.fields.len() {
        vec![("Space", "toggle"), ("Esc", "cancel"), ("↵", "submit")]
    } else {
        vec![("Tab", "next"), ("Esc", "cancel"), ("↵", "submit")]
    }
}

fn retry_line(s: Styles, lines: &mut Vec<Line<'static>>, what: &str, attempt: u32, attempts: u32) {
    if attempt > 1 {
        lines.push(Line::from(Span::styled(
            format!("Incorrect {what}, try again ({attempt}/{attempts})."),
            s.status(s.theme.status.error),
        )));
    }
}

/// Input fields on a raised surface (masked when secret), then the
/// "remember" toggle.
fn field_lines(p: &PromptDialog, s: Styles, lines: &mut Vec<Line<'static>>) {
    input_lines(&p.fields, p.focus, s, lines);
    if p.can_remember() {
        lines.push(Line::from(""));
        let mark = if p.remember { "■" } else { "□" };
        let style = if p.focus == p.fields.len() {
            s.accent()
        } else {
            s.secondary()
        };
        lines.push(Line::from(Span::styled(
            format!("{mark} Remember in {}", p.remember_in),
            style,
        )));
    }
}

fn input_lines(fields: &[Field], focus: usize, s: Styles, lines: &mut Vec<Line<'static>>) {
    let label_width = fields
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let surface = s.key_hint();
    for (i, field) in fields.iter().enumerate() {
        let focused = focus == i;
        let shown = if field.masked {
            if field.value.is_empty() {
                String::new()
            } else {
                MASK.to_owned()
            }
        } else {
            crate::ui::fmt::truncate(&field.value, FIELD_WIDTH - 2)
        };
        let cursor = if focused { "▏" } else { " " };
        let pad = FIELD_WIDTH.saturating_sub(shown.chars().count() + 2);
        lines.push(Line::from(vec![
            Span::styled(format!("{:<label_width$}", field.label), s.muted()),
            Span::styled(format!(" {shown}"), surface),
            Span::styled(cursor, s.accent().patch(surface)),
            Span::styled(" ".repeat(pad), surface),
        ]));
    }
}

/// `At least 8 characters   5 / 8 min` under a new passphrase, from the
/// start. `typed` is already capped at the minimum (the length of a
/// secret is never shown beyond that).
pub(crate) fn passphrase_rule(s: Styles, typed: usize) -> Line<'static> {
    let min = MIN_PASSPHRASE_CHARS;
    let mut spans = vec![Span::styled(
        format!("At least {min} characters"),
        s.secondary(),
    )];
    spans.push(Span::styled(format!("   {typed} / {min} min"), s.muted()));
    if typed >= min {
        spans.push(Span::styled(" ✓", s.muted()));
    }
    Line::from(spans)
}

/// Why the vault is asked for after a host save (`SecretDialog::host_save`).
const HOST_SAVE_CREATE: &str = "This host's password is remembered in the Portico vault, encrypted under a master passphrase. Create it to save the host.";
const HOST_SAVE_UNLOCK: &str =
    "This host's password is remembered in the Portico vault. Unlock it to save the host.";

/// Vault unlock / create / change, and a key passphrase for the agent.
/// Explanation lines, then (title, primary action, cancel action).
fn secret_intro(
    d: &SecretDialog,
    s: Styles,
    lines: &mut Vec<Line<'static>>,
) -> (&'static str, &'static str, &'static str) {
    let text = |lines: &mut Vec<Line<'static>>, t: &str, style| {
        lines.push(Line::from(Span::styled(t.to_owned(), style)));
    };
    match &d.purpose {
        // A host's password waits for the vault: "not now" goes back to
        // the form, the password still typed.
        SecretPurpose::VaultUnlock { .. } if d.host_save => {
            text(lines, HOST_SAVE_UNLOCK, s.secondary());
            (" Unlock the vault ", "unlock & save", "not now")
        }
        SecretPurpose::VaultCreate { .. } if d.host_save => {
            text(lines, HOST_SAVE_CREATE, s.secondary());
            text(
                lines,
                "It cannot be recovered if forgotten.",
                s.status(s.theme.status.warning),
            );
            (" Create the vault ", "create & save", "not now")
        }
        SecretPurpose::VaultUnlock { startup } => {
            text(
                lines,
                "Your remembered passwords and passphrases are in the Portico vault.",
                s.secondary(),
            );
            if *startup {
                text(
                    lines,
                    "Esc continues without it: secrets will not be remembered or filled in.",
                    s.muted(),
                );
            }
            (
                " Unlock the vault ",
                "unlock",
                if *startup {
                    "continue without"
                } else {
                    "cancel"
                },
            )
        }
        SecretPurpose::VaultCreate { offered } => {
            if *offered {
                text(
                    lines,
                    "This system has no keyring, so that secret could not be remembered.",
                    s.secondary(),
                );
                text(
                    lines,
                    "Create a Portico vault to remember secrets from now on, encrypted under a master passphrase.",
                    s.secondary(),
                );
            } else {
                text(
                    lines,
                    "Secrets you choose to remember are encrypted under this master passphrase.",
                    s.secondary(),
                );
            }
            text(
                lines,
                "It cannot be recovered if forgotten.",
                s.status(s.theme.status.warning),
            );
            (
                " Create the vault ",
                "create",
                if *offered { "not now" } else { "cancel" },
            )
        }
        SecretPurpose::VaultChange => {
            text(
                lines,
                "Re-encrypts the vault under a new master passphrase.",
                s.secondary(),
            );
            (" Vault passphrase ", "change", "cancel")
        }
        SecretPurpose::AgentPassphrase {
            key_name, lifetime, ..
        } => {
            text(
                lines,
                &format!("{key_name} is encrypted. Its passphrase unlocks it for ssh-agent."),
                s.secondary(),
            );
            if let Some(l) = lifetime {
                text(
                    lines,
                    &format!(
                        "The agent forgets it after {}.",
                        crate::keys::duration_words(l.as_secs())
                    ),
                    s.muted(),
                );
            }
            (" Key passphrase · ssh-agent ", "add", "cancel")
        }
    }
}

/// Vault unlock / create / change, and a key passphrase for the agent.
fn render_secret(d: &SecretDialog, s: Styles, tick: u64, frame: &mut Frame<'_>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (title, primary, cancel) = secret_intro(d, s, &mut lines);
    let text = |lines: &mut Vec<Line<'static>>, t: &str, style| {
        lines.push(Line::from(Span::styled(t.to_owned(), style)));
    };
    lines.push(Line::from(""));
    let first_field = lines.len();
    input_lines(&d.fields, d.focus, s, &mut lines);
    // The rule sits right under the new passphrase, aligned with it.
    if let Some((index, typed)) = d.new_passphrase_progress() {
        let label_width = d
            .fields
            .iter()
            .map(|f| f.label.chars().count())
            .max()
            .unwrap_or(0)
            + 3;
        let mut rule = passphrase_rule(s, typed);
        rule.spans.insert(0, Span::raw(" ".repeat(label_width)));
        let at = (first_field + index + 1).min(lines.len());
        lines.insert(at, rule);
    }
    if d.busy {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", crate::ui::spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Checking…", s.secondary()),
        ]));
    } else if let Some(e) = &d.error {
        lines.push(Line::from(""));
        let count = match d.failures {
            0 => String::new(),
            1 => " · 1 failed attempt".to_owned(),
            n => format!(" · {n} failed attempts"),
        };
        // The key path is already in the text above: keep it short.
        let message = if e.code == "key_wrong_passphrase" {
            "wrong passphrase"
        } else {
            e.message.as_str()
        };
        lines.push(Line::from(Span::styled(
            format!("{}{count}", crate::ui::fmt::sentence(message)),
            s.status(s.theme.status.error),
        )));
        if let Some(h) = &e.hint {
            text(&mut lines, h, s.secondary());
        }
    }
    lines.push(Line::from(""));
    let actions = if d.fields.len() > 1 {
        vec![("Tab", "next"), ("Esc", cancel), ("↵", primary)]
    } else {
        vec![("Esc", cancel), ("↵", primary)]
    };
    lines.push(footer(s, &actions));
    // Word wrapping can take a row more than the estimate: keep the
    // actions visible.
    let height = wrapped_height(&lines, PROMPT_WIDTH).saturating_add(1);
    let area = modal(frame, PROMPT_WIDTH, height);
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
/// What an unknown-host-key prompt shows.
struct HostKeyShown<'a> {
    host: &'a str,
    port: u16,
    algorithm: &'a str,
    fingerprint: &'a str,
    other_algorithms: &'a [String],
    /// A remembered password, not yet tied to a host key, leaves for this
    /// server once the key is accepted (`SECURITY.md`, risk 13).
    sends_remembered_password: bool,
}

/// Same wording as the GUI's host-key dialog: the account the password is
/// remembered for, `user@host:port`.
pub(crate) fn remembered_password_warning(account: &str) -> String {
    format!(
        "The password remembered for {account} will be sent to this server once you accept its key."
    )
}

/// A server in prose: `host:port`, always (the `[host]:port` form is only
/// for `known_hosts` lines).
pub(crate) fn prose_target(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn host_key_lines(
    p: &PromptDialog,
    s: Styles,
    lines: &mut Vec<Line<'static>>,
    shown: &HostKeyShown<'_>,
) {
    let HostKeyShown {
        host,
        port,
        algorithm,
        fingerprint,
        other_algorithms,
        sends_remembered_password,
    } = *shown;
    let target = prose_target(host, port);
    lines.push(Line::from(Span::styled(
        format!("First connection to {target}."),
        s.title(),
    )));
    lines.push(Line::from(Span::styled(
        "Verify this fingerprint with the server's administrator.",
        s.secondary(),
    )));
    lines.push(Line::from(""));
    lines.push(kv(s, 13, "Key type", algorithm.to_owned()));
    lines.push(kv(s, 13, "Fingerprint", fingerprint.to_owned()));
    if sends_remembered_password {
        lines.push(Line::from(""));
        let account = p
            .account
            .clone()
            .unwrap_or_else(|| prose_target(host, port));
        lines.push(Line::from(Span::styled(
            format!("▲ {}", remembered_password_warning(&account)),
            s.status(s.theme.status.warning),
        )));
    }
    if other_algorithms.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "⚠ This host is already known with another key type ({}). This is unusual.",
            other_algorithms.join(", ")
        ),
        s.status(s.theme.status.warning),
    )));
    lines.push(Line::from(vec![
        Span::styled("Type yes to trust it  ", s.secondary()),
        Span::styled(format!(" {:<4}", p.confirm), s.key_hint()),
        Span::styled("▏", s.accent()),
    ]));
}

fn render_changed(a: &ChangedKeyAlert, s: Styles, frame: &mut Frame<'_>) {
    let error = s.theme.status.error;
    let target = prose_target(&a.host, a.port);
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
        kv(s, 13, "Key type", a.algorithm.clone()),
        kv(s, 13, "Expected", a.expected.clone()),
        Line::from(vec![
            Span::styled(format!("{:<13}", "Received"), s.muted()),
            Span::styled(
                a.fingerprint.clone(),
                s.status(error).add_modifier(Modifier::BOLD),
            ),
        ]),
        kv(
            s,
            13,
            "Recorded in",
            format!("{}:{}", display_path(&a.file), a.line),
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Portico refused to connect. If the administrator confirms the new key:",
            s.secondary(),
        )),
        Line::from(vec![
            Span::styled(format!("{:<13}", "Fix"), s.muted()),
            Span::styled(format!("ssh-keygen -R '{target}'"), s.title()),
        ]),
        Line::from(""),
        footer(s, &[("↵ / Esc", "close")]),
    ];
    let area = modal(frame, ALERT_WIDTH, wrapped_height(&lines, ALERT_WIDTH));
    let banner = ratatui::style::Style::new()
        .bg(crate::theme::c(error))
        .fg(crate::theme::c(s.theme.palette.bg_base))
        .add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(s.status(error))
                .title(Line::from(Span::styled(" ⚠ HOST KEY CHANGED ", banner)))
                .padding(Padding::new(2, 2, 1, 0))
                .style(s.overlay()),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use portico_core::events::Prompt;
    use portico_core::ids::PromptId;
    use portico_theme::Theme;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use super::*;

    /// The rows of `dialog` drawn at 80×24, and the foreground of the
    /// first cell of `needle`.
    fn draw(dialog: &Dialog, needle: &str) -> (String, Option<Color>) {
        let s = Styles::new(Theme::default_theme());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(dialog, s, 0, f)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut colour = None;
        let mut rows = Vec::new();
        for y in 0..24u16 {
            let row: String = (0..80u16)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect();
            if row.contains(needle) {
                let before = row.split(needle).next().unwrap().chars().count();
                let x = u16::try_from(before).unwrap();
                colour = buffer.cell((x, y)).map(|c| c.fg);
            }
            rows.push(row);
        }
        (rows.join("\n"), colour)
    }

    fn host_key(sends_remembered_password: bool) -> Dialog {
        let mut dialog = PromptDialog::new(
            Prompt {
                id: PromptId::next(),
                session: None,
                kind: PromptKind::UnknownHostKey {
                    host: "db.example.test".into(),
                    port: 22,
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: "SHA256:abc".into(),
                    other_algorithms: vec![],
                    sends_remembered_password,
                },
            },
            Some("db".into()),
        );
        if sends_remembered_password {
            dialog.account = Some("deploy@db.example.test:22".into());
        }
        Dialog::Prompt(dialog)
    }

    #[test]
    fn host_key_prompt_says_a_remembered_password_will_be_sent() {
        let (screen, colour) = draw(&host_key(true), "▲ The password remembered for");
        // The target as `host:port` in prose, port 22 included.
        assert!(
            screen.contains("First connection to db.example.test:22."),
            "{screen}"
        );
        // The warning names the account.
        assert!(
            screen.contains("▲ The password remembered for deploy@db.example.test:22"),
            "{screen}"
        );
        // Wrapped in the modal: the end of the sentence on the next row.
        assert!(screen.contains("its key."), "{screen}");
        let s = Styles::new(Theme::default_theme());
        assert_eq!(
            colour,
            s.status(s.theme.status.warning).fg,
            "in the warning colour:\n{screen}"
        );
        assert!(screen.contains("trust & remember"), "{screen}");

        let (screen, colour) = draw(&host_key(false), "remembered password");
        assert_eq!(colour, None, "{screen}");
        assert!(screen.contains("SHA256:abc"), "{screen}");
    }
}
