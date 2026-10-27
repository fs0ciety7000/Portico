//! Split panes in the app: rendering, refusals, key routing and session
//! lifetime, the last ones against in-process SSH servers.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::EngineEvent;
use portico_core::events::SessionPhase;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::broadcast;

use super::*;
use crate::app::test_app;
use crate::panes::{MAX_PANES, PaneArea};
use crate::ssh_support::{Behaviour, Env, TestServer, new_key, target_config};

// ── rendering, without sessions ────────────────────────────────────────────

fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect()
        })
        .collect()
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

fn failed(alias: &str) -> PaneSlot {
    PaneSlot::Failed {
        alias: alias.into(),
        session: None,
        error: ErrorSummary {
            code: "connect_failed".into(),
            message: "connection refused by 10.0.0.7:22".into(),
            hint: Some("Is sshd running on the host?".into()),
        },
    }
}

fn opening(app: &mut App, alias: &str, phase: SessionPhase) -> PaneSlot {
    let session = SessionId::next();
    app.sessions.insert(
        alias.into(),
        SessionView {
            id: session,
            phase,
            latency: Some(Duration::from_millis(12)),
        },
    );
    PaneSlot::Opening {
        alias: alias.into(),
        session,
    }
}

/// A tab of `slots` on a `cols`×`rows` screen: the second to the right of
/// the first, the third below the first, the fourth below the second.
fn tab_of(app: &mut App, cols: u16, rows: u16, slots: Vec<PaneSlot>) -> Vec<PaneId> {
    app.term_size = GridSize {
        cols,
        rows: rows - 2,
    };
    let area = app.body_area();
    let mut slots = slots.into_iter();
    let first = PaneId::next();
    let mut tab = Tab::new(first, slots.next().unwrap());
    let mut ids = vec![first];
    for (i, slot) in slots.enumerate() {
        let id = PaneId::next();
        let split = match i {
            0 => Split::Vertical,
            1 => {
                tab.set_focus(first);
                Split::Horizontal
            }
            _ => {
                tab.set_focus(ids[1]);
                Split::Horizontal
            }
        };
        tab.split(area, split, id, slot).unwrap();
        ids.push(id);
    }
    app.tabs.push(tab);
    app.active = Some(app.tabs.len() - 1);
    app.focus = Focus::Terminal;
    ids
}

#[tokio::test]
async fn two_panes_show_titles_divider_and_focus() {
    let mut app = test_app();
    let web = opening(&mut app, "web-01", SessionPhase::Connected);
    let ids = tab_of(&mut app, 80, 24, vec![failed("db-primary"), web]);
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    // Header: the focused pane's alias and the pane count.
    assert!(
        lines[0].contains("2 web-01 (2)") || lines[0].contains("1 web-01 (2)"),
        "{all}"
    );
    // Titles: alias and status of each pane; the bar marks the focus.
    assert!(lines[1].contains(" db-primary  ● Failed"), "{all}");
    assert!(lines[1].contains("▍web-01  ● Connected · 12 ms"), "{all}");
    assert!(lines[1].contains("2/2"), "{all}");
    // A thin divider down the middle.
    for row in &lines[1..23] {
        assert_eq!(row.chars().nth(39), Some('│'), "{all}");
    }
    // The failed pane: message, hint and close key. Not focused, it says
    // how to get there rather than `↵ retry` (`↵` goes to the focused pane).
    assert!(all.contains("Connection refused by 10.0.0.7:22"), "{all}");
    assert!(all.contains("Is sshd running on the host?"), "{all}");
    assert!(all.contains("focus to retry (Ctrl-Space ←↓↑→)"), "{all}");
    assert!(!all.contains("↵  retry"), "{all}");
    assert!(all.contains("Ctrl-Space x  close"), "{all}");
    // The opening pane says what it waits for.
    assert!(all.contains("Opening a shell on web-01"), "{all}");
    assert!(lines[23].contains("TERMINAL"), "{all}");

    // The focus marking follows the focus.
    app.update(Action::FocusPane(Dir::Left));
    assert_eq!(app.active_tab().map(Tab::focus), ids.first().copied());
    let lines = screen(&mut app, 80, 24);
    assert!(lines[1].starts_with("▍db-primary"), "{}", text(&lines));
    assert!(!lines[1].contains("▍web-01"), "{}", text(&lines));
    // Focused, the failed pane offers ↵ retry, in the status bar too.
    assert!(text(&lines[1..23]).contains("↵  retry"), "{}", text(&lines));
    assert!(!text(&lines).contains("focus to retry"), "{}", text(&lines));
    assert!(lines[23].contains("↵  retry"), "{}", text(&lines));
    // The prefix bar lists the pane commands first.
    app.update(Action::Prefix);
    let lines = screen(&mut app, 80, 24);
    assert!(lines[23].contains("% \"  split"), "{}", text(&lines));
}

fn buffer(app: &mut App, cols: u16, rows: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    terminal.backend().buffer().clone()
}

#[tokio::test]
async fn dividers_are_drawn_in_border_subtle() {
    let mut app = test_app();
    tab_of(&mut app, 80, 24, vec![failed("a"), failed("b")]);
    let buf = buffer(&mut app, 80, 24);
    let palette = app.styles().theme.palette;
    for y in 1..23 {
        let cell = buf.cell((39, y)).unwrap();
        assert_eq!(cell.symbol(), "│");
        // The stroke colour, on the base surface (not the text colour).
        assert_eq!(cell.fg, crate::theme::c(palette.border_subtle), "row {y}");
        assert_eq!(cell.bg, crate::theme::c(palette.bg_base), "row {y}");
        assert_ne!(cell.fg, crate::theme::c(palette.text_primary));
    }
    // Same for a horizontal divider and where they join.
    let mut app = test_app();
    let slots = vec![failed("a"), failed("b"), failed("c")];
    tab_of(&mut app, 80, 24, slots);
    let buf = buffer(&mut app, 80, 24);
    let row = (1..23)
        .find(|&y| buf.cell((0, y)).unwrap().symbol() == "─")
        .unwrap();
    for x in 0..40 {
        let cell = buf.cell((x, row)).unwrap();
        assert_eq!(cell.fg, crate::theme::c(palette.border_subtle), "col {x}");
    }
    assert_eq!(buf.cell((39, row)).unwrap().symbol(), "┤");
}

