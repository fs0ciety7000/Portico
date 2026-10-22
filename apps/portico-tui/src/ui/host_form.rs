//! "Add host…", "Edit host…", "Duplicate as Portico host…" overlay, and
//! the read-only view of a host Portico does not edit. Inline refusals
//! under their field (message, then hint), the password as a fixed mask,
//! a discreet reminder that Portico never edits `~/.ssh/config`.

use std::fmt::Write as _;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::App;
use crate::form::display_safe;
use crate::host_form::{
    ADD_COMMAND, DUPLICATE_COMMAND, EDIT_COMMAND, FormMode, HostField, HostForm,
    PASSWORD_DESTINATION_NOTE, PasswordMode, origin_label, read_only_hint,
};
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Label column.
const LABEL: usize = 10;
/// Width of the user input (the port shares its row).
const USER_WIDTH: usize = 22;
/// Width of the port input.
const PORT_WIDTH: usize = 7;
/// Where the hosts go (said under every form).
const FOOTER: &str = "Saved in Portico's hosts.conf · Portico never edits ~/.ssh/config";

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let Some(form) = &app.host_form else { return };
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(76);
    let w = usize::from(width.saturating_sub(6));
    let remember_in = if matches!(
        app.vault,
        Some(
            portico_core::vault::VaultStatus::Absent
                | portico_core::vault::VaultStatus::Locked
                | portico_core::vault::VaultStatus::Unlocked
        )
    ) {
        "the Portico vault"
    } else {
        "the system keyring"
    };
    let lines = lines(form, s, app.tick, w, remember_in);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let (title, subject) = match &form.mode {
        FormMode::Create => (ADD_COMMAND, String::new()),
        FormMode::Edit { original } => (EDIT_COMMAND, original.clone()),
        FormMode::ReadOnly { alias, .. } => (EDIT_COMMAND, alias.clone()),
        FormMode::Duplicate { source } => (DUPLICATE_COMMAND, source.clone()),
    };
    let mut title_spans = vec![Span::styled(format!(" {title} "), s.title())];
    if !subject.is_empty() {
        title_spans.push(Span::styled(
            format!("{} ", fmt::truncate(&subject, 32)),
            s.secondary(),
        ));
    }
    let close = if matches!(form.mode, FormMode::ReadOnly { .. }) {
        " Esc close "
    } else {
        " Esc cancel "
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(title_spans))
        .title_bottom(Line::from(Span::styled(close, s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    // One clear column on each side, as the other panels.
    let left = area.x.saturating_sub(1).max(screen.x);
    let right = area.right().saturating_add(1).min(screen.right());
    let margin = Rect {
        x: left,
        width: right.saturating_sub(left),
        ..area
    };
    frame.render_widget(Clear, margin);
    frame.render_widget(Block::new().style(s.base()), margin);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The refusal under `field`: message, then its hint (muted).
fn problem(form: &HostForm, field: Option<HostField>, s: Styles, w: usize) -> Vec<Line<'static>> {
    match &form.error {
        Some((f, e)) if *f == field => {
            let indent = if field.is_some() { LABEL + 1 } else { 0 };
            // Already shaped by `HostForm::refused` (an alias keeps its case).
            let mut lines: Vec<Line<'static>> =
                fmt::wrap(&e.message, w.saturating_sub(indent).max(10))
                    .into_iter()
                    .map(|l| {
                        Line::from(vec![
                            Span::raw(" ".repeat(indent)),
                            Span::styled(l, s.status(s.theme.status.error)),
                        ])
                    })
                    .collect();
            if let Some(hint) = &e.hint {
                lines.extend(
                    fmt::wrap(hint, w.saturating_sub(indent).max(10))
                        .into_iter()
                        .map(|l| {
                            Line::from(vec![
                                Span::raw(" ".repeat(indent)),
                                Span::styled(l, s.muted()),
                            ])
                        }),
                );
            }
            lines
        }
        _ => Vec::new(),
    }
}

/// A muted note after a field, on its own row, aligned with the input.
fn note(text: &str, s: Styles) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(LABEL + 1)),
        Span::styled(text.to_owned(), s.muted()),
    ])
}

