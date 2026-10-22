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
    // The failed pane: message, hint, retry and close keys.
    assert!(all.contains("Connection refused by 10.0.0.7:22"), "{all}");
    assert!(all.contains("Is sshd running on the host?"), "{all}");
    assert!(all.contains("↵  retry"), "{all}");
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
    // A failed focused pane offers ↵ retry in the status bar too.
    assert!(lines[23].contains("↵  retry"), "{}", text(&lines));
    // The prefix bar lists the pane commands first.
    app.update(Action::Prefix);
    let lines = screen(&mut app, 80, 24);
    assert!(lines[23].contains("%  split right"), "{}", text(&lines));
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
    // The focused pane is 40 columns wide: two halves of 20 and a divider
    // do not fit.
    app.update(Action::SplitPane(Split::Vertical));
    let toast = app.toast.clone().unwrap();
    assert_eq!(toast.message, "Not enough room to split this pane");
    assert!(toast.hint.unwrap().contains("20×5"));
    assert_eq!(app.active_tab().map(Tab::len), Some(2));

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
    assert_eq!(
        app.toast.clone().unwrap().message,
        "A tab holds at most 8 panes"
    );
    app.toast = None;
    app.update(Action::SplitPaneHost);
    assert!(app.toast.is_some() && app.pane_pick.is_none());
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
    assert!(all.contains("Split onto…"), "{all}");
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