#[tokio::test]
async fn a_failed_pane_reads_the_same_in_its_title_and_its_tab() {
    let mut app = test_app();
    let web = opening(&mut app, "web", SessionPhase::Connected);
    tab_of(&mut app, 80, 24, vec![web, failed("db")]);
    let error = crate::theme::c(app.styles().theme.status.error);
    let buf = buffer(&mut app, 80, 24);
    let lines = screen(&mut app, 80, 24);
    // The focused pane is the failed one (the last split): its title has a
    // filled dot and "Failed", both in status-error.
    let title = &lines[1];
    let at = |needle: &str| {
        let byte = title.find(needle).unwrap();
        u16::try_from(title[..byte].chars().count()).unwrap()
    };
    let dot = at("db  ●") + 4;
    assert_eq!(buf.cell((dot, 1)).unwrap().symbol(), "●");
    assert_eq!(buf.cell((dot, 1)).unwrap().fg, error);
    let label = at("Failed");
    assert_eq!(buf.cell((label, 1)).unwrap().fg, error);
    // The tab: the same filled dot, in the same colour (never the hollow
    // one, kept for "no shell attached").
    let header = &lines[0];
    assert!(header.contains("1 db (2) ●"), "{header}");
    assert!(!header.contains('○'), "{header}");
    let byte = header.find("1 db (2) ●").unwrap();
    let x = u16::try_from(header[..byte].chars().count()).unwrap() + 9;
    assert_eq!(buf.cell((x, 0)).unwrap().fg, error);
    // An unfocused failed pane: the same dot and colour in its title.
    app.update(Action::FocusPane(Dir::Left));
    let buf = buffer(&mut app, 80, 24);
    let lines = screen(&mut app, 80, 24);
    let title = &lines[1];
    let byte = title.find("Failed").unwrap();
    let x = u16::try_from(title[..byte].chars().count()).unwrap();
    assert_eq!(buf.cell((x, 1)).unwrap().fg, error);
}

#[tokio::test]
async fn a_long_alias_keeps_the_counter_clear_of_the_latency() {
    let mut app = test_app();
    let slots = vec![
        opening(&mut app, "alpha", SessionPhase::Connected),
        opening(
            &mut app,
            "analytics-warehouse-replica-02",
            SessionPhase::Connected,
        ),
    ];
    tab_of(&mut app, 60, 24, slots);
    let lines = screen(&mut app, 60, 24);
    let title = &lines[1];
    assert!(title.contains("12 ms 2/2"), "{title}");
    assert!(!title.contains("ms2/2"), "{title}");
    assert!(title.contains('…'), "the alias gives way: {title}");
}

#[tokio::test]
async fn help_from_a_split_tab_opens_on_the_panes_page() {
    let mut app = test_app();
    tab_of(&mut app, 80, 24, vec![failed("a"), failed("b")]);
    app.update(Action::Prefix);
    app.update(Action::ToggleHelp);
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("Panes (Ctrl-Space)"), "{all}");
    assert!(
        all.contains("Split right") && all.contains("Move divider"),
        "{all}"
    );
    assert!(all.contains("Tab page 2/2"), "{all}");
    app.update(Action::CloseHelp);
    // A tab with one pane: the first page, as before.
    let mut app = test_app();
    tab_of(&mut app, 80, 24, vec![failed("a")]);
    app.update(Action::ToggleHelp);
    let lines = screen(&mut app, 80, 24);
    assert!(text(&lines).contains("Tab page 1/2"), "{}", text(&lines));
}

#[tokio::test]
async fn the_prefix_bar_of_a_split_tab_lists_pane_commands_first() {
    let mut app = test_app();
    tab_of(&mut app, 80, 24, vec![failed("a"), failed("b")]);
    app.update(Action::Prefix);
    let lines = screen(&mut app, 80, 24);
    let bar = &lines[23];
    let order = [
        "% \"  split",
        "←↓↑→  pane",
        "x  close pane",
        "z  zoom",
        "?  help",
        "h  hosts",
    ];
    let mut last = 0;
    for hint in order {
        let at = bar.find(hint).unwrap_or_else(|| panic!("{hint}: {bar}"));
        assert!(at >= last, "{hint} out of order: {bar}");
        last = at;
    }
    // At 120 columns, more fits (tabs, then the host split).
    let lines = screen(&mut app, 120, 24);
    assert!(lines[23].contains("1-9  tab"), "{}", lines[23]);
    assert!(lines[23].contains("S  split with host…"), "{}", lines[23]);
}

