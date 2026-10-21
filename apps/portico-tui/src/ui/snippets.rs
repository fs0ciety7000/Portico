//! Snippets overlay: filter, the snippets that apply to the target host
//! (two-line rows: name and scope; command preview), then the placeholder
//! form or the editor in place of the list.

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Padding, Paragraph};

use crate::app::App;
use crate::keys::Load;
use crate::snippets::{
    EditField, EditForm, FillForm, SCOPES, SnipMode, SnippetsPanel, scope_label,
};
use crate::theme::Styles;
use crate::ui::{fmt, form, spinner};

/// Rows of the command box in the editor.
const COMMAND_ROWS: usize = 4;

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>) {
    let s = app.styles();
    let tick = app.tick;
    let Some(panel) = &mut app.snippets_panel else {
        return;
    };
    let screen = frame.area();
    let width = screen.width.saturating_sub(2).min(100);
    let height = screen.height.saturating_sub(2).min(32);
    let [v] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(screen);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(v);
    let target = match &panel.alias {
        Some(alias) => format!("· {} ", fmt::truncate(alias, 40)),
        None => "· all hosts ".to_owned(),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(s.border_focus())
        .title(Line::from(vec![
            Span::styled(" Snippets ", s.title()),
            Span::styled(target, s.muted()),
        ]))
        .title_bottom(Line::from(Span::styled(" Esc close ", s.muted())).right_aligned())
        .padding(Padding::new(2, 2, 1, 0))
        .style(s.overlay());
    frame.render_widget(Clear, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let w = usize::from(inner.width);
    let hints = panel.hints();

    match &panel.mode {
        SnipMode::Fill(f) => {
            let lines = fill_lines(f, panel.alias.as_deref(), s, w);
            with_footer(frame, inner, lines, footer(s, Vec::new(), &hints, w, true));
        }
        SnipMode::Edit(f) => {
            let lines = edit_lines(f, s, tick, w);
            with_footer(frame, inner, lines, footer(s, Vec::new(), &hints, w, true));
        }
        _ => browse(panel, s, tick, frame, inner, &hints),
    }
}

fn with_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    top: Vec<Line<'static>>,
    bottom: Vec<Line<'static>>,
) {
    let height = u16::try_from(bottom.len()).unwrap_or(u16::MAX);
    let [a, b] = Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(area);
    frame.render_widget(Paragraph::new(top), a);
    frame.render_widget(Paragraph::new(bottom), b);
}

fn footer(
    s: Styles,
    extra: Vec<Line<'static>>,
    hints: &[(&'static str, &'static str)],
    width: usize,
    modal: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    lines.extend(extra);
    let hints: Vec<_> = hints
        .iter()
        .copied()
        .filter(|(k, l)| !(*k == "Esc" && *l == "close"))
        .collect();
    lines.extend(form::hints(s, &hints, modal, width));
    lines
}

fn browse(
    panel: &mut SnippetsPanel,
    s: Styles,
    tick: u64,
    frame: &mut Frame<'_>,
    area: Rect,
    hints: &[(&'static str, &'static str)],
) {
    let w = usize::from(area.width);
    let visible = panel.visible();
    let in_scope = panel.in_scope();
    let mut head = vec![{
        let mut line = form::input(
            s,
            "Filter",
            8,
            &panel.filter,
            true,
            w.saturating_sub(24).min(36),
        );
        if panel.all.ready().is_some() {
            line.spans.push(Span::styled(
                format!("  {} of {in_scope}", visible.len()),
                s.muted(),
            ));
        }
        line
    }];
    if let Some(why) = &panel.cannot_run {
        for (i, l) in fmt::wrap(why, w.saturating_sub(2)).into_iter().enumerate() {
            let lead = if i == 0 { "▲ " } else { "  " };
            head.push(Line::from(vec![
                Span::styled(lead, s.status(s.theme.status.warning)),
                Span::styled(l, s.secondary()),
            ]));
        }
    }
    head.push(Line::from(""));

    let mut extra = Vec::new();
    match &panel.mode {
        SnipMode::ConfirmDelete { name, .. } => {
            extra.push(Line::from(vec![
                Span::styled("▲ ", s.status(s.theme.status.warning)),
                Span::styled(
                    format!("Delete {}?", fmt::truncate(name, w.saturating_sub(12))),
                    s.title(),
                ),
            ]));
            extra.push(Line::from(Span::styled(
                "It is removed from snippets.toml.",
                s.secondary(),
            )));
        }
        SnipMode::NeedsRun { error, .. } => {
            extra.extend(form::notice(
                s,
                false,
                &error.message,
                error.hint.as_deref(),
                w,
            ));
        }
        _ => {}
    }
    if let Some(n) = &panel.notice {
        extra.extend(form::notice(s, n.ok, &n.message, n.hint.as_deref(), w));
    }
    let modal = !matches!(panel.mode, SnipMode::Browse);
    let bottom = footer(s, extra, hints, w, modal);

    let head_height = u16::try_from(head.len()).unwrap_or(3);
    let bottom_height = u16::try_from(bottom.len()).unwrap_or(2);
    let [top, list_area, foot] = Layout::vertical([
        Constraint::Length(head_height),
        Constraint::Min(2),
        Constraint::Length(bottom_height),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(head), top);
    frame.render_widget(Paragraph::new(bottom), foot);

    if let Some(state) = list_state(panel, visible.len(), in_scope, s, tick, w) {
        frame.render_widget(state, list_area);
        return;
    }

    let selected = panel.list.selected();
    let items: Vec<ListItem<'static>> = visible
        .iter()
        .enumerate()
        .map(|(i, snippet)| snippet_item(snippet, selected == Some(i), s, w))
        .collect();
    frame.render_stateful_widget(List::new(items), list_area, &mut panel.list);
}

/// Loading, error and empty states in place of the list (`None`: the
/// list has rows).
fn list_state(
    panel: &SnippetsPanel,
    visible: usize,
    in_scope: usize,
    s: Styles,
    tick: u64,
    w: usize,
) -> Option<Paragraph<'static>> {
    let empty = |a: &str, b: &str| {
        Some(Paragraph::new(vec![
            Line::from(Span::styled(a.to_owned(), s.secondary())),
            Line::from(Span::styled(b.to_owned(), s.muted())),
        ]))
    };
    match &panel.all {
        Load::Loading => Some(Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Loading snippets…", s.muted()),
        ]))),
        Load::Failed(e) => Some(Paragraph::new(form::notice(
            s,
            false,
            &e.message,
            e.hint.as_deref(),
            w,
        ))),
        Load::Ready(all) if all.is_empty() => empty(
            "No snippets yet.",
            "Ctrl-N creates one: a command, with {{placeholders}} if needed.",
        ),
        Load::Ready(_) if in_scope == 0 => empty(
            &format!(
                "No snippet applies to {}.",
                panel.alias.as_deref().unwrap_or_default()
            ),
            "Ctrl-N creates one (global, for this host or for a #tag).",
        ),
        Load::Ready(_) if visible == 0 => empty(
            &format!("Nothing matches “{}”.", fmt::truncate(&panel.filter, 30)),
            "Esc clears the filter.",
        ),
        Load::Ready(_) => None,
    }
}

