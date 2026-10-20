//! Application state, update loop and event multiplexing.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use color_eyre::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use portico_core::engine::{ConnectRequest, HostInventory, PromptResponse};
use portico_core::events::{SessionEvent, SessionPhase};
use portico_core::ids::SessionId;
use portico_core::search::{Hit, HostSearch};
use portico_core::{Engine, EngineEvent};
use portico_theme::{THEMES, Theme};
use ratatui::DefaultTerminal;
use ratatui::widgets::ListState;
use tokio::sync::mpsc;

use crate::action::Action;
use crate::keymap::map_key;
use crate::theme::Styles;
use crate::ui;

/// Which widget receives text input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Hosts,
    Search,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overlay {
    Help,
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
}

/// Transient notification shown bottom-right, fades after `ttl` ticks.
#[derive(Debug, Clone)]
pub(crate) struct Toast {
    pub(crate) message: String,
    pub(crate) is_error: bool,
    pub(crate) ttl: u16,
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
    /// Sessions whose prompt was declined (no dialog yet: phase 1b).
    declined: HashSet<SessionId>,
    /// Monotonic frame counter for spinners.
    pub(crate) tick: u64,
    search: HostSearch,
    tx: mpsc::UnboundedSender<Action>,
    rx: mpsc::UnboundedReceiver<Action>,
}

/// Animation frame interval. Only drives spinners/toasts; idle UI does not redraw.
const TICK: Duration = Duration::from_millis(80);

impl App {
    pub(crate) fn new(engine: Engine) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
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
            declined: HashSet::new(),
            tick: 0,
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

    /// Main loop: multiplexes terminal input, internal actions and ticks.
    pub(crate) async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let mut input = EventStream::new();
        let mut ticker = tokio::time::interval(TICK);
        let mut engine_events = self.engine.subscribe();
        self.spawn_load_hosts();

        terminal.draw(|f| ui::render(&mut self, f))?;
        while self.running {
            let action = tokio::select! {
                Some(Ok(event)) = input.next() => match event {
                    Event::Key(key) => map_key(key, self.focus, self.overlay.is_some()),
                    Event::Resize(..) => Some(Action::Tick),
                    _ => None,
                },
                Some(action) = self.rx.recv() => Some(action),
                event = engine_events.recv() => event.ok().map(Action::Engine),
                _ = ticker.tick(), if self.needs_animation() => Some(Action::Tick),
            };
            if let Some(action) = action {
                self.update(action);
                terminal.draw(|f| ui::render(&mut self, f))?;
            }
        }
        self.engine.shutdown();
        Ok(())
    }

    fn needs_animation(&self) -> bool {
        self.is_loading()
            || self.toast.is_some()
            || self.sessions.values().any(SessionView::in_flight)
    }

    fn spawn_load_hosts(&self) {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = engine.load_hosts().await.map_err(|e| e.summary().message);
            let _ = tx.send(Action::HostsLoaded(result));
        });
    }

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
            Action::FocusSearch => self.focus = Focus::Search,
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
                self.overlay = match self.overlay {
                    Some(Overlay::Help) => None,
                    None => Some(Overlay::Help),
                };
            }
            Action::CloseOverlay => self.overlay = None,
            Action::CycleTheme => {
                self.theme_index = (self.theme_index + 1) % THEMES.len();
                let name = self.styles().theme.name;
                self.notify(format!("Theme · {name}"), false);
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
                    self.notify(format!("{warnings} ssh_config warning(s)"), false);
                }
            }
            Action::HostsLoaded(Err(e)) => self.load_error = Some(e),
            Action::Connect => self.connect_selected(),
            Action::Disconnect => self.disconnect_selected(),
            Action::Engine(event) => self.on_engine_event(event),
        }
    }

    pub(crate) fn selected_host(&self) -> Option<&portico_core::hosts::HostEntry> {
        let hit = self.hits.get(self.list.selected()?)?;
        self.inventory.as_ref()?.hosts.get(hit.index)
    }

    pub(crate) fn session_for(&self, alias: &str) -> Option<&SessionView> {
        self.sessions.get(alias)
    }

    fn connect_selected(&mut self) {
        let Some(alias) = self.selected_host().map(|h| h.alias.clone()) else {
            return;
        };
        if self
            .session_for(&alias)
            .is_some_and(|s| s.in_flight() || s.phase == SessionPhase::Connected)
        {
            return;
        }
        match self.engine.connect(ConnectRequest::new(alias.clone())) {
            Ok(id) => {
                self.sessions.insert(
                    alias,
                    SessionView {
                        id,
                        phase: SessionPhase::Resolving,
                        latency: None,
                    },
                );
            }
            Err(e) => self.notify(e.summary().message, true),
        }
    }

    fn disconnect_selected(&mut self) {
        let Some(alias) = self.selected_host().map(|h| h.alias.clone()) else {
            return;
        };
        let Some(id) = self.session_for(&alias).map(|s| s.id) else {
            return;
        };
        let engine = self.engine.clone();
        tokio::spawn(async move {
            let _ = engine.disconnect(id).await;
        });
    }

    fn on_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Session { id, event } => {
                let Some((alias, view)) = self.sessions.iter_mut().find(|(_, v)| v.id == id) else {
                    return;
                };
                let alias = alias.clone();
                match event {
                    SessionEvent::Phase(phase) => {
                        view.phase = phase.clone();
                        match phase {
                            SessionPhase::Connected => {
                                self.notify(format!("Connected to {alias}"), false);
                            }
                            SessionPhase::Failed { error } => {
                                let msg = if self.declined.remove(&id) {
                                    format!(
                                        "{alias}: needs an answer (host key or password) — prompt dialogs arrive in the next phase"
                                    )
                                } else {
                                    match error.hint {
                                        Some(hint) => format!("{} — {hint}", error.message),
                                        None => error.message,
                                    }
                                };
                                self.notify(msg, true);
                            }
                            _ => {}
                        }
                    }
                    SessionEvent::Latency { rtt } => view.latency = Some(rtt),
                    _ => {}
                }
            }
            EngineEvent::Prompt(prompt) => {
                // Interactive prompt dialogs land in phase 1b; decline cleanly
                // so the session reports a precise failure instead of hanging.
                let _ = self.engine.respond(prompt.id, PromptResponse::Cancel);
                if let Some(id) = prompt.session {
                    self.declined.insert(id);
                }
            }
            _ => {}
        }
    }

    fn notify(&mut self, message: String, is_error: bool) {
        self.toast = Some(Toast {
            message,
            is_error,
            ttl: if is_error { 80 } else { 40 },
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
