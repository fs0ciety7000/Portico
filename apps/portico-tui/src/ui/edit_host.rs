//! "Edit <alias>" overlay: tags (with chips), group, notes, a discreet
//! reminder of where they live, inline errors with their hint.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::App;
use crate::edit_host::{EditField, EditHostPanel, split_tags};
use crate::form::display_safe;
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Label column.
const LABEL: usize = 8;
/// Rows of the notes box.
const NOTES_ROWS: usize = 5;

pub(crate) fn render(app: &App, frame: &mut Frame<'_>, band: u16) {
    let s = app.styles();
    let Some(panel) = &app.edit_panel else { return };
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(76);
    let w = usize::from(width.saturating_sub(6));
    let lines = lines(panel, s, app.tick, w);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    let area = crate::ui::panel_rect(screen, width, height, band);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Edit ", s.title()),
            Span::styled(
                format!("{} ", fmt::truncate(&panel.alias, 48)),
                s.secondary(),
            ),
        ]))
        .title_bottom(Line::from(Span::styled(" Esc cancel ", s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    // One clear column on each side: the panel's corners never touch the
    // sidebar's border behind it (80×24).
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

fn problem(panel: &EditHostPanel, field: EditField, s: Styles, w: usize) -> Vec<Line<'static>> {
    match &panel.error {
        Some((f, e)) if *f == field => {
            let mut lines = form::problem(s, LABEL + 1, &e.message, w);
            if let Some(hint) = &e.hint {
                lines.extend(
                    fmt::wrap(hint, w.saturating_sub(LABEL + 1))
                        .into_iter()
                        .map(|l| {
                            Line::from(vec![
                                Span::raw(" ".repeat(LABEL + 1)),
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

/// One row of chips under the tags field, `…` when the rest does not fit.
fn chips(tags: Vec<(String, Style)>, s: Styles, w: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(LABEL))];
    let mut used = LABEL;
    for (tag, style) in tags {
        let chip = format!(" {tag} ");
        let need = chip.chars().count() + 1;
        if used + need > w {
            spans.push(Span::styled("…", s.muted()));
            break;
        }
        used += need;
        spans.push(Span::styled(chip, style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn lines(panel: &EditHostPanel, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let field_w = w.saturating_sub(LABEL).min(60);
    let mut lines = vec![form::input(
        s,
        "Tags",
        LABEL,
        &panel.tags,
        panel.focus == EditField::Tags,
        field_w,
    )];
    // Chips as the engine will store them, or why it would refuse.
    match panel.chips() {
        Ok(tags) if tags.is_empty() => lines.push(Line::from(vec![
            Span::raw(" ".repeat(LABEL)),
            Span::styled("space or comma between tags, # optional", s.muted()),
        ])),
        Ok(tags) => lines.push(chips(
            tags.into_iter()
                .map(|tag| (format!("#{tag}"), s.key_hint()))
                .collect(),
            s,
            w,
        )),
        // The typed tags, the refused one marked, then why.
        Err(e) => {
            if let Some(bad) = panel.bad_tag() {
                let typed = split_tags(&panel.tags)
                    .iter()
                    .enumerate()
                    .map(|(i, tag)| {
                        let style = if i == bad {
                            s.key_hint().patch(s.status(s.theme.status.error))
                        } else {
                            s.muted()
                        };
                        (fmt::truncate(&display_safe(tag), 24), style)
                    })
                    .collect();
                lines.push(chips(typed, s, w));
            }
            if panel.error.is_none() {
                lines.extend(form::problem(s, LABEL + 1, &e.message, w));
            }
        }
    }
    lines.extend(problem(panel, EditField::Tags, s, w));
    lines.push(Line::from(""));
    lines.push(form::input(
        s,
        "Group",
        LABEL,
        &panel.group,
        panel.focus == EditField::Group,
        field_w,
    ));
    lines.extend(problem(panel, EditField::Group, s, w));
    lines.push(Line::from(""));
    // The notes box: its last lines, the caret on the last one.
    let notes: Vec<&str> = panel.notes.split('\n').collect();
    let skip = notes.len().saturating_sub(NOTES_ROWS);
    let focused = panel.focus == EditField::Notes;
    for i in 0..NOTES_ROWS.min(notes.len().max(1)).max(3) {
        let index = skip + i;
        let label = if i == 0 { "Notes" } else { "" };
        let text = notes.get(index).copied().unwrap_or_default();
        let last = index + 1 == notes.len();
        lines.push(form::input(s, label, LABEL, text, focused && last, field_w));
    }
    if focused {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(LABEL)),
            Span::styled("↵ new line · Ctrl-S save", s.muted()),
        ]));
    }
    lines.extend(problem(panel, EditField::Notes, s, w));
    lines.push(Line::from(""));
    if panel.busy {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Saving…", s.secondary()),
        ]));
    } else {
        // `Esc cancel` is in the border: said once.
        lines.extend(form::hints(
            s,
            &[("Ctrl-S", "save"), ("Tab", "next field")],
            true,
            w,
        ));
    }
    for l in fmt::wrap(
        "Portico never edits ~/.ssh/config: these live in hosts.toml",
        w,
    ) {
        lines.push(Line::from(Span::styled(l, s.muted())));
    }
    lines
}