#[tokio::test]
async fn four_panes_fit_80x24_with_joined_dividers() {
    let mut app = test_app();
    let slots = vec![
        opening(&mut app, "alpha", SessionPhase::Connected),
        opening(&mut app, "beta", SessionPhase::Handshake),
        opening(&mut app, "gamma", SessionPhase::Connected),
        failed("delta"),
    ];
    tab_of(&mut app, 80, 24, slots);
    assert_eq!(app.active_tab().map(Tab::len), Some(4));
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    for alias in ["alpha", "beta", "gamma", "delta"] {
        assert!(all.contains(alias), "{alias}:\n{all}");
    }
    assert!(all.contains("Negotiating"), "{all}");
    assert_eq!(all.matches('▍').count(), 2, "tab and focused pane:\n{all}");
    // Each column is split on its own, at the same height: the dividers
    // cross in the middle.
    assert!(all.contains('┼'), "{all}");
    // Every pane keeps at least 20×5 cells of grid.
    let layout = app.active_tab().unwrap().layout(app.body_area());
    assert_eq!(layout.panes.len(), 4);
    for PaneArea { grid, .. } in &layout.panes {
        assert!(grid.width >= 20 && grid.height >= 5, "{grid:?}");
    }
    // Zoom: one pane, its title says so; no divider.
    app.update(Action::ZoomPane);
    let lines = screen(&mut app, 80, 24);
    assert!(lines[1].contains("zoom 4/4"), "{}", text(&lines));
    assert!(!text(&lines[1..23]).contains('│'), "{}", text(&lines));
    app.update(Action::ZoomPane);
    let lines = screen(&mut app, 80, 24);
    assert!(text(&lines[1..23]).contains('│'));
}

#[tokio::test]
async fn a_failure_shown_in_a_pane_needs_no_toast() {
    let mut app = test_app();
    let slot = opening(&mut app, "web", SessionPhase::Resolving);
    let session = slot.session().unwrap();
    tab_of(&mut app, 80, 24, vec![slot]);
    let error = ErrorSummary {
        code: "connect_failed".into(),
        message: "connection refused".into(),
        hint: Some("Is sshd running?".into()),
    };
    app.update(Action::Engine(EngineEvent::Session {
        id: session,
        event: portico_core::events::SessionEvent::Phase(SessionPhase::Failed {
            error: error.clone(),
        }),
    }));
    assert!(app.toast.is_none(), "{:?}", app.toast);
    // The open request comes back refused: the pane shows the session's
    // error, its hint and ↵ retry.
    app.update(Action::TerminalOpened {
        alias: "web".into(),
        pane: app.active_tab().map(Tab::focus),
        result: Err(ErrorSummary {
            code: "session_ended".into(),
            message: "the session ended before the request could be served".into(),
            hint: None,
        }),
    });
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("Connection refused"), "{all}");
    assert!(all.contains("Is sshd running?"), "{all}");
    assert!(all.contains("↵  retry"), "{all}");
}

#[tokio::test]
async fn refused_splits_say_why() {
    let mut app = test_app();
    tab_of(&mut app, 80, 24, vec![failed("a"), failed("b")]);
    let before = screen(&mut app, 80, 24);
    // The focused pane is 40 columns wide: two halves of 20 and a divider
    // do not fit.
    app.update(Action::SplitPane(Split::Vertical));
    assert!(app.toast.is_none(), "no toast over the panes");
    let note = app.note.clone().unwrap();
    assert_eq!(note.message, "Each pane needs at least 20 columns × 5 rows");
    assert_eq!(note.hint, "Enlarge the window, or close a pane.");
    assert_eq!(app.active_tab().map(Tab::len), Some(2));
    // One line in the status bar, in status-warning; the panes (and their
    // prompt lines) are exactly as before.
    let lines = screen(&mut app, 80, 24);
    assert_eq!(lines[..23], before[..23], "{}", text(&lines));
    assert!(
        lines[23].contains("▲ Each pane needs at least 20 columns × 5 rows · Enlarge"),
        "{}",
        lines[23]
    );
    let buf = buffer(&mut app, 80, 24);
    let x = u16::try_from(lines[23].find('▲').unwrap()).unwrap();
    let warning = crate::theme::c(app.styles().theme.status.warning);
    assert_eq!(buf.cell((x, 23)).unwrap().fg, warning);
    assert_eq!(buf.cell((x + 2, 23)).unwrap().fg, warning);
    // Wide enough, the whole hint shows.
    let lines = screen(&mut app, 120, 24);
    assert!(
        lines[23].contains("· Enlarge the window, or close a pane."),
        "{}",
        lines[23]
    );
    // It goes after about 4 s (one wake-up at the deadline, no ticking).
    assert_eq!(app.next_deadline(), Some(note.until));
    assert!(note.until > Instant::now() + Duration::from_secs(3));
    app.expire(note.until.checked_sub(Duration::from_millis(1)).unwrap());
    assert!(app.note.is_some());
    app.expire(note.until);
    assert!(app.note.is_none());
    assert_eq!(app.next_deadline(), None);
    let lines = screen(&mut app, 80, 24);
    assert!(lines[23].contains("TERMINAL") && !lines[23].contains('▲'));

    // Eight panes on a big screen, then a ninth is refused.
    let mut app = test_app();
    app.term_size = GridSize {
        cols: 400,
        rows: 200,
    };
    let area = app.body_area();
    let mut tab = Tab::new(PaneId::next(), failed("h0"));
    for i in 1..MAX_PANES {
        let split = if i % 2 == 0 {
            Split::Vertical
        } else {
            Split::Horizontal
        };
        tab.split(area, split, PaneId::next(), failed(&format!("h{i}")))
            .unwrap();
    }
    app.tabs.push(tab);
    app.active = Some(0);
    app.focus = Focus::Terminal;
    app.update(Action::SplitPane(Split::Horizontal));
    let note = app.note.clone().unwrap();
    assert_eq!(note.message, "This tab already has 8 panes");
    assert_eq!(note.hint, "Close a pane first.");
    assert!(app.toast.is_none());
    app.note = None;
    app.update(Action::SplitPaneHost);
    assert!(app.note.is_some() && app.pane_pick.is_none());
}

