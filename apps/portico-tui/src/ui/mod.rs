//! Pure rendering: `render(&App) -> Frame`. No I/O here.

mod details;
mod dialog;
mod files;
pub(crate) mod fmt;
mod form;
mod hosts;
mod keys;
mod overlays;
mod snippets;
mod status;
mod statusbar;
mod terminal;
mod tunnels;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

/// Braille spinner, advanced by `App::tick`.
pub(crate) const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

pub(crate) fn spinner(tick: u64) -> &'static str {
    // `tick % len` is always in bounds.
    let i = usize::try_from(tick % SPINNER.len() as u64).unwrap_or(0);
    SPINNER.get(i).copied().unwrap_or("·")
}

pub(crate) fn render(app: &mut App, frame: &mut Frame<'_>) {
    let s = app.styles();
    frame.render_widget(Block::new().style(s.base()), frame.area());

    let [header, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(app, frame, header);

    // The terminal fills the body edge to edge: fidelity first.
    app.term_size = crate::terminal::GridSize {
        cols: body.width.max(1),
        rows: body.height.max(1),
    };
    let tick = app.tick;
    // Where toasts go: inside the right-hand panel, clear of its borders.
    let toast_zone;
    let link = app
        .files
        .as_ref()
        .map(|f| files::RemoteLink::of(app.session_by_id(f.session), s));
    if app.focus == crate::app::Focus::Files
        && let Some(view) = &mut app.files
    {
        let link = link.unwrap_or(files::RemoteLink::Up);
        toast_zone = files::render(
            view,
            &link,
            s,
            tick,
            frame,
            body.inner(ratatui::layout::Margin::new(1, 0)),
        );
    } else if let Some(view) = app.active_terminal() {
        let focused = app.dialogs.is_empty() && app.overlay.is_none();
        terminal::render(view, focused, s, frame, body);
        toast_zone = body.inner(ratatui::layout::Margin::new(1, 1));
    } else {
        let [sidebar, main] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .spacing(1)
                .areas(body.inner(ratatui::layout::Margin::new(1, 0)));
        hosts::render(app, frame, sidebar);
        details::render(app, frame, main);
        toast_zone = main.inner(ratatui::layout::Margin::new(1, 1));
    }
    statusbar::render(app, frame, status);
    overlays::render(app, frame, toast_zone);
    if app.overlay == Some(crate::app::Overlay::Tunnels)
        && let Some(panel) = &mut app.tunnels_panel
    {
        tunnels::render(panel, &app.tunnels, s, tick, frame);
    }
    match app.overlay {
        Some(crate::app::Overlay::Keys) => keys::render(app, frame),
        Some(crate::app::Overlay::Snippets) => snippets::render(app, frame),
        _ => {}
    }
    if let Some(d) = app.dialogs.front() {
        dialog::render(d, s, tick, frame);
    }
}

/// Brand, then tabs `Hosts │ 1 web-01 ● │ 2 db ●`, theme on the right when
/// it fits. Tabs are windowed around the active one so it is always visible.
fn render_header(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let s = app.styles();
    let width = usize::from(area.width);
    let brand = vec![
        Span::styled(" ╭┴┴╮ ", s.accent()),
        Span::styled("PORTICO", s.title()),
        Span::raw("  "),
    ];
    let brand_width = 15;

    let count = app.inventory.as_ref().map_or(0, |i| i.hosts.len());
    // Each tab: (spans, width).
    let mut tabs: Vec<(Vec<Span<'static>>, usize)> = Vec::new();
    tabs.push(tab_spans(
        s,
        &format!("Hosts {count}"),
        None,
        app.active.is_none(),
    ));
    for (i, view) in app.terminals.iter().enumerate() {
        let st = status::status(app.session_by_id(view.session), s);
        let dead = view.closed || view.ended.is_some();
        let dot = if dead {
            "○"
        } else if st.busy {
            spinner(app.tick)
        } else {
            "●"
        };
        let color = if dead { s.theme.status.idle } else { st.color };
        let label = format!("{} {}", i + 1, fmt::truncate(&view.alias, 16));
        tabs.push(tab_spans(
            s,
            &label,
            Some((dot, color)),
            app.active == Some(i),
        ));
    }

    // Window [first, last] around the active tab within the budget.
    let theme = s.theme.name;
    let active = app.active.map_or(0, |i| i + 1);
    let budget_with_theme = width.saturating_sub(brand_width + theme.chars().count() + 3);
    let budget = width.saturating_sub(brand_width + 1);
    let total: usize = tabs.iter().map(|(_, w)| w).sum();
    let show_theme = total <= budget_with_theme;
    let budget = if show_theme {
        budget_with_theme
    } else {
        budget
    };
    let (mut first, mut last) = (active, active);
    let mut used = tabs.get(active).map_or(0, |(_, w)| *w);
    let width_of = |i: usize| tabs.get(i).map(|(_, w)| *w);
    loop {
        let mut grew = false;
        // Reserve room for the ‹+n / +n› markers.
        if let Some(w) = width_of(last + 1).filter(|w| used + w + 8 <= budget) {
            last += 1;
            used += w;
            grew = true;
        }
        if let Some(w) = first
            .checked_sub(1)
            .and_then(width_of)
            .filter(|w| used + w + 8 <= budget)
        {
            first -= 1;
            used += w;
            grew = true;
        }
        if !grew {
            break;
        }
    }

    let mut spans = brand;
    if first > 0 {
        spans.push(Span::styled(format!("‹+{first} "), s.muted()));
    }
    for (tab, _) in tabs.iter().take(last + 1).skip(first) {
        spans.extend(tab.iter().cloned());
    }
    if last + 1 < tabs.len() {
        spans.push(Span::styled(
            format!(" +{}›", tabs.len() - last - 1),
            s.muted(),
        ));
    }
    if show_theme {
        let used: usize = spans.iter().map(Span::width).sum();
        let pad = width.saturating_sub(used + theme.chars().count() + 2);
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(theme, s.muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(s.chrome()), area);
}

fn tab_spans(
    s: crate::theme::Styles,
    label: &str,
    dot: Option<(&'static str, portico_theme::Rgb)>,
    active: bool,
) -> (Vec<Span<'static>>, usize) {
    let mut spans = vec![Span::styled("│", s.border())];
    if active {
        spans.push(Span::styled("▍", s.accent().patch(s.selected())));
        spans.push(Span::styled(
            format!("{label} "),
            s.title().patch(s.selected()),
        ));
    } else {
        spans.push(Span::styled(format!(" {label} "), s.muted()));
    }
    if let Some((dot, color)) = dot {
        let style = if active {
            s.status(color).patch(s.selected())
        } else {
            s.status(color)
        };
        spans.push(Span::styled(format!("{dot} "), style));
    }
    let width = spans.iter().map(Span::width).sum();
    (spans, width)
}
