//! Application state, update loop and event multiplexing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::time::Duration;

use alacritty_terminal::grid::Scroll;
use color_eyre::Result;
use crossterm::event::{Event, EventStream, KeyEvent};
use futures::StreamExt;
use portico_core::engine::{ConnectRequest, HostInventory};
use portico_core::events::{SessionEvent, SessionPhase, TerminalEvent};
use portico_core::ids::SessionId;
use portico_core::search::{Hit, HostSearch};
use portico_core::{Engine, EngineEvent, ErrorSummary};
use portico_theme::{THEMES, Theme};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;
use tokio::sync::mpsc;

use crate::action::Action;
use crate::dialog::{ChangedKeyAlert, Dialog, Outcome, PromptDialog};
use crate::files::{FilesView, KeyOutcome};
use crate::keymap::map_key;
use crate::terminal::{Ended, GridSize, TerminalView, encode_key};
use crate::theme::Styles;
use crate::tunnels::{PanelOutcome, TunnelRow, TunnelStatus, TunnelsPanel};
use crate::ui;

/// Which widget receives keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Hosts,
    Search,
    Terminal,
    /// SFTP explorer of `App::files`.
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
    Tunnels,
}

/// Transient notification shown bottom-right, fades after `ttl` ticks.
#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub(crate) message: String,
    /// Actionable advice, on its own line under the message.
    pub(crate) hint: Option<String>,
    pub(crate) is_error: bool,
    pub(crate) ttl: u16,
}

/// Live state of one SSH session, derived from engine events.
#[derive(Debug, Clone)]
pub(crate) struct SessionView {
    pub(crate) id: SessionId,
    pub(crate) phase: SessionPhase,
    pub(crate) latency: Option<Duration>,
}

impl SessionView {
    pub(crate) fn in_flight(&self) -> bool {
        !matches!(
            self.phase,
            SessionPhase::Connected | SessionPhase::Failed { .. } | SessionPhase::Closed { .. }
        )
    }

    /// Connected or on its way: something can still be served on it.
    pub(crate) fn usable(&self) -> bool {
        self.in_flight() || self.phase == SessionPhase::Connected
    }
}

#[derive(Debug)]
pub(crate) struct App {
    engine: Engine,
    pub(crate) running: bool,
    pub(crate) focus: Focus,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) theme_index: usize,
    pub(crate) inventory: Option<HostInventory>,
    pub(crate) load_error: Option<String>,
    pub(crate) query: String,
    pub(crate) hits: Vec<Hit>,
    pub(crate) list: ListState,
    pub(crate) toast: Option<Toast>,
    /// Latest session per host alias.
    pub(crate) sessions: HashMap<String, SessionView>,
    /// Terminal tabs, in display order.
    pub(crate) terminals: Vec<TerminalView>,
    /// `None` = Home (host list), `Some(i)` = `terminals[i]`.
    pub(crate) active: Option<usize>,
    /// `Ctrl-Space` was pressed: the next key is a Portico command.
    pub(crate) prefix: bool,
    /// Pending dialogs; the front one is shown and receives keys.
    pub(crate) dialogs: VecDeque<Dialog>,
    /// Size of the terminal area, updated on every render.
    pub(crate) term_size: GridSize,
    /// Monotonic frame counter for spinners.
    pub(crate) tick: u64,
    /// SFTP explorer (kept while hidden, one session at a time).
    pub(crate) files: Option<FilesView>,
    /// Tunnels started from this UI (all sessions).
    pub(crate) tunnels: Vec<TunnelRow>,
    /// The tunnels panel, while `overlay == Some(Overlay::Tunnels)`.
    pub(crate) tunnels_panel: Option<TunnelsPanel>,
    search: HostSearch,
    tx: mpsc::UnboundedSender<Action>,
    rx: mpsc::UnboundedReceiver<Action>,
}

/// Animation frame interval. Only drives spinners/toasts; idle UI does not redraw.
const TICK: Duration = Duration::from_millis(80);
/// Header + status bar.
const CHROME_ROWS: u16 = 2;