#[tokio::test]
async fn splitting_a_zoomed_pane_unzooms_then_splits() {
    let mut app = test_app();
    tab_of(&mut app, 120, 40, vec![failed("a"), failed("b")]);
    app.update(Action::ZoomPane);
    assert!(app.active_tab().unwrap().zoomed());
    // Room in the split layout: unzoomed, then split.
    let session = opening(&mut app, "c", SessionPhase::Connected);
    let PaneSlot::Opening { session, .. } = session else {
        unreachable!()
    };
    let index = app.active.unwrap();
    app.split_with(index, Split::Horizontal, "c".into(), session);
    let tab = app.active_tab().unwrap();
    assert_eq!(tab.len(), 3);
    assert!(!tab.zoomed());
    assert!(app.note.is_none());
    // No room in the split layout: refused, and the zoom stays.
    let mut app = test_app();
    tab_of(
        &mut app,
        80,
        24,
        vec![failed("a"), failed("b"), failed("c"), failed("d")],
    );
    app.update(Action::ZoomPane);
    app.update(Action::SplitPane(Split::Vertical));
    let tab = app.active_tab().unwrap();
    assert_eq!(tab.len(), 4);
    assert!(tab.zoomed(), "a refusal keeps the zoom");
    assert_eq!(
        app.note.as_ref().map(|n| n.message.as_str()),
        Some("Each pane needs at least 20 columns × 5 rows")
    );
}

#[test]
fn closing_panes_collapses_then_closes_the_tab() {
    let mut app = test_app();
    app.term_size = GridSize { cols: 80, rows: 22 };
    let area = app.body_area();
    let mut tab = Tab::new(PaneId::next(), failed("a"));
    tab.split(area, Split::Vertical, PaneId::next(), failed("b"))
        .unwrap();
    app.tabs.push(tab);
    app.active = Some(0);
    app.focus = Focus::Terminal;
    app.update(Action::ClosePane);
    assert_eq!(app.active_tab().map(Tab::len), Some(1));
    assert_eq!(app.focused_slot().map(PaneSlot::alias), Some("a"));
    app.update(Action::ClosePane);
    assert!(app.tabs.is_empty());
    assert_eq!(app.active, None);
    assert_eq!(app.focus, Focus::Hosts);
}

#[test]
fn picking_a_host_can_be_cancelled() {
    let mut app = test_app();
    app.term_size = GridSize { cols: 80, rows: 22 };
    let pane = PaneId::next();
    app.tabs.push(Tab::new(pane, failed("a")));
    app.active = Some(0);
    app.focus = Focus::Terminal;
    app.update(Action::SplitPaneHost);
    assert_eq!(app.pane_pick.as_ref().map(|p| p.pane), Some(pane));
    assert_eq!(app.focus, Focus::Search, "type the host at once");
    assert_eq!(app.active, None, "the host list is shown");
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("Split with host…"), "{all}");
    assert!(
        lines[23].contains("SPLIT") && lines[23].contains("Esc  cancel"),
        "{all}"
    );
    app.update(Action::CancelPanePick);
    assert_eq!(app.pane_pick, None);
    assert_eq!(app.active, Some(0));
    assert_eq!(app.focus, Focus::Terminal);
}

// ── real sessions (in-process SSH servers) ─────────────────────────────────

struct Lab {
    /// Keeps the throwaway HOME (and the engine's files) alive.
    _env: Env,
    alpha: TestServer,
    beta: TestServer,
    events: broadcast::Receiver<EngineEvent>,
    app: App,
}

/// Two hosts, `alpha` and `beta`, each its own echoing SSH server; key
/// authentication, host keys already trusted (no prompt).
async fn lab() -> Lab {
    let key = new_key();
    let behaviour = || Behaviour {
        keys: vec![key.public_key().clone()],
        pty_echo: true,
        ..Behaviour::default()
    };
    let alpha = TestServer::start(behaviour()).await;
    let beta = TestServer::start(behaviour()).await;
    let identity = "  IdentityFile ~/.ssh/id_test\n";
    let config = format!(
        "{}{}",
        target_config("alpha", alpha.port, identity),
        target_config("beta", beta.port, identity)
    );
    let env = Env::new(&config);
    env.write_key("id_test", &key, None);
    env.trust(&alpha);
    env.trust(&beta);
    let events = env.engine.subscribe();
    let mut app = App::new(env.engine.clone());
    app.clipboard = false;
    app.term_size = GridSize {
        cols: 110,
        rows: 26,
    };
    let inventory = env
        .engine
        .load_hosts()
        .await
        .map_err(|e| e.summary().message);
    app.update(Action::HostsLoaded(inventory));
    Lab {
        _env: env,
        alpha,
        beta,
        events,
        app,
    }
}

impl Lab {
    async fn until(&mut self, done: impl FnMut(&App) -> bool) {
        self.app.pump_until(&mut self.events, done).await;
    }

    /// Selects `alias` in the host list (through the search).
    fn select(&mut self, alias: &str) {
        self.app.update(Action::SearchClear);
        for c in alias.chars() {
            self.app.update(Action::SearchInput(c));
        }
        assert_eq!(
            self.app.selected_host().map(|h| h.alias.as_str()),
            Some(alias)
        );
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.app.update(Action::TerminalKey(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
    }

    fn panes(&self) -> usize {
        self.app.active_tab().map_or(0, Tab::len)
    }
}

fn all_live(app: &App) -> bool {
    app.active_tab()
        .is_some_and(|t| t.items().iter().all(|(_, s)| s.live()))
}

fn received(server: &TestServer) -> String {
    String::from_utf8_lossy(&server.behaviour.received.lock().unwrap()).into_owned()
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("not reached in time");
}

#[tokio::test]
async fn keys_reach_only_the_focused_pane() {
    let mut lab = lab().await;
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;

    // Ctrl-Space S, then `beta` in the list: a pane on another host.
    lab.app.update(Action::SplitPaneHost);
    assert!(lab.app.pane_pick.is_some());
    lab.select("beta");
    // The details say what `↵` does now.
    let all = text(&screen(&mut lab.app, 110, 28));
    assert!(all.contains("↵  split with beta"), "{all}");
    assert!(!all.contains("↵  connect"), "{all}");
    assert!(all.contains("Split with host…"), "{all}");
    lab.app.update(Action::Connect);
    assert!(lab.app.pane_pick.is_none());
    lab.until(|app| app.active_tab().is_some_and(|t| t.len() == 2) && all_live(app))
        .await;
    assert_eq!(lab.app.focused_slot().map(PaneSlot::alias), Some("beta"));

    lab.typed("qw");
    let beta = &lab.beta;
    eventually(|| received(beta).contains("qw")).await;
    assert!(!received(&lab.alpha).contains("qw"), "no broadcast");

    lab.app.update(Action::FocusPane(Dir::Left));
    assert_eq!(lab.app.focused_slot().map(PaneSlot::alias), Some("alpha"));
    lab.typed("zx");
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("zx")).await;
    assert!(!received(&lab.beta).contains("zx"), "no broadcast");
    // A paste goes to the focused pane only, too.
    lab.app.update(Action::Paste("pasted".into()));
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("pasted")).await;
    assert!(!received(&lab.beta).contains("pasted"));