/// `▍ name        paste only · #prod` / `  first line  +2 lines`.
fn snippet_item(
    snippet: &portico_core::snippets::Snippet,
    selected: bool,
    s: Styles,
    w: usize,
) -> ListItem<'static> {
    let marker = if selected {
        Span::styled("▍", s.accent())
    } else {
        Span::raw(" ")
    };
    let scope = scope_label(&snippet.scope);
    let paste_note = if snippet.run { "" } else { "paste only · " };
    let right = format!("{paste_note}{}", fmt::truncate(&scope, 28));
    let name_room = w.saturating_sub(right.chars().count() + 4);
    let name = fmt::truncate(&snippet.name, name_room);
    let pad = w.saturating_sub(name.chars().count() + right.chars().count() + 2);
    let mut command_lines = snippet.command.lines();
    let first = command_lines.next().unwrap_or_default();
    let extra = command_lines.count();
    let extra_label = match extra {
        0 => String::new(),
        1 => "  +1 line".to_owned(),
        n => format!("  +{n} lines"),
    };
    let command_room = w.saturating_sub(3 + extra_label.chars().count());
    let item = ListItem::new(Text::from(vec![
        Line::from(vec![
            marker,
            Span::styled(format!(" {name}"), s.title()),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, s.muted()),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(fmt::truncate(first, command_room), s.secondary()),
            Span::styled(extra_label, s.muted()),
        ]),
    ]));
    if selected {
        item.style(s.selected())
    } else {
        item
    }
}