fn lines(form: &HostForm, s: Styles, tick: u64, w: usize, remember_in: &str) -> Vec<Line<'static>> {
    match &form.mode {
        FormMode::ReadOnly { alias, origin } => read_only_lines(alias, *origin, s, w),
        FormMode::Duplicate { source } => {
            let mut lines: Vec<Line<'static>> = fmt::wrap(
                &format!(
                    "Copies HostName, User, Port, the key and the jump host of {} into a new host you can edit.",
                    fmt::truncate(source, 40)
                ),
                w,
            )
            .into_iter()
            .map(|l| Line::from(Span::styled(l, s.secondary())))
            .collect();
            lines.push(Line::from(""));
            lines.push(form::input(
                s,
                "New alias",
                LABEL,
                &form.alias,
                true,
                w.saturating_sub(LABEL).min(40),
            ));
            lines.extend(problem(form, Some(HostField::Alias), s, w));
            lines.extend(problem(form, None, s, w));
            lines.push(Line::from(""));
            lines.extend(tail(form, s, tick, w, &[("↵", "duplicate")]));
            lines
        }
        FormMode::Create | FormMode::Edit { .. } => form_lines(form, s, tick, w, remember_in),
    }
}

fn read_only_lines(
    alias: &str,
    origin: portico_core::hosts::HostOrigin,
    s: Styles,
    w: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:<LABEL$}", "Origin"), s.muted()),
        Span::styled(origin_label(origin), s.title()),
        Span::styled(" · read-only", s.muted()),
    ])];
    lines.push(Line::from(""));
    lines.extend(
        fmt::wrap(read_only_hint(origin), w)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, s.secondary()))),
    );
    lines.push(Line::from(""));
    lines.extend(form::hints(
        s,
        &[
            ("D", "duplicate as Portico host"),
            ("t", "tags, group, notes"),
        ],
        true,
        w,
    ));
    lines.extend(
        fmt::wrap(
            &format!(
                "Tags, group and notes of {} live in hosts.toml.",
                fmt::truncate(alias, 32)
            ),
            w,
        )
        .into_iter()
        .map(|l| Line::from(Span::styled(l, s.muted()))),
    );
    lines
}

/// The keys (or the discard question, or the spinner), then the footer.
fn tail(
    form: &HostForm,
    s: Styles,
    tick: u64,
    w: usize,
    keys: &[(&str, &str)],
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if form.busy {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Saving…", s.secondary()),
        ]));
    } else if form.loading {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Reading hosts.conf…", s.secondary()),
        ]));
    } else if form.confirm_discard {
        let warning = s.status(s.theme.status.warning);
        lines.push(Line::from(vec![
            Span::styled("▲ Discard your changes?  ", warning),
            Span::styled(" y ", s.key_primary()),
            Span::styled(" discard  ", s.muted()),
            Span::styled(" any key ", s.key_hint()),
            Span::styled(" keep editing", s.muted()),
        ]));
    } else {
        lines.extend(form::hints(s, keys, true, w));
    }
    for l in fmt::wrap(FOOTER, w) {
        lines.push(Line::from(Span::styled(l, s.muted())));
    }
    lines
}

/// `‹ choice ›  note · 2/5`.
fn picker(
    s: Styles,
    label: &str,
    choice: &str,
    note: &str,
    position: (usize, usize),
    focused: bool,
) -> Line<'static> {
    let (index, count) = position;
    let mut note = note.to_owned();
    if count > 1 {
        if !note.is_empty() {
            note.push_str(" · ");
        }
        let _ = write!(note, "{}/{count}", index + 1);
    }
    form::select(s, label, LABEL, choice, &note, focused)
}