    // Splitting onto `alpha` again reuses its live session.
    lab.app.update(Action::SplitPaneHost);
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.active_tab().is_some_and(|t| t.len() == 3) && all_live(app))
        .await;
    let alpha_sessions: Vec<SessionId> = lab
        .app
        .views()
        .filter(|v| v.alias == "alpha")
        .map(|v| v.session)
        .collect();
    assert_eq!(alpha_sessions.len(), 2);
    assert_eq!(alpha_sessions[0], alpha_sessions[1]);
    assert_eq!(
        lab.alpha.behaviour.connections.load(Ordering::SeqCst),
        1,
        "one transport, two PTY channels"
    );

    // Each pane's PTY follows its own size, once the geometry is still.
    lab.app.sync_terminal_sizes();
    lab.until(|app| !app.resizes_pending()).await;
    let sizes: Vec<(u16, u16)> = lab
        .app
        .views()
        .map(|v| (v.size.cols, v.size.rows))
        .collect();
    assert!(sizes.iter().all(|(c, r)| *c < 110 && *r < 26), "{sizes:?}");

    // Four live panes, drawn on the screen they were laid out for.
    lab.app.update(Action::SplitPane(Split::Horizontal));
    lab.until(|app| app.active_tab().is_some_and(|t| t.len() == 4) && all_live(app))
        .await;
    let lines = screen(&mut lab.app, 110, 28);
    let all = text(&lines);
    assert_eq!(all.matches("alpha  ●").count(), 3, "{all}");
    assert_eq!(all.matches("beta  ●").count(), 1, "{all}");
    assert!(!all.contains("Failed"), "{all}");
    assert_eq!(all.matches('▍').count(), 2, "tab and focused pane:\n{all}");
    assert!(all.contains("welcome"), "{all}");
    assert_eq!(lab.panes(), 4);
}

#[tokio::test]
async fn closing_the_last_pane_of_a_session_disconnects_it() {
    let mut lab = lab().await;
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;
    let session = lab.app.active_terminal().map(|v| v.session).unwrap();

    // `%`: a second pane on the same session (one more channel).
    lab.app.update(Action::SplitPane(Split::Vertical));
    lab.until(|app| app.active_tab().is_some_and(|t| t.len() == 2) && all_live(app))
        .await;
    assert!(lab.app.views().all(|v| v.session == session));

    // Closing one pane closes its channel only: the session lives on.
    lab.app.update(Action::ClosePane);
    let alpha = &lab.alpha;
    eventually(|| alpha.behaviour.closed_channels.load(Ordering::SeqCst) == 1).await;
    assert_eq!(lab.panes(), 1);
    lab.typed("still");
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("still")).await;
    assert!(lab.app.session_alive(session));

    // The last pane: the session is disconnected, as with the last tab.
    lab.app.update(Action::ClosePane);
    assert!(lab.app.tabs.is_empty());
    lab.until(|app| {
        app.session_by_id(session)
            .is_some_and(|s| matches!(s.phase, SessionPhase::Closed { .. }))
    })
    .await;
}

