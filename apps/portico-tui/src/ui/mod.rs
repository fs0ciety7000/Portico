//! Pure rendering: `render(&App) -> Frame`. No I/O here.

mod debug;
mod details;
mod dialog;
mod edit_host;
mod files;
pub(crate) mod fmt;
mod form;
mod host_form;
mod hosts;
mod keys;
mod overlays;
mod panes;
mod recordings;
mod snippets;
mod status;
mod statusbar;
mod sync;
mod terminal;
mod tunnels;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Overlay};

/// An overlay panel of at most `width` × `height`: centred between the
/// header and the status bar, its bottom clear of the toast `band` (the
/// rows just above the status bar). Only the bottom moves when the band
/// appears, so the content does not jump.
pub(crate) fn panel_rect(screen: Rect, width: u16, height: u16, band: u16) -> Rect {
    let region = Rect {
        y: screen.y.saturating_add(1),
        height: screen.height.saturating_sub(2),
        ..screen
    };
    let [v] = Layout::vertical([Constraint::Length(height.min(region.height))])
        .flex(ratatui::layout::Flex::Center)
        .areas(region);
    let [mut area] = Layout::horizontal([Constraint::Length(width.min(region.width))])
        .flex(ratatui::layout::Flex::Center)
        .areas(v);
    let limit = region.bottom().saturating_sub(band);
    if area.bottom() > limit {
        area.height = limit.saturating_sub(area.y);
    }
    area
}

/// The REC badge: the error colour, reserved for meaning (a recorded
/// terminal is a sensitive state), in bold.
pub(crate) fn rec_style(s: crate::theme::Styles) -> ratatui::style::Style {
    s.status(s.theme.status.error)
        .add_modifier(ratatui::style::Modifier::BOLD)
}

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
    if let Some(player) = app
        .player
        .as_ref()
        .filter(|_| app.overlay == Some(Overlay::Player))
    {
        // The playback takes the whole body: it is a screen of its own.
        recordings::render_player(player, s, tick, frame, body);
        toast_zone = body.inner(ratatui::layout::Margin::new(1, 1));
    } else if app.focus == crate::app::Focus::Files
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
    } else if let Some(tab) = app.active_tab() {
        panes::render(app, tab, frame, body);
        toast_zone = body.inner(ratatui::layout::Margin::new(1, 1));
    } else {
        let [sidebar, main] =
            Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                .spacing(1)
                .areas(body.inner(ratatui::layout::Margin::new(1, 0)));
        hosts::render(app, frame, sidebar);
        // Toasts stay over the details, never over the `Include` banner.
        let details = details::render(app, frame, main);
        toast_zone = details.inner(ratatui::layout::Margin::new(1, 1));
    }
    // Over a panel, the toast sits between the panel and the status bar,
    // never on the panel's frame or hints: the panel gives way if needed.
    let panel_open = matches!(
        app.overlay,
        Some(
            Overlay::Keys
                | Overlay::Snippets
                | Overlay::Tunnels
                | Overlay::Edit
                | Overlay::HostForm
                | Overlay::Debug
                | Overlay::Recordings
                | Overlay::Sync
                | Overlay::SyncWizard
        )
    );
    let band = match &app.toast {
        Some(t) if panel_open => {
            overlays::ToastBox::new(s, t.kind, &t.shown(), t.hint.as_deref(), status.width).height
        }
        _ => 0,
    };
    let toast_zone = if band > 0 {
        Rect {
            y: status.y.saturating_sub(band),
            height: band,
            ..status
        }
    } else {
        toast_zone
    };
    render_panels(app, frame, band, panel_open);
    // After the panels: their scroll is clamped to what they show, so the
    // bar's keys (`G follow`) match the screen.
    statusbar::render(app, frame, status);
    if app.overlay == Some(Overlay::Help) {
        scrim(s, frame);
    }
    // Help, then toasts: above the panels, so a result is never hidden.
    overlays::render(app, frame, toast_zone);
    if let Some(d) = app.dialogs.front() {
        scrim(s, frame);
        dialog::render(d, s, tick, frame);
    }
}

