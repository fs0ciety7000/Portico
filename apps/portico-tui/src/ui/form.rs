//! Form widgets shared by the keys and snippets panels: labelled input
//! lines on a raised surface, selects, toggles, inline problems and
//! flowing key hints.

use ratatui::text::{Line, Span};

use crate::theme::Styles;
use crate::ui::fmt;

/// Fixed mask for secrets: shows that something was typed, never how much.
pub(crate) const MASK: &str = "••••••••";

/// `Label     [ value▏      ]`. The value keeps its end visible (what is
/// being typed), the caret shows on the focused field.
pub(crate) fn input(
    s: Styles,
    label: &str,
    label_width: usize,
    value: &str,
    focused: bool,
    width: usize,
) -> Line<'static> {
    let surface = s.key_hint();
    let room = width.saturating_sub(2);
    let count = value.chars().count();
    let shown: String = if count > room {
        let tail: String = value.chars().skip(count + 1 - room).collect();
        format!("…{tail}")
    } else {
        value.to_owned()
    };
    let caret = if focused { "▏" } else { " " };
    let pad = width.saturating_sub(shown.chars().count() + 2);
    let label_style = if focused { s.accent() } else { s.muted() };
    Line::from(vec![
        Span::styled(format!("{label:<label_width$}"), label_style),
        Span::styled(format!(" {shown}"), surface),
        Span::styled(caret, s.accent().patch(surface)),
        Span::styled(" ".repeat(pad), surface),
    ])
}

/// A secret input: the fixed mask once something is typed.
pub(crate) fn secret(
    s: Styles,
    label: &str,
    label_width: usize,
    typed: bool,
    focused: bool,
    width: usize,
) -> Line<'static> {
    input(
        s,
        label,
        label_width,
        if typed { MASK } else { "" },
        focused,
        width,
    )
}

/// `Label     ‹ Choice ›  note`.
pub(crate) fn select(
    s: Styles,
    label: &str,
    label_width: usize,
    choice: &str,
    note: &str,
    focused: bool,
) -> Line<'static> {
    let (label_style, arrows) = if focused {
        (s.accent(), s.accent())
    } else {
        (s.muted(), s.muted())
    };
    let mut spans = vec![
        Span::styled(format!("{label:<label_width$}"), label_style),
        Span::styled(" ‹ ", arrows),
        Span::styled(choice.to_owned(), s.title()),
        Span::styled(" ›", arrows),
    ];
    if !note.is_empty() {
        spans.push(Span::styled(format!("  {note}"), s.muted()));
    }
    Line::from(spans)
}

/// `Label     ■ text`.
pub(crate) fn toggle(
    s: Styles,
    label: &str,
    label_width: usize,
    on: bool,
    text: &str,
    focused: bool,
) -> Line<'static> {
    let label_style = if focused { s.accent() } else { s.muted() };
    let mark = if on { "■" } else { "□" };
    Line::from(vec![
        Span::styled(format!("{label:<label_width$}"), label_style),
        Span::styled(
            format!(" {mark} {text}"),
            if focused { s.accent() } else { s.secondary() },
        ),
    ])
}

/// A problem under a field, aligned with the input.
pub(crate) fn problem(
    s: Styles,
    label_width: usize,
    text: &str,
    width: usize,
) -> Vec<Line<'static>> {
    fmt::wrap(
        &fmt::sentence(text),
        width.saturating_sub(label_width).max(10),
    )
    .into_iter()
    .map(|l| {
        Line::from(vec![
            Span::raw(" ".repeat(label_width)),
            Span::styled(l, s.status(s.theme.status.error)),
        ])
    })
    .collect()
}

/// Message (error colour, or success colour) then hint, wrapped.
pub(crate) fn notice(
    s: Styles,
    ok: bool,
    message: &str,
    hint: Option<&str>,
    width: usize,
) -> Vec<Line<'static>> {
    let (icon, color) = if ok {
        ("✓ ", s.theme.status.connected)
    } else {
        ("✕ ", s.theme.status.error)
    };
    let mut lines: Vec<Line<'static>> = fmt::wrap(&fmt::sentence(message), width.saturating_sub(2))
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let lead = if i == 0 { icon } else { "  " };
            Line::from(vec![
                Span::styled(lead, s.status(color)),
                Span::styled(l, if ok { s.title() } else { s.status(color) }),
            ])
        })
        .collect();
    if let Some(hint) = hint {
        lines.extend(
            fmt::wrap(hint, width.saturating_sub(2))
                .into_iter()
                .map(|l| Line::from(vec![Span::raw("  "), Span::styled(l, s.secondary())])),
        );
    }
    lines
}

/// Key caps flowing over as many lines as `width` needs. The first key is
/// the primary action when `primary` (accent cap).
pub(crate) fn hints(
    s: Styles,
    hints: &[(&str, &str)],
    primary: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (i, (key, label)) in hints.iter().enumerate() {
        let need = key.chars().count() + label.chars().count() + 5;
        if used > 0 && used + need > width {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        let cap = if primary && i == 0 {
            s.key_primary()
        } else {
            s.key_hint()
        };
        spans.push(Span::styled(format!(" {key} "), cap));
        spans.push(Span::styled(format!(" {label}  "), s.muted()));
        used += need;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}