#[tokio::test]
async fn a_pane_whose_connection_closed_reopens_on_enter() {
    let mut lab = lab().await;
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;
    let first = lab.app.active_terminal().map(|v| v.session).unwrap();
    // The session goes down under the pane.
    lab.app.engine.disconnect(first).await.unwrap();
    lab.until(|app| {
        app.focused_slot().is_some_and(|s| !s.live())
            && app
                .session_by_id(first)
                .is_some_and(|s| matches!(s.phase, SessionPhase::Closed { .. }))
    })
    .await;
    assert!(
        lab.app
            .focused_slot()
            .and_then(|s| lab.app.pane_failure(s))
            .is_some_and(|f| f.retry)
    );
    let lines = screen(&mut lab.app, 80, 24);
    assert!(text(&lines).contains("↵ reconnect"), "{}", text(&lines));
    // Other keys do nothing; ↵ reconnects and reopens in place.
    lab.typed("x");
    assert!(lab.app.focused_slot().is_some_and(|s| !s.live()));
    lab.app.update(Action::TerminalKey(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    lab.until(all_live).await;
    assert_eq!(lab.app.tabs.len(), 1);
    let second = lab.app.active_terminal().map(|v| v.session).unwrap();
    assert_ne!(first, second, "a new session");
}

/// A key as the terminal delivers it: through the dispatcher, then applied.
fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let event = crossterm::event::Event::Key(KeyEvent::new(code, modifiers));
    if let Some(action) = app.map_event(event) {
        app.update(action);
    }
}

#[tokio::test]
async fn pane_mode_repeats_arrows_then_hands_keys_back() {
    let mut lab = lab().await;
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;
    lab.app.update(Action::SplitPaneHost);
    lab.select("beta");
    lab.app.update(Action::Connect);
    lab.until(|app| app.active_tab().is_some_and(|t| t.len() == 2) && all_live(app))
        .await;
    let focused = |app: &App| app.focused_slot().map(|s| s.alias().to_owned());
    assert_eq!(focused(&lab.app).as_deref(), Some("beta"));
    let none = KeyModifiers::NONE;

    // Ctrl-Space ←: the focus moves and pane mode starts.
    press(&mut lab.app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    press(&mut lab.app, KeyCode::Left, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("alpha"));
    let window = lab.app.pane_repeat.unwrap();
    assert!(window > Instant::now() + Duration::from_millis(400));
    assert!(window <= Instant::now() + PANE_REPEAT);
    let lines = screen(&mut lab.app, 110, 28);
    assert!(lines[27].contains(" PANE "), "{}", lines[27]);
    assert!(lines[27].contains("←↓↑→  pane"), "{}", lines[27]);
    assert!(
        lines[27].contains("Shift-←↓↑→  move divider"),
        "{}",
        lines[27]
    );

    // Within the window, arrows and Shift+arrows need no prefix.
    press(&mut lab.app, KeyCode::Right, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("beta"));
    press(&mut lab.app, KeyCode::Left, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("alpha"));
    let area = lab.app.body_area();
    let width = |app: &App| {
        app.active_tab()
            .unwrap()
            .layout(area)
            .panes
            .first()
            .map(|p| p.grid.width)
    };
    let before = width(&lab.app);
    press(&mut lab.app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_ne!(width(&lab.app), before, "the divider moved");
    assert!(lab.app.pane_repeat.is_some());

    // Any other key ends the mode and reaches the focused pane's program.
    press(&mut lab.app, KeyCode::Char('q'), none);
    assert!(lab.app.pane_repeat.is_none());
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains('q')).await;
    assert!(!received(&lab.beta).contains('q'));
    // None of the arrows of the mode went to a remote program.
    assert!(!received(&lab.alpha).contains('\u{1b}'));
    assert!(!received(&lab.beta).contains('\u{1b}'));
    let lines = screen(&mut lab.app, 110, 28);
    assert!(lines[27].contains(" TERMINAL "), "{}", lines[27]);

    // Out of the mode, an arrow is the program's again.
    press(&mut lab.app, KeyCode::Left, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("alpha"));
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains('\u{1b}')).await;

    // The window runs out: the next arrow goes to the program as well.
    press(&mut lab.app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    press(&mut lab.app, KeyCode::Right, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("beta"));
    let until = lab.app.pane_repeat.unwrap();
    assert_eq!(lab.app.next_deadline(), Some(until));
    lab.app.expire(until);
    assert!(lab.app.pane_repeat.is_none());
    press(&mut lab.app, KeyCode::Left, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("beta"));
    let beta = &lab.beta;
    eventually(|| received(beta).contains('\u{1b}')).await;
    // A stale window (its wake-up not handled yet) does not count either.
    lab.app.pane_repeat = Instant::now().checked_sub(Duration::from_millis(1));
    press(&mut lab.app, KeyCode::Left, none);
    assert_eq!(focused(&lab.app).as_deref(), Some("beta"));
}

/// SECURITY: a shell that arrives after the user moved to another tab must
/// not take the keyboard (the rest of a line and its Enter would run on
/// the wrong host).
#[tokio::test]
async fn a_late_terminal_never_steals_the_focus() {
    let mut lab = lab().await;
    // Tab A: alpha, live, with the focus.
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;

    // Connecting to beta opens its tab at once, at the action (waiting
    // for its shell), and shows it.
    lab.app.update(Action::GoHome);
    lab.select("beta");
    lab.app.update(Action::Connect);
    assert_eq!(lab.app.tabs.len(), 2);
    assert_eq!(lab.app.active, Some(1));
    assert!(matches!(
        lab.app.focused_slot(),
        Some(PaneSlot::Opening { alias, .. }) if alias == "beta"
    ));
    let all = text(&screen(&mut lab.app, 80, 24));
    assert!(all.contains("Opening a shell on beta"), "{all}");
    // The user goes back to A and types while beta's shell is on its way.
    lab.app.update(Action::GoTab(0));
    lab.typed("ear");
    lab.until(|app| {
        app.tabs
            .get(1)
            .is_some_and(|t| t.items().iter().all(|(_, s)| s.live()))
    })
    .await;
    // Beta's shell filled its pane; A kept the focus.
    assert_eq!(lab.app.active, Some(0));
    assert_eq!(lab.app.focus, Focus::Terminal);
    assert_eq!(lab.app.focused_slot().map(PaneSlot::alias), Some("alpha"));
    lab.typed("ly\r");
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("early")).await;
    assert!(!received(&lab.beta).contains("ear"), "nothing went to beta");

    // A shell delivered without a pane (`TerminalOpened { pane: None }`)
    // lands in a background tab: A keeps the focus and the keys.
    let session = lab.app.session_for("beta").map(|s| s.id).unwrap();
    let handle = lab
        .app
        .engine
        .open_terminal(session, GridSize { cols: 80, rows: 24 }.pty())
        .await
        .unwrap();
    lab.app.update(Action::TerminalOpened {
        alias: "beta".into(),
        pane: None,
        result: Ok(Box::new(handle)),
    });
    assert_eq!(lab.app.tabs.len(), 3);
    assert_eq!(lab.app.active, Some(0));
    assert_eq!(lab.app.focus, Focus::Terminal);
    assert_eq!(lab.app.focused_slot().map(PaneSlot::alias), Some("alpha"));
    lab.typed("still-a");
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("still-a")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !received(&lab.beta).contains("still"),
        "nothing went to beta"
    );
}

// ── scrollback search (phase 9) ────────────────────────────────────────────