impl App {
    pub(crate) fn new(engine: Engine) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            engine,
            running: true,
            focus: Focus::Hosts,
            overlay: None,
            theme_index: 0,
            inventory: None,
            load_error: None,
            query: String::new(),
            hits: Vec::new(),
            list: ListState::default(),
            toast: None,
            sessions: HashMap::new(),
            terminals: Vec::new(),
            active: None,
            prefix: false,
            dialogs: VecDeque::new(),
            term_size: GridSize {
                cols,
                rows: rows.saturating_sub(CHROME_ROWS).max(1),
            },
            tick: 0,
            files: None,
            tunnels: Vec::new(),
            tunnels_panel: None,
            search: HostSearch::default(),
            tx,
            rx,
        }
    }

    pub(crate) fn styles(&self) -> Styles {
        Styles::new(
            THEMES
                .get(self.theme_index)
                .unwrap_or_else(|| Theme::default_theme()),
        )
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.inventory.is_none() && self.load_error.is_none()
    }

    pub(crate) fn active_terminal(&self) -> Option<&TerminalView> {
        self.active.and_then(|i| self.terminals.get(i))
    }

    /// Main loop: multiplexes terminal input, internal actions and ticks.
    pub(crate) async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut ticker = tokio::time::interval(TICK);
        // The ticker is only polled while animating: after an idle spell it
        // must not replay the missed ticks in a burst (a fresh toast would
        // vanish at once).
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut engine_events = self.engine.subscribe();
        self.spawn_load_hosts();

        terminal.draw(|f| ui::render(&mut self, f))?;
        while self.running {
            let action = tokio::select! {
                Some(Ok(event)) = input.next() => self.map_event(event),
                Some(action) = self.rx.recv() => Some(action),
                event = engine_events.recv() => event.ok().map(Action::Engine),
                _ = ticker.tick(), if self.needs_animation() => Some(Action::Tick),
            };
            if let Some(action) = action {
                self.update(action);
                // Drain what is already queued before drawing (one frame per burst).
                while let Ok(action) = self.rx.try_recv() {
                    self.update(action);
                }
                terminal.draw(|f| ui::render(&mut self, f))?;
                self.sync_terminal_sizes();
            }
        }
        for view in &mut self.terminals {
            view.close();
        }
        self.engine.shutdown();
        Ok(())
    }

    fn map_event(&mut self, event: Event) -> Option<Action> {
        match event {
            Event::Key(key) => {
                let action = map_key(
                    key,
                    self.focus,
                    self.overlay,
                    !self.dialogs.is_empty(),
                    self.prefix,
                );
                // The prefix applies to exactly one key.
                if action.is_some() && !matches!(action, Some(Action::Prefix)) {
                    self.prefix = false;
                }
                action
            }
            Event::Paste(text) => Some(Action::Paste(text)),
            Event::Resize(..) => Some(Action::Tick),
            _ => None,
        }
    }

    fn needs_animation(&self) -> bool {
        self.is_loading()
            || self.toast.is_some()
            || self.sessions.values().any(SessionView::in_flight)
            || (self.focus == Focus::Search)
            || self.files.as_ref().is_some_and(FilesView::running)
            || self
                .tunnels
                .iter()
                .any(|t| t.status == TunnelStatus::Starting)
    }

    fn spawn_load_hosts(&self) {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = engine.load_hosts().await.map_err(|e| e.summary().message);
            let _ = tx.send(Action::HostsLoaded(result));
        });
    }

    #[allow(clippy::too_many_lines)] // flat dispatch table, one arm per action
    pub(crate) fn update(&mut self, action: Action) {
        match action {
            Action::Quit => self.running = false,
            Action::Tick => {
                self.tick = self.tick.wrapping_add(1);
                if let Some(t) = &mut self.toast {
                    t.ttl = t.ttl.saturating_sub(1);
                    if t.ttl == 0 {
                        self.toast = None;
                    }
                }
            }
            Action::SelectNext => self.move_selection(1),
            Action::SelectPrev => self.move_selection(-1),
            Action::SelectFirst => {
                self.list
                    .select(if self.hits.is_empty() { None } else { Some(0) });
            }
            Action::SelectLast => self.list.select(self.hits.len().checked_sub(1)),
            Action::FocusSearch => {
                self.go_home();
                self.focus = Focus::Search;
            }
            Action::BlurSearch => self.focus = Focus::Hosts,
            Action::SearchInput(c) => {
                self.query.push(c);
                self.refilter();
            }
            Action::SearchBackspace => {
                self.query.pop();
                self.refilter();
            }
            Action::SearchClear => {
                self.query.clear();
                self.refilter();
            }
            Action::ToggleHelp => {
                self.prefix = false;
                self.tunnels_panel = None;
                self.overlay = match self.overlay {
                    Some(Overlay::Help) => None,
                    _ => Some(Overlay::Help),
                };
            }
            Action::CloseOverlay => {
                self.overlay = None;
                self.tunnels_panel = None;
            }
            Action::CycleTheme => {
                self.theme_index = (self.theme_index + 1) % THEMES.len();
                let name = self.styles().theme.name;
                self.notify(format!("Theme · {name}"));
            }
            Action::Reload => {
                self.inventory = None;
                self.load_error = None;
                self.spawn_load_hosts();
            }
            Action::HostsLoaded(Ok(inv)) => {
                let warnings = inv.warnings.len();
                self.inventory = Some(inv);
                self.refilter();
                if warnings > 0 {
                    self.notify(format!("{warnings} ssh_config warning(s)"));
                }
            }
            Action::HostsLoaded(Err(e)) => self.load_error = Some(e),
            Action::Connect => self.connect_selected(),
            Action::Disconnect => self.disconnect_current(),
            Action::Engine(event) => self.on_engine_event(event),
            Action::DialogKey(key) => self.dialog_key(key),
            Action::Prefix => self.prefix = true,
            Action::CancelPrefix => self.prefix = false,
            Action::GoHome => {
                self.prefix = false;
                self.go_home();
            }
            Action::GoTab(index) => {
                self.prefix = false;
                self.activate(index);
            }
            Action::NextTab => self.cycle_tab(1),
            Action::PrevTab => self.cycle_tab(-1),
            Action::NewTab => {
                self.prefix = false;
                if let Some(view) = self.active_terminal() {
                    let (alias, session) = (view.alias.clone(), view.session);
                    self.open_terminal(alias, session);
                }
            }
            Action::CloseTab => {
                self.prefix = false;
                self.close_active_tab();
            }
            Action::TerminalKey(key) => self.terminal_key(key),
            Action::Paste(text) => {
                // Wiped on drop: the paste may be a password.
                let text = zeroize::Zeroizing::new(text);
                self.paste(&text);
            }
            Action::ScrollUp => self.scroll(Scroll::PageUp),
            Action::ScrollDown => self.scroll(Scroll::PageDown),
            Action::TerminalDirty(id) => {
                if let Some(view) = self.terminals.iter().find(|t| t.id == id) {
                    view.dirty.store(false, Ordering::Release);
                }
            }
            Action::OpenFiles => {
                self.prefix = false;
                self.open_files();
            }
            Action::FilesKey(key) => {
                let down = self
                    .files
                    .as_ref()
                    .is_some_and(|f| !self.session_alive(f.session));
                let outcome = self.files.as_mut().map(|f| f.handle_key(key, down));
                match outcome {
                    Some(KeyOutcome::Leave) => self.leave_files(),
                    Some(KeyOutcome::Reconnect) => self.reconnect_files(),
                    Some(KeyOutcome::Handled) | None => {}
                }
            }
            Action::Files(event) => {
                if let Some(files) = &mut self.files {
                    files.on_event(event);
                }
            }
            Action::OpenTunnels => {
                self.prefix = false;
                self.open_tunnels();
            }
            Action::TunnelsKey(key) => self.tunnels_key(key),
            Action::TunnelStarted {
                session,
                alias,
                spec,
                result,
            } => match result {
                Ok(id) => {
                    // The engine may already have reported a failure.
                    if !self.tunnels.iter().any(|t| t.id == id) {
                        self.tunnels.push(TunnelRow {
                            id,
                            session,
                            alias,
                            spec,
                            status: TunnelStatus::Starting,
                            connections: 0,
                        });
                    }
                    self.rebuild_tunnels_panel();
                }
                Err(e) => {
                    if let Some(panel) = &mut self.tunnels_panel {
                        panel.error = Some(e);
                    } else {
                        self.notify_error(e);
                    }
                }
            },
            Action::TerminalOpened { alias, result } => match result {
                Ok(handle) => {
                    let view = TerminalView::spawn(*handle, alias, self.term_size, self.tx.clone());
                    self.terminals.push(view);
                    self.activate(self.terminals.len() - 1);
                }
                // The session's own failure is already reported.
                Err(error) if error.code == "session_ended" => {}
                Err(error) => self.notify_error(error),
            },
        }
    }

    pub(crate) fn selected_host(&self) -> Option<&portico_core::hosts::HostEntry> {
        let hit = self.hits.get(self.list.selected()?)?;
        self.inventory.as_ref()?.hosts.get(hit.index)
    }

    pub(crate) fn session_for(&self, alias: &str) -> Option<&SessionView> {
        self.sessions.get(alias)
    }

    pub(crate) fn session_by_id(&self, id: SessionId) -> Option<&SessionView> {
        self.sessions.values().find(|s| s.id == id)
    }

    /// The session is connected or connecting (not failed, closed or
    /// replaced by a newer one for the same host).
    pub(crate) fn session_alive(&self, id: SessionId) -> bool {
        self.session_by_id(id).is_some_and(SessionView::usable)
    }

    // ── tabs ───────────────────────────────────────────────────────────────

    fn go_home(&mut self) {
        self.active = None;
        if self.focus == Focus::Terminal {
            self.focus = Focus::Hosts;
        }
    }

    fn activate(&mut self, index: usize) {
        if index < self.terminals.len() {
            self.active = Some(index);
            self.focus = Focus::Terminal;
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        self.prefix = false;
        // Positions: 0 = Home, 1..=n = terminals.
        let n = self.terminals.len() + 1;
        let current = self.active.map_or(0, |i| i + 1);
        let next = (current + n).saturating_add_signed(delta) % n;
        if next == 0 {
            self.go_home();
        } else {
            self.activate(next - 1);
        }
    }

    fn close_active_tab(&mut self) {
        let Some(index) = self.active else { return };
        if index >= self.terminals.len() {
            return;
        }
        let mut view = self.terminals.remove(index);
        view.close();
        // Closing the last tab of a session disconnects it.
        if !self.terminals.iter().any(|t| t.session == view.session) {
            let engine = self.engine.clone();
            tokio::spawn(async move {
                let _ = engine.disconnect(view.session).await;
            });
        }
        if self.terminals.is_empty() {
            self.go_home();
        } else {
            self.activate(index.min(self.terminals.len() - 1));
        }
    }

    /// A paste goes to the open dialog if any (it may be a password from a
    /// password manager), never past it to a shell or the search field.
    fn paste(&mut self, text: &str) {
        match self.dialogs.front_mut() {
            Some(Dialog::Prompt(prompt)) => prompt.paste(text),
            Some(Dialog::ChangedKey(_)) => {}
            None => match (self.focus, self.active_terminal()) {
                (Focus::Terminal, Some(view)) => view.paste(text),
                (Focus::Search, _) => {
                    self.query.push_str(text.lines().next().unwrap_or_default());
                    self.refilter();
                }
                _ => {}
            },
        }
    }

    fn scroll(&mut self, scroll: Scroll) {
        if let Some(view) = self.active_terminal() {
            view.scroll(scroll);
        }
    }

    fn terminal_key(&mut self, key: KeyEvent) {
        self.prefix = false;
        let Some(view) = self.active_terminal() else {
            return;
        };
        if view.closed || view.ended.is_some() {
            return;
        }
        if let Some(bytes) = encode_key(key, view.mode()) {
            view.send(bytes);
        }
    }

    /// Keeps PTY sizes in sync with the drawn terminal area.
    fn sync_terminal_sizes(&mut self) {
        let size = self.term_size;
        for view in &mut self.terminals {
            view.resize(size);
        }
    }

    // ── SFTP explorer & tunnels ────────────────────────────────────────────

    /// The session the user is looking at: the active terminal's, else the
    /// selected host's live one. `connect` starts a session if there is none.
    fn current_session(&mut self, connect: bool) -> Option<(SessionId, String)> {
        if let Some(view) = self
            .active_terminal()
            .filter(|_| self.focus != Focus::Hosts)
        {
            return Some((view.session, view.alias.clone()));
        }
        if self.focus == Focus::Files
            && let Some(files) = &self.files
        {
            return Some((files.session, files.alias.clone()));
        }
        let alias = self.selected_host()?.alias.clone();
        if let Some(s) = self.session_for(&alias).filter(|s| s.usable()) {
            return Some((s.id, alias));
        }
        if !connect {
            return None;
        }
        match self.engine.connect(ConnectRequest::new(alias.clone())) {
            Ok(id) => {
                self.sessions.insert(
                    alias.clone(),
                    SessionView {
                        id,
                        phase: SessionPhase::Resolving,
                        latency: None,
                    },
                );
                Some((id, alias))
            }
            Err(e) => {
                self.notify_error(e.summary());
                None
            }
        }
    }

    fn open_files(&mut self) {
        let Some((session, alias)) = self.current_session(true) else {
            return;
        };
        if self.files.as_ref().is_none_or(|f| f.session != session) {
            self.files = Some(FilesView::open(
                self.engine.clone(),
                self.tx.clone(),
                session,
                alias,
            ));
        }
        self.focus = Focus::Files;
    }

    /// `↵ reconnect` in a remote pane whose session is down: reuse a live
    /// session of the host if one exists, else connect, then reopen SFTP.
    /// The explorer's host failed with an error that forbids reconnecting
    /// (changed host key).
    pub(crate) fn files_refused(&self) -> bool {
        let Some(alias) = self.files.as_ref().map(|f| f.alias.as_str()) else {
            return false;
        };
        matches!(
            self.session_for(alias).map(|s| &s.phase),
            Some(SessionPhase::Failed { error }) if crate::files::refuses_reconnect(error)
        )
    }

    fn reconnect_files(&mut self) {
        if self.files_refused() {
            return;
        }
        let Some(alias) = self.files.as_ref().map(|f| f.alias.clone()) else {
            return;
        };
        let session = match self.session_for(&alias).filter(|s| s.usable()) {
            Some(s) => s.id,
            None => match self.engine.connect(ConnectRequest::new(alias.clone())) {
                Ok(id) => {
                    self.sessions.insert(
                        alias,
                        SessionView {
                            id,
                            phase: SessionPhase::Resolving,
                            latency: None,
                        },
                    );
                    id
                }
                Err(e) => {
                    self.notify_error(e.summary());
                    return;
                }
            },
        };
        if let Some(files) = &mut self.files {
            files.rebind(session);
        }
    }

    fn leave_files(&mut self) {
        self.focus = if self.active_terminal().is_some() {
            Focus::Terminal
        } else {
            Focus::Hosts
        };
    }

    fn open_tunnels(&mut self) {
        // Only a live session can take new tunnels; otherwise the panel
        // just lists what runs and offers `Esc`.
        let session = self
            .current_session(false)
            .filter(|(id, _)| self.session_alive(*id));
        let alias_of = |id: SessionId| -> String {
            self.sessions
                .iter()
                .find(|(_, v)| v.id == id)
                .map(|(a, _)| a.clone())
                .unwrap_or_default()
        };
        let known = self.engine.tunnels();
        let mut rows = std::mem::take(&mut self.tunnels);
        crate::tunnels::sync(&mut rows, &known, alias_of);
        self.tunnels = rows;
        self.tunnels_panel = Some(TunnelsPanel::new(session));
        self.overlay = Some(Overlay::Tunnels);
        self.rebuild_tunnels_panel();
    }

    /// Forwards declared in `ssh_config` for the panel's host.
    fn declared_forwards(&self, alias: &str) -> Vec<portico_core::forward::ForwardSpec> {
        let Some(host) = self
            .inventory
            .as_ref()
            .and_then(|i| i.hosts.iter().find(|h| h.alias == alias))
        else {
            return Vec::new();
        };
        let local = host
            .resolved
            .local_forwards
            .iter()
            .filter_map(|f| portico_core::forward::ForwardSpec::parse_local(f).ok());
        let remote = host
            .resolved
            .remote_forwards
            .iter()
            .filter_map(|f| portico_core::forward::ForwardSpec::parse_remote(f).ok());
        local.chain(remote).collect()
    }

    fn rebuild_tunnels_panel(&mut self) {
        let declared = self
            .tunnels_panel
            .as_ref()
            .and_then(|p| p.session.as_ref())
            .map(|(_, alias)| self.declared_forwards(alias))
            .unwrap_or_default();
        if let Some(panel) = &mut self.tunnels_panel {
            panel.rebuild(&self.tunnels, &declared);
        }
    }

    fn tunnels_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.tunnels_panel else {
            return;
        };
        match panel.handle_key(key) {
            PanelOutcome::None => {}
            PanelOutcome::Close => {
                self.overlay = None;
                self.tunnels_panel = None;
            }
            PanelOutcome::Start(spec, allow_exposed) => {
                let Some((session, alias)) = panel.session.clone() else {
                    return;
                };
                let (engine, tx) = (self.engine.clone(), self.tx.clone());
                tokio::spawn(async move {
                    let result = engine
                        .start_tunnel(session, spec.clone(), allow_exposed)
                        .await
                        .map_err(|e| e.summary());
                    let _ = tx.send(Action::TunnelStarted {
                        session,
                        alias,
                        spec,
                        result,
                    });
                });
            }
            PanelOutcome::Stop(id) => {
                if self.engine.stop_tunnel(id).is_err() {
                    // Already gone (failed): just forget it.
                    self.tunnels.retain(|t| t.id != id);
                    self.rebuild_tunnels_panel();
                }
            }
        }
    }

    // ── sessions ───────────────────────────────────────────────────────────

    fn connect_selected(&mut self) {
        let Some(alias) = self.selected_host().map(|h| h.alias.clone()) else {
            return;
        };
        // Existing live tab for this host: just switch to it.
        if let Some(index) = self
            .terminals
            .iter()
            .position(|t| t.alias == alias && !t.closed && t.ended.is_none())
        {
            self.activate(index);
            return;
        }
        let session = match self
            .session_for(&alias)
            .filter(|s| s.usable())
            .map(|s| s.id)
        {
            Some(id) => id,
            None => match self.engine.connect(ConnectRequest::new(alias.clone())) {
                Ok(id) => {
                    self.sessions.insert(
                        alias.clone(),
                        SessionView {
                            id,
                            phase: SessionPhase::Resolving,
                            latency: None,
                        },
                    );
                    id
                }
                Err(e) => {
                    self.notify_error(e.summary());
                    return;
                }
            },
        };
        self.open_terminal(alias, session);
    }

    /// Requests a shell; served once the session is connected.
    fn open_terminal(&self, alias: String, session: SessionId) {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        let size = self.term_size.pty();
        tokio::spawn(async move {
            let result = engine
                .open_terminal(session, size)
                .await
                .map(Box::new)
                .map_err(|e| e.summary());
            let _ = tx.send(Action::TerminalOpened { alias, result });
        });
    }

    fn disconnect_current(&mut self) {
        self.prefix = false;
        let id = match (self.focus, self.active_terminal()) {
            (Focus::Terminal, Some(view)) => Some(view.session),
            _ => self
                .selected_host()
                .and_then(|h| self.session_for(&h.alias))
                .map(|s| s.id),
        };
        let Some(id) = id else { return };
        let engine = self.engine.clone();
        tokio::spawn(async move {
            let _ = engine.disconnect(id).await;
        });
    }

    fn on_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Session { id, event } => self.on_session_event(id, event),
            EngineEvent::Terminal { id, event, .. } => {
                if let Some(view) = self.terminals.iter_mut().find(|t| t.id == id) {
                    match event {
                        TerminalEvent::Exited { status, signal } => {
                            view.ended = Some(match (status, signal) {
                                (Some(code), _) => Ended::Exited(code),
                                (None, Some(sig)) => Ended::Killed(sig),
                                (None, None) => Ended::Closed,
                            });
                        }
                        TerminalEvent::Closed => {
                            view.closed = true;
                            view.ended.get_or_insert(Ended::Closed);
                        }
                        _ => {}
                    }
                }
            }
            EngineEvent::Prompt(prompt) => {
                let alias = prompt
                    .session
                    .and_then(|id| self.sessions.iter().find(|(_, v)| v.id == id))
                    .map(|(a, _)| a.clone());
                self.dialogs
                    .push_back(Dialog::Prompt(PromptDialog::new(prompt, alias)));
            }
            EngineEvent::PromptResolved { id } => {
                self.dialogs
                    .retain(|d| !matches!(d, Dialog::Prompt(p) if p.id == id));
            }
            EngineEvent::Sftp { session, event, .. } => {
                if let Some(files) = self.files.as_mut().filter(|f| f.session == session) {
                    files.on_sftp(&event);
                }
            }
            // A guard would hide the mutation in `apply`.
            #[allow(clippy::collapsible_match)]
            EngineEvent::Tunnel { id, event, .. } => {
                if crate::tunnels::apply(&mut self.tunnels, id, &event) {
                    self.rebuild_tunnels_panel();
                }
            }
            _ => {}
        }
    }

    fn on_session_event(&mut self, id: SessionId, event: SessionEvent) {
        let Some(view) = self.sessions.values_mut().find(|v| v.id == id) else {
            return;
        };
        match event {
            SessionEvent::Phase(phase) => {
                view.phase = phase.clone();
                match phase {
                    // The blocking alert already explains a changed key.
                    SessionPhase::Failed { error } if error.code == "host_key_changed" => {
                        self.end_terminals(id);
                    }
                    // The user refused the key: the host list says so, quietly.
                    SessionPhase::Failed { error } if error.code == "host_key_rejected" => {
                        self.end_terminals(id);
                    }
                    SessionPhase::Failed { error } => {
                        // The open explorer already shows it, with its hint
                        // and `↵ reconnect`: no duplicate toast on top.
                        let shown = self.focus == Focus::Files
                            && self.files.as_ref().is_some_and(|f| f.session == id);
                        if !shown {
                            self.notify_error(error);
                        }
                        self.end_terminals(id);
                    }
                    SessionPhase::Closed { .. } => self.end_terminals(id),
                    _ => {}
                }
                if !self.session_alive(id) {
                    self.drop_tunnels_target(id);
                }
            }
            SessionEvent::Latency { rtt } => view.latency = Some(rtt),
            SessionEvent::HostKeyChanged {
                host,
                port,
                algorithm,
                fingerprint,
                expected_fingerprint,
                known_hosts_file,
                known_hosts_line,
            } => {
                self.dialogs.push_back(Dialog::ChangedKey(ChangedKeyAlert {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    expected: expected_fingerprint,
                    file: known_hosts_file,
                    line: known_hosts_line,
                }));
            }
            _ => {}
        }
    }

    /// The tunnels panel stops offering to add tunnels to a dead session.
    fn drop_tunnels_target(&mut self, session: SessionId) {
        let Some(panel) = &mut self.tunnels_panel else {
            return;
        };
        if panel.session.as_ref().is_some_and(|(id, _)| *id == session) {
            panel.session = None;
            panel.input = None;
            panel.confirm = None;
            self.rebuild_tunnels_panel();
        }
    }

    fn end_terminals(&mut self, session: SessionId) {
        for view in self.terminals.iter_mut().filter(|t| t.session == session) {
            view.closed = true;
            view.ended.get_or_insert(Ended::Closed);
        }
    }

    fn dialog_key(&mut self, key: KeyEvent) {
        let Some(front) = self.dialogs.front_mut() else {
            return;
        };
        match front {
            Dialog::ChangedKey(_) => {
                if matches!(
                    key.code,
                    crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Enter
                ) {
                    self.dialogs.pop_front();
                }
            }
            Dialog::Prompt(prompt) => {
                if let Outcome::Answer(response) = prompt.handle_key(key) {
                    let id = prompt.id;
                    self.dialogs.pop_front();
                    // Already resolved elsewhere (cancelled session): nothing to do.
                    let _ = self.engine.respond(id, response);
                }
            }
        }
    }

    fn notify_error(&mut self, error: ErrorSummary) {
        self.toast = Some(Toast {
            message: error.message,
            hint: error.hint,
            is_error: true,
            ttl: 80,
        });
    }

    fn notify(&mut self, message: String) {
        self.toast = Some(Toast {
            message,
            hint: None,
            is_error: false,
            ttl: 40,
        });
    }

    fn refilter(&mut self) {
        let Some(inv) = &self.inventory else { return };
        self.hits = self.search.search(&inv.hosts, &self.query);
        self.list
            .select(if self.hits.is_empty() { None } else { Some(0) });
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.hits.len();
        if len == 0 {
            return;
        }
        let current = self.list.selected().unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(len - 1);
        self.list.select(Some(next));
    }
}