#[allow(clippy::too_many_lines)] // one row per field, in order
fn form_lines(
    form: &HostForm,
    s: Styles,
    tick: u64,
    w: usize,
    remember_in: &str,
) -> Vec<Line<'static>> {
    let field_w = w.saturating_sub(LABEL).min(52);
    let focused = |f: HostField| form.focus == f;
    let mut lines = vec![form::input(
        s,
        "Alias",
        LABEL,
        &form.alias,
        focused(HostField::Alias),
        field_w,
    )];
    lines.extend(problem(form, Some(HostField::Alias), s, w));
    lines.push(form::input(
        s,
        "HostName",
        LABEL,
        &form.hostname,
        focused(HostField::HostName),
        field_w,
    ));
    lines.extend(problem(form, Some(HostField::HostName), s, w));
    // User and port share a row.
    let user = form::input(
        s,
        "User",
        LABEL,
        &form.user,
        focused(HostField::User),
        USER_WIDTH,
    );
    let port = form::input(
        s,
        "  Port ",
        7,
        &form.port,
        focused(HostField::Port),
        PORT_WIDTH,
    );
    let mut spans = user.spans;
    spans.extend(port.spans);
    if form.port.is_empty() {
        spans.push(Span::styled(" 22", s.muted()));
    }
    lines.push(Line::from(spans));
    lines.extend(problem(form, Some(HostField::User), s, w));
    lines.extend(problem(form, Some(HostField::Port), s, w));

    let key = form.keys.get(form.key);
    let key_label = key.map_or("none", |k| k.label.as_str());
    let key_note = match key {
        _ if !form.keys_loaded => "listing keys…".to_owned(),
        Some(k) => k.note.clone(),
        None => String::new(),
    };
    lines.push(picker(
        s,
        "Key",
        &fmt::truncate(key_label, 36),
        &key_note,
        (form.key, form.keys.len()),
        focused(HostField::Key),
    ));
    lines.extend(problem(form, Some(HostField::Key), s, w));
    let jump = form
        .jumps
        .get(form.jump)
        .cloned()
        .flatten()
        .unwrap_or_else(|| "none".into());
    lines.push(picker(
        s,
        "ProxyJump",
        &fmt::truncate(&jump, 32),
        "",
        (form.jump, form.jumps.len()),
        focused(HostField::Jump),
    ));
    lines.extend(problem(form, Some(HostField::Jump), s, w));
    lines.push(Line::from(""));

    lines.push(form::input(
        s,
        "Tags",
        LABEL,
        &form.tags,
        focused(HostField::Tags),
        field_w,
    ));
    if focused(HostField::Tags) && form.error.is_none() {
        lines.push(note("space or comma between tags, # optional", s));
    }
    lines.extend(problem(form, Some(HostField::Tags), s, w));
    lines.push(form::input(
        s,
        "Group",
        LABEL,
        &form.group,
        focused(HostField::Group),
        field_w,
    ));
    lines.extend(problem(form, Some(HostField::Group), s, w));
    lines.push(form::toggle(
        s,
        "Favorite",
        LABEL,
        form.favorite,
        "★ in favorites",
        focused(HostField::Favorite),
    ));
    // Notes: the last line, with the caret; earlier lines are counted.
    let notes: Vec<&str> = form.notes.split('\n').collect();
    let last = notes.last().copied().unwrap_or_default();
    let mut notes_line = form::input(
        s,
        "Notes",
        LABEL,
        &display_safe(last),
        focused(HostField::Notes),
        field_w,
    );
    if notes.len() > 1 {
        notes_line.spans.push(Span::styled(
            format!(" +{} lines", notes.len() - 1),
            s.muted(),
        ));
    }
    lines.push(notes_line);
    if focused(HostField::Notes) && form.error.is_none() {
        lines.push(note("Alt-↵ new line", s));
    }
    lines.extend(problem(form, Some(HostField::Notes), s, w));

    if let FormMode::Edit { .. } = form.mode {
        let what = match form.password_mode {
            PasswordMode::Keep => "the remembered password, if any",
            PasswordMode::Change => "type the new one below",
            PasswordMode::Forget => "removed when you save",
        };
        lines.push(form::select(
            s,
            "Password",
            LABEL,
            form.password_mode.label(),
            what,
            focused(HostField::PasswordMode),
        ));
        if form.password_mode == PasswordMode::Change {
            lines.push(form::secret(
                s,
                "",
                LABEL,
                form.password_typed(),
                focused(HostField::Password),
                field_w.min(30),
            ));
            lines.push(note(&format!("remembered in {remember_in}"), s));
        }
    } else {
        let mut row = form::secret(
            s,
            "Password",
            LABEL,
            form.password_typed(),
            focused(HostField::Password),
            field_w.min(30),
        );
        row.spans.push(Span::styled("  optional", s.muted()));
        lines.push(row);
        if focused(HostField::Password) {
            lines.push(note(&format!("remembered in {remember_in}"), s));
        }
    }
    if form.password_set_or_kept() {
        lines.extend(
            fmt::wrap(
                PASSWORD_DESTINATION_NOTE,
                w.saturating_sub(LABEL + 1).max(10),
            )
            .into_iter()
            .map(|l| note(&l, s)),
        );
    }
    lines.extend(problem(form, Some(HostField::Password), s, w));
    lines.extend(problem(form, Some(HostField::PasswordMode), s, w));
    lines.extend(problem(form, None, s, w));
    lines.push(Line::from(""));
    let mut keys = vec![("↵", "save"), ("Tab", "next field")];
    if form.on_picker() {
        keys.push(("← →", "choose"));
    }
    lines.extend(tail(form, s, tick, w, &keys));
    lines
}