/// Presses `key` the way the event loop does (keymap, then update).
fn press_key(app: &mut App, key: KeyEvent) {
    press(app, key.code, key.modifiers);
}

fn press_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

/// The focused pane's whole grid (scrollback and screen) as text.
fn grid_text(app: &App) -> String {
    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Point};
    app.active_terminal().map_or_else(String::new, |view| {
        let term = view.term.lock().unwrap();
        term.bounds_to_string(
            Point::new(term.topmost_line(), Column(0)),
            Point::new(term.bottommost_line(), term.last_column()),
        )
    })
}

fn display_offset(app: &App) -> usize {
    app.active_terminal()
        .map_or(0, |v| v.term.lock().unwrap().grid().display_offset())
}

/// `alpha` connected, with a scrollback: `needle one`, 40 lines, then
/// `Needle two`. The lines are parsed into the pane's grid as remote output
/// would be (the echo server only echoes keys, one packet per key).
async fn searchable_lab() -> Lab {
    use std::fmt::Write as _;
    let mut lab = lab().await;
    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;
    // `welcome` first: the output after it must not interleave with it.
    lab.until(|app| grid_text(app).contains("welcome")).await;
    let mut output = String::from("needle one\r\n");
    for i in 0..40 {
        let _ = write!(output, "output line {i}\r\n");
    }
    output.push_str("Needle two\r\n$ ");
    if let Some(view) = lab.app.active_terminal() {
        let mut term = view.term.lock().unwrap();
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut *term, output.as_bytes());
    }
    lab
}