/// An app on an engine that never touches the real `~/.ssh` (tests only).
#[cfg(test)]
pub(crate) fn test_app() -> App {
    use std::sync::Arc;

    use portico_core::EngineConfig;
    use portico_core::secrets::MemoryStore;
    use portico_core::store::Paths;

    let engine = Engine::new(EngineConfig {
        paths: Paths::rooted(std::path::Path::new("/nonexistent")),
        local_user: "alice".into(),
        event_capacity: 16,
        secrets: Arc::new(MemoryStore::default()),
        agent_socket: None,
        connect_timeout: Duration::from_secs(1),
        keepalive: Duration::from_secs(1),
    });
    App::new(engine)
}

#[cfg(test)]
mod tests {
    use portico_core::events::{Prompt, PromptKind};
    use portico_core::ids::PromptId;

    use super::*;

    fn app() -> App {
        test_app()
    }

    #[test]
    fn paste_goes_to_the_open_dialog_never_to_search() {
        let mut app = app();
        app.focus = Focus::Search;
        let mut dialog = PromptDialog::new(
            Prompt {
                id: PromptId::next(),
                session: None,
                kind: PromptKind::Password {
                    user: "u".into(),
                    host: "h".into(),
                    attempt: 1,
                    attempts: 3,
                },
            },
            None,
        );
        dialog.opened_at = std::time::Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        app.dialogs.push_back(Dialog::Prompt(dialog));

        app.update(Action::Paste("s3cr3t".into()));
        assert!(
            app.query.is_empty(),
            "the secret must not reach the search field"
        );
        let Some(Dialog::Prompt(p)) = app.dialogs.front() else {
            panic!("dialog")
        };
        assert_eq!(p.fields[0].value.as_str(), "s3cr3t");
    }
}