/// The overlay panels (tunnels, keys, snippets…) over a scrim, clear of
/// the toast `band`.
fn render_panels(app: &mut App, frame: &mut Frame<'_>, band: u16, panel_open: bool) {
    let s = app.styles();
    if panel_open || app.overlay == Some(Overlay::Tunnels) {
        scrim(s, frame);
    }
    if app.overlay == Some(Overlay::Tunnels)
        && let Some(panel) = &mut app.tunnels_panel
    {
        tunnels::render(panel, &app.tunnels, s, app.tick, frame, band);
    }
    match app.overlay {
        Some(Overlay::Keys) => keys::render(app, frame, band),
        Some(Overlay::Snippets) => snippets::render(app, frame, band),
        Some(Overlay::Edit) => edit_host::render(app, frame, band),
        Some(Overlay::HostForm) => host_form::render(app, frame, band),
        Some(Overlay::Debug) => debug::render(app, frame, band),
        Some(Overlay::Recordings) => recordings::render(app, frame, band),
        Some(Overlay::Sync) => sync::render_panel(app, frame, band),
        Some(Overlay::SyncWizard) => sync::render_wizard(app, frame, band),
        _ => {}
    }
}

/// Light themes only: the screen under a modal sinks from the page planes
/// (`bg-base`, `bg-surface`) to `bg-elevated`, the TUI's scrim, so the
/// modal (`bg-overlay`, the lightest plane) stands apart. Dark themes need
/// none: their overlay is already lighter than the page. Idempotent.
fn scrim(s: crate::theme::Styles, frame: &mut Frame<'_>) {
    if !s.theme.is_light() {
        return;
    }
    let p = &s.theme.palette;
    let page = [crate::theme::c(p.bg_base), crate::theme::c(p.bg_surface)];
    let under = crate::theme::c(p.bg_elevated);
    let area = frame.area();
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y))
                && page.contains(&cell.bg)
            {
                cell.set_bg(under);
            }
        }
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
        false,
    ));
    for (i, tab) in app.tabs.iter().enumerate() {
        let (label, dot) = tab_label(app, i, tab);
        // REC when any pane of the tab is recorded (seen from any tab).
        let rec = tab
            .items()
            .into_iter()
            .any(|(_, slot)| app.recorded_in(slot));
        tabs.push(tab_spans(s, &label, Some(dot), app.active == Some(i), rec));
    }

    // Window [first, last] around the active tab within the budget.
    let theme = app.theme.label();
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

/// `1 web-01` (`1 web-01 (3)` with three panes) and its status dot: the
/// tab shows its focused pane, with the same dot as the pane's title.
fn tab_label(
    app: &App,
    index: usize,
    tab: &crate::app_panes::Tab,
) -> (String, (&'static str, portico_theme::Rgb)) {
    let s = app.styles();
    let Some(slot) = tab.focused() else {
        return (format!("{}", index + 1), ("○", s.theme.status.idle));
    };
    let panes::PaneStatus { dot, color, .. } = panes::pane_status(app, slot);
    let alias = fmt::truncate(slot.alias(), 16);
    let label = if tab.len() > 1 {
        format!("{} {alias} ({})", index + 1, tab.len())
    } else {
        format!("{} {alias}", index + 1)
    };
    (label, (dot, color))
}

fn tab_spans(
    s: crate::theme::Styles,
    label: &str,
    dot: Option<(&'static str, portico_theme::Rgb)>,
    active: bool,
    rec: bool,
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
    // `1 alpha ● · REC`: the separator keeps REC apart from the connection
    // dot (two meanings, two marks).
    if rec {
        let (sep, style) = if active {
            (
                s.muted().patch(s.selected()),
                rec_style(s).patch(s.selected()),
            )
        } else {
            (s.muted(), rec_style(s))
        };
        spans.push(Span::styled("· ", sep));
        spans.push(Span::styled("REC ", style));
    }
    let width = spans.iter().map(Span::width).sum();
    (spans, width)
}