/// Opens the bar (`Ctrl-Space /`), types `needle`, then `↵` (older).
fn search_needle(app: &mut App) {
    press_key(
        app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
    );
    press_key(app, KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(app.searching());
    press_text(app, "needle");
    press_key(app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

/// Lays the tab out for a `cols`×`rows` screen and draws it.
fn screen_at(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
    app.term_size = GridSize {
        cols,
        rows: rows - 2,
    };
    app.sync_terminal_sizes();
    screen(app, cols, rows)
}

#[tokio::test]
async fn scrollback_search_finds_scrolls_and_keeps_keys_from_the_remote() {
    let mut lab = Box::pin(searchable_lab()).await;
    lab.alpha.behaviour.received.lock().unwrap().clear();

    search_needle(&mut lab.app);
    let count = |app: &App| app.active_terminal().and_then(|v| v.search.as_ref()?.count);
    // Smart case: `needle` finds both; ↵ went from the newest to the older.
    assert_eq!(
        count(&lab.app),
        Some(crate::term_search::Count::Exact {
            index: Some(2),
            total: 2
        })
    );
    assert!(
        display_offset(&lab.app) > 0,
        "scrolled back to `needle one`"
    );
    // Every other key stays in the bar: Ctrl-D, Ctrl-Space twice, a paste.
    press_key(
        &mut lab.app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    );
    for _ in 0..2 {
        press_key(
            &mut lab.app,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
        );
    }
    lab.app.update(Action::Paste("zz\nrm -rf /\n".into()));
    let query = |app: &App| {
        app.active_terminal()
            .and_then(|v| Some(v.search.as_ref()?.query.clone()))
    };
    assert_eq!(query(&lab.app).as_deref(), Some("needlezz"));
    for _ in 0..2 {
        press_key(
            &mut lab.app,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );
    }

    // Editing the query searches again from the bottom: the newest.
    assert_eq!(
        count(&lab.app),
        Some(crate::term_search::Count::Exact {
            index: Some(1),
            total: 2
        })
    );
    press_key(
        &mut lab.app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Drawn at 80 columns (a resize: the same match stays current and
    // shown): the bar on the pane's last row, above the status bar.
    let lines = screen_at(&mut lab.app, 80, 24);
    let all = text(&lines);
    let bar = lines.get(22).cloned().unwrap_or_default();
    assert!(bar.starts_with("▍ Search  needle"), "{all}");
    assert!(bar.trim_end().ends_with("2 of 2"), "{all}");
    assert!(lines.get(23).is_some_and(|l| l.contains("SEARCH")), "{all}");
    assert!(all.contains("needle one"), "{all}");
    // No match: the warning text; the view stays where it was.
    let offset = display_offset(&lab.app);
    press_text(&mut lab.app, "xyz");
    let lines = screen_at(&mut lab.app, 80, 24);
    assert!(
        lines.get(22).is_some_and(|l| l.contains("No match")),
        "{}",
        text(&lines)
    );
    assert_eq!(display_offset(&lab.app), offset);

    // Esc: back to the live view; keys reach the remote again.
    press_key(
        &mut lab.app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(!lab.app.searching());
    assert_eq!(display_offset(&lab.app), 0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        received(&lab.alpha),
        "",
        "nothing sent while the bar was open"
    );
    lab.typed("ok");
    let alpha = &lab.alpha;
    eventually(|| received(alpha) == "ok").await;
}

/// Feeds `output` into the focused pane as remote output would be.
fn feed_pane(app: &App, output: &str) {
    if let Some(view) = app.active_terminal() {
        let mut term = view.term.lock().unwrap();
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut *term, output.as_bytes());
    }
}

fn open_bar(app: &mut App) {
    press_key(
        app,
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
    );
    press_key(app, KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(app.searching());
}

/// The bar owns its whole row: with an empty query, nothing of the grid
/// line under it shows through.
#[tokio::test]
async fn the_empty_search_bar_hides_the_line_under_it() {
    let mut lab = Box::pin(searchable_lab()).await;
    feed_pane(&lab.app, &"x".repeat(70));
    open_bar(&mut lab.app);
    let lines = screen_at(&mut lab.app, 80, 24);
    let bar = lines.get(22).cloned().unwrap_or_default();
    let rest = bar.strip_prefix("▍ Search").unwrap_or("x");
    assert!(
        rest.chars().all(|c| c == ' ') && bar.chars().count() == 80,
        "{}",
        text(&lines)
    );
}

/// A current match on the live view's bottom line is drawn just above the
/// bar, which stays at the bottom.
#[tokio::test]
async fn a_match_under_the_bar_is_drawn_above_it() {
    let mut lab = Box::pin(searchable_lab()).await;
    feed_pane(&lab.app, "pinpoint");
    open_bar(&mut lab.app);
    press_text(&mut lab.app, "pinpoint");
    let lines = screen_at(&mut lab.app, 80, 24);
    let all = text(&lines);
    assert!(
        lines.get(21).is_some_and(|l| l.contains("$ pinpoint")),
        "{all}"
    );
    assert!(
        lines
            .get(22)
            .is_some_and(|l| l.starts_with("▍ Search  pinpoint")),
        "{all}"
    );
}

/// Text captures of the bar for the report and the design review:
/// `cargo test -p portico-tui search_bar_captures -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "prints captures"]
#[allow(clippy::print_stdout)] // the captures are the point of this test
async fn search_bar_captures() {
    let mut lab = Box::pin(searchable_lab()).await;
    search_needle(&mut lab.app);
    let mut empty = Box::pin(searchable_lab()).await;
    open_bar(&mut empty.app);
    println!("── 80×24, empty query ──");
    for line in screen_at(&mut empty.app, 80, 24) {
        println!("{line}|");
    }
    for (cols, rows) in [(80, 24), (120, 40)] {
        println!("── {cols}×{rows} ──");
        for line in screen_at(&mut lab.app, cols, rows) {
            println!("{}", line.trim_end());
        }
    }
}

/// A snippet received through the folder sync and not confirmed on this
/// device is never typed: the TUI only gets snippet bytes from the core,
/// which refuses it (F8). Real sync on a real folder, public API only.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, two devices, end to end
async fn run_snippet_refuses_unconfirmed_synced_snippet() {
    use portico_core::snippets::{Snippet, SnippetScope};
    use portico_core::sync::{
        JoinBulkChoice, NewPassphrase, SyncCreate, SyncJoin, generate_passphrase,
    };

    let mut lab = lab().await;
    // Another device shares a snippet through a sync folder; this one joins.
    let other = Env::new("");
    let shared = tempfile::tempdir().unwrap();
    other
        .engine
        .save_snippet(Snippet {
            id: "deploy".into(),
            name: "Deploy".into(),
            command: "make deploy".into(),
            description: None,
            scope: SnippetScope::Global,
            run: true,
        })
        .await
        .unwrap();
    let passphrase = generate_passphrase().unwrap();
    let typed = passphrase.as_secret().clone();
    other
        .engine
        .sync_create(SyncCreate {
            folder: shared.path().to_path_buf(),
            passphrase: NewPassphrase::Generated(passphrase),
            device_name: "Laptop".into(),
            remember_key: false,
        })
        .await
        .unwrap();
    let preview = lab
        .app
        .engine
        .sync_join_preview(SyncJoin {
            folder: shared.path().to_path_buf(),
            passphrase: typed,
            device_name: "Desktop".into(),
            remember_key: false,
        })
        .await
        .unwrap();
    lab.app
        .engine
        .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
        .await
        .unwrap();

    lab.select("alpha");
    lab.app.update(Action::Connect);
    lab.until(|app| app.tabs.len() == 1 && all_live(app)).await;
    lab.until(|app| grid_text(app).contains("welcome")).await;
    lab.app.update(Action::OpenSnippets);
    lab.until(|app| {
        app.snippets_panel
            .as_ref()
            .is_some_and(|p| p.all.ready().is_some_and(|all| all.len() == 1))
    })
    .await;
    press(&mut lab.app, KeyCode::Enter, KeyModifiers::NONE);
    // Refused by the core: its review opens (the whole command), over the
    // snippets panel, which stays.
    lab.until(|app| {
        app.overlay == Some(crate::app::Overlay::Sync)
            && app
                .sync_panel
                .as_ref()
                .is_some_and(|p| p.tab == crate::sync_panel::SyncTab::Received)
            && app.sync.snippets.len() == 1
            && !app.sync.stale()
    })
    .await;
    assert!(lab.app.snippets_panel.is_some());
    let review = text(&screen(&mut lab.app, 80, 24));
    assert!(review.contains("make deploy"), "{review}");
    assert!(review.contains("Runs as soon as it is sent"), "{review}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!received(&lab.alpha).contains("make deploy"));

    // `y` too soon (typed for something else) confirms nothing.
    press(&mut lab.app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(
        lab.app
            .sync_panel
            .as_ref()
            .is_some_and(|p| p.busy.is_none())
    );
    // Then `y` confirms the hash shown, and the snippets panel is back.
    tokio::time::sleep(crate::dialog::ARM_DELAY).await;
    press(&mut lab.app, KeyCode::Char('y'), KeyModifiers::NONE);
    lab.until(|app| app.overlay == Some(crate::app::Overlay::Snippets) && app.sync_panel.is_none())
        .await;
    assert!(
        lab.app
            .snippets_panel
            .as_ref()
            .and_then(|p| p.notice.as_ref())
            .is_some_and(|n| n.ok && n.message.contains("confirmed")),
        "{:?}",
        lab.app.snippets_panel.as_ref().map(|p| &p.notice)
    );
    assert!(
        lab.app
            .engine
            .synced_snippets_unconfirmed()
            .await
            .unwrap()
            .is_empty()
    );
    press(&mut lab.app, KeyCode::Enter, KeyModifiers::NONE);
    lab.until(|app| app.snippets_panel.is_none()).await;
    let alpha = &lab.alpha;
    eventually(|| received(alpha).contains("make deploy")).await;
}