/// Rendering checks at 80×24 (the smallest supported terminal).
#[cfg(test)]
mod render_tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use portico_core::forward::ForwardSpec;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::keymap::HELP;

    fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|f| ui::render(app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    fn contains(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    #[test]
    fn every_help_label_fits_80x24() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Help);
        let lines = screen(&mut app, 80, 24);
        for (title, rows) in HELP {
            assert!(
                contains(&lines, title),
                "section {title}\n{}",
                lines.join("\n")
            );
            for (key, label) in *rows {
                let row = format!("{key}  ");
                assert!(
                    lines.iter().any(|l| l.contains(&row) && l.contains(label)),
                    "`{key}` / `{label}` is not shown in full:\n{}",
                    lines.join("\n")
                );
            }
        }
        // The only ellipsis is the one in "Ctrl-Space, then…": nothing cut.
        let ellipses = lines.iter().map(|l| l.matches('…').count()).sum::<usize>();
        assert_eq!(ellipses, 1, "{}", lines.join("\n"));
    }

    fn files_on(app: &mut App, phase: SessionPhase) -> SessionId {
        let id = SessionId::next();
        app.sessions.insert(
            "db-primary".into(),
            SessionView {
                id,
                phase,
                latency: None,
            },
        );
        app.files = Some(FilesView::open(
            app.engine.clone(),
            app.tx.clone(),
            id,
            "db-primary".into(),
        ));
        app.focus = Focus::Files;
        id
    }

    #[tokio::test]
    async fn connecting_remote_pane_shows_the_phase() {
        let mut app = test_app();
        files_on(
            &mut app,
            SessionPhase::Connecting {
                hop: 1,
                hops: 2,
                target: "bastion".into(),
            },
        );
        let lines = screen(&mut app, 80, 24);
        assert!(
            contains(&lines, "Connecting · bastion (1/2)"),
            "{}",
            lines.join("\n")
        );
        assert!(
            !contains(&lines, "db-primary:"),
            "no `alias:` with an empty path"
        );
    }

    #[tokio::test]
    async fn failed_remote_pane_offers_reconnect_not_retry() {
        let mut app = test_app();
        let old = files_on(
            &mut app,
            SessionPhase::Failed {
                error: ErrorSummary {
                    code: "connect_failed".into(),
                    message: "connection refused".into(),
                    hint: Some("Is sshd running?".into()),
                },
            },
        );
        let lines = screen(&mut app, 80, 24);
        let text = lines.join("\n");
        assert!(contains(&lines, "connection refused"), "{text}");
        assert!(contains(&lines, "Is sshd running?"), "{text}");
        assert!(contains(&lines, "↵  reconnect"), "{text}");
        assert!(!contains(&lines, "Ctrl-R"), "{text}");
        assert!(!contains(&lines, "db-primary:"), "{text}");

        app.update(Action::FilesKey(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        // Either a new session is on its way, or the refusal is reported.
        let rebound = app.files.as_ref().is_some_and(|f| f.session != old);
        assert!(rebound || app.toast.is_some());
    }

    #[test]
    fn tunnels_without_a_live_session_only_offer_close() {
        let mut app = test_app();
        app.update(Action::OpenTunnels);
        let lines = screen(&mut app, 80, 24);
        let text = lines.join("\n");
        assert!(contains(&lines, "Esc close"), "{text}");
        for inert in ["a  add", "start suggested", "x  stop"] {
            assert!(!contains(&lines, inert), "`{inert}` offered:\n{text}");
        }
    }

    #[test]
    fn tunnel_errors_wrap_above_the_hints() {
        let mut app = test_app();
        let mut panel = TunnelsPanel::new(Some((SessionId::next(), "web".into())));
        panel.rebuild(&[], &[ForwardSpec::parse_cli("-L 8080:db:5432").unwrap()]);
        let long = "the remote side refused to forward port 5432 to db because the \
                    administrator disabled TCP forwarding for this account";
        panel.error = Some(ErrorSummary {
            code: "forward_refused".into(),
            message: long.into(),
            hint: Some("Ask for AllowTcpForwarding.".into()),
        });
        app.tunnels_panel = Some(panel);
        app.overlay = Some(Overlay::Tunnels);
        let lines = screen(&mut app, 80, 24);
        let text = lines.join("\n");
        let message_row = lines.iter().position(|l| l.contains("administrator"));
        let hint_row = lines.iter().position(|l| l.contains("AllowTcpForwarding"));
        let keys_row = lines.iter().position(|l| l.contains("a  add"));
        assert!(contains(&lines, "account"), "not truncated:\n{text}");
        assert!(message_row < hint_row && hint_row < keys_row, "{text}");
        assert!(contains(&lines, "↵  start"), "{text}");
    }
}