fn fill_lines(f: &FillForm, alias: Option<&str>, s: Styles, w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("Run ", s.secondary()),
        Span::styled(f.snippet.name.clone(), s.title()),
        Span::styled(
            alias.map(|a| format!(" on {a}")).unwrap_or_default(),
            s.secondary(),
        ),
    ])];
    lines.push(Line::from(""));
    let lw = f
        .values
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(4)
        .min(20)
        + 2;
    for (i, (name, value)) in f.values.iter().enumerate() {
        lines.push(form::input(
            s,
            &fmt::truncate(name, lw - 2),
            lw,
            value,
            f.focus == i,
            w.saturating_sub(lw).min(44),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Will type", s.muted())));
    match f.preview() {
        Ok(rendered) => {
            for l in rendered.lines().take(6) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(fmt::truncate(l, w.saturating_sub(2)), s.title()),
                ]));
            }
            if f.snippet.run {
                lines.push(Line::from(Span::styled("  then Enter", s.muted())));
            }
        }
        Err(_) => lines.push(Line::from(Span::styled("  fill in every value", s.muted()))),
    }
    if let Some(e) = &f.error {
        lines.push(Line::from(""));
        lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
    }
    lines
}

fn edit_lines(f: &EditForm, s: Styles, tick: u64, w: usize) -> Vec<Line<'static>> {
    let lw = 13;
    let field_w = w.saturating_sub(lw).min(56);
    let title = if f.id.is_empty() {
        "New snippet"
    } else {
        "Edit snippet"
    };
    let mut lines = vec![
        Line::from(Span::styled(title, s.secondary())),
        Line::from(""),
    ];
    lines.push(form::input(
        s,
        "Name",
        lw,
        &f.name,
        f.focus == EditField::Name,
        field_w,
    ));
    if let Some(p) = f.problem(EditField::Name) {
        lines.extend(form::problem(s, lw, p, w));
    }
    // The command box: its last lines, the caret on the last one.
    let command_lines: Vec<&str> = f.command.split('\n').collect();
    let skip = command_lines.len().saturating_sub(COMMAND_ROWS);
    let focused = f.focus == EditField::Command;
    for (i, l) in command_lines.iter().enumerate().skip(skip) {
        let label = if i == skip { "Command" } else { "" };
        let last = i + 1 == command_lines.len();
        lines.push(form::input(s, label, lw, l, focused && last, field_w));
    }
    if let Some(p) = f.problem(EditField::Command) {
        lines.extend(form::problem(s, lw, p, w));
    }
    lines.push(form::input(
        s,
        "Description",
        lw,
        &f.description,
        f.focus == EditField::Description,
        field_w,
    ));
    let scope_note = match f.scope {
        0 => "every host",
        1 => "these host aliases",
        _ => "hosts with these tags",
    };
    lines.push(form::select(
        s,
        "Applies to",
        lw,
        SCOPES.get(f.scope).copied().unwrap_or("Global"),
        scope_note,
        f.focus == EditField::Scope,
    ));
    if f.scope != 0 {
        lines.push(form::input(
            s,
            if f.scope == 1 { "Hosts" } else { "Tags" },
            lw,
            &f.scope_values,
            f.focus == EditField::ScopeValues,
            field_w,
        ));
        if let Some(p) = f.problem(EditField::ScopeValues) {
            lines.extend(form::problem(s, lw, p, w));
        }
    }
    lines.push(form::toggle(
        s,
        "Run",
        lw,
        f.run,
        if f.run {
            "press Enter after typing it"
        } else {
            "only type it (you press Enter)"
        },
        f.focus == EditField::Run,
    ));
    lines.push(Line::from(Span::styled(
        "{{name}} in the command asks for a value each time.",
        s.muted(),
    )));
    if f.busy {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", spinner(tick)),
                s.status(s.theme.status.connecting),
            ),
            Span::styled("Saving…", s.secondary()),
        ]));
    }
    if let Some(e) = &f.error {
        lines.extend(form::notice(s, false, &e.message, e.hint.as_deref(), w));
    }
    lines
}
