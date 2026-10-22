//! Split panes of the terminal tabs (phase 6): the app side of
//! [`crate::panes`].
//!
//! Each tab is a [`Tab`], a tree of [`PaneSlot`]s. A pane is one more
//! `TerminalHandle`: on the same session (one more PTY channel on the same
//! transport) or on another host's session. Only the focused pane of the
//! active tab receives keys, pastes and scrolls (no broadcast). Closing a
//! pane closes its channel; when no pane of any tab uses a session any
//! more, the session is disconnected (as closing a host's last tab always
//! did).

use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, KeyEventKind};
use portico_core::ErrorSummary;
use portico_core::engine::{ConnectRequest, TerminalHandle};
use portico_core::events::SessionPhase;
use portico_core::ids::{SessionId, TerminalId};
use ratatui::layout::Rect;

use crate::action::Action;
use crate::app::{App, Focus, SessionView};
use crate::panes::{Dir, PaneId, PaneTree, Refusal, Split};
use crate::terminal::{Ended, GridSize, TerminalView};

/// What one pane shows.
#[derive(Debug)]
pub(crate) enum PaneSlot {
    /// Waiting for its shell (the session may still be connecting).
    Opening {
        alias: String,
        session: SessionId,
    },
    Live(TerminalView),
    /// The shell could not be opened.
    Failed {
        alias: String,
        session: Option<SessionId>,
        error: ErrorSummary,
    },
}

impl PaneSlot {
    pub(crate) fn alias(&self) -> &str {
        match self {
            Self::Opening { alias, .. } | Self::Failed { alias, .. } => alias,
            Self::Live(view) => &view.alias,
        }
    }

    pub(crate) fn session(&self) -> Option<SessionId> {
        match self {
            Self::Opening { session, .. } => Some(*session),
            Self::Live(view) => Some(view.session),
            Self::Failed { session, .. } => *session,
        }
    }

    pub(crate) fn view(&self) -> Option<&TerminalView> {
        match self {
            Self::Live(view) => Some(view),
            _ => None,
        }
    }

    pub(crate) fn view_mut(&mut self) -> Option<&mut TerminalView> {
        match self {
            Self::Live(view) => Some(view),
            _ => None,
        }
    }

    /// The shell is up and takes input.
    pub(crate) fn live(&self) -> bool {
        self.view().is_some_and(|v| !v.closed && v.ended.is_none())
    }
}

/// A terminal tab: its panes.
pub(crate) type Tab = PaneTree<PaneSlot>;

/// `Ctrl-Space S`: the host list is picking the host of a new pane that
/// will split `pane`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanePick {
    pub(crate) pane: PaneId,
    /// The list's search before the pick (it starts empty, and comes back
    /// once the pick ends).
    pub(crate) query: String,
}

/// How long the arrows keep working without the prefix after
/// `Ctrl-Space` + arrow (tmux's repeat-time).
pub(crate) const PANE_REPEAT: Duration = Duration::from_millis(600);
/// How long a refusal stays in the status bar.
pub(crate) const NOTE_TTL: Duration = Duration::from_secs(4);

/// A refusal shown on one line of the status bar, in `status-warning`:
/// compact, it never covers a pane (nor its prompt line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusNote {
    pub(crate) message: String,
    pub(crate) hint: String,
    pub(crate) until: Instant,
}

/// The error a dead pane shows, and whether `↵` may retry it.
#[derive(Debug, Clone)]
pub(crate) struct PaneFailure {
    pub(crate) error: Option<ErrorSummary>,
    pub(crate) retry: bool,
}

/// A session a new pane may use: connected or on its way, and not being
/// torn down (a shell asked of a disconnecting session would only fail).
fn reusable(view: &SessionView) -> bool {
    view.usable() && view.phase != SessionPhase::Disconnecting
}

/// The split that suits the focused pane (see
/// [`PaneTree::preferred_splits`]), else the other one, else why neither.
fn best_split(tab: &Tab, area: Rect) -> Result<Split, Refusal> {
    let splits = tab.preferred_splits(area);
    let mut refusal = Refusal::TooSmall;
    for split in splits {
        match tab.can_split(area, split) {
            Ok(()) => return Ok(split),
            Err(Refusal::TooMany) => return Err(Refusal::TooMany),
            Err(other) => refusal = other,
        }
    }
    Err(refusal)
}

impl App {
    // ── lookups ────────────────────────────────────────────────────────────

    /// The area every tab is laid out in (the body, between header and
    /// status bar), as of the last frame.
    pub(crate) fn body_area(&self) -> Rect {
        Rect::new(0, 0, self.term_size.cols, self.term_size.rows)
    }

    pub(crate) fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// The pane with the focus, in the active tab.
    pub(crate) fn focused_slot(&self) -> Option<&PaneSlot> {
        self.active_tab()?.focused()
    }

    /// Every terminal of every tab.
    pub(crate) fn views(&self) -> impl Iterator<Item = &TerminalView> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.items().into_iter().filter_map(|(_, s)| s.view()))
    }

    pub(crate) fn views_mut(&mut self) -> impl Iterator<Item = &mut TerminalView> {
        self.tabs.iter_mut().flat_map(|tab| {
            tab.items_mut()
                .into_iter()
                .filter_map(|(_, s)| s.view_mut())
        })
    }

    pub(crate) fn tab_of(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.contains(pane))
    }

    /// Tab and pane of terminal `id`.
    pub(crate) fn pane_of_terminal(&self, id: TerminalId) -> Option<(usize, PaneId)> {
        self.tabs.iter().enumerate().find_map(|(index, tab)| {
            tab.items()
                .into_iter()
                .find(|(_, slot)| slot.view().is_some_and(|v| v.id == id))
                .map(|(pane, _)| (index, pane))
        })
    }

    /// Shows tab `index` with `pane` focused.
    pub(crate) fn show_pane(&mut self, index: usize, pane: PaneId) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.set_focus(pane);
        }
        self.activate(index);
    }

    /// Some pane of some tab still uses `session`.
    pub(crate) fn session_in_use(&self, session: SessionId) -> bool {
        self.tabs.iter().any(|tab| {
            tab.items()
                .iter()
                .any(|(_, slot)| slot.session() == Some(session))
        })
    }

    /// A pane on screen shows `session`'s state (its failure needs no
    /// toast on top).
    pub(crate) fn panes_show(&self, session: SessionId) -> bool {
        let unobstructed = self.overlay.is_none() && self.focus == Focus::Terminal;
        unobstructed
            && self.active_tab().is_some_and(|tab| {
                let visible = if tab.zoomed() {
                    vec![tab.focus()]
                } else {
                    tab.ids()
                };
                visible
                    .into_iter()
                    .filter_map(|id| tab.get(id))
                    .any(|slot| slot.session() == Some(session))
            })
    }

    /// Disconnects `session` when no pane uses it any more.
    pub(crate) fn release_session(&self, session: SessionId) {
        if self.session_in_use(session) {
            return;
        }
        let engine = self.engine.clone();
        tokio::spawn(async move {
            let _ = engine.disconnect(session).await;
        });
    }

    /// The host's live session, or a new one on its way.
    pub(crate) fn session_or_connect(&mut self, alias: &str) -> Option<SessionId> {
        if let Some(view) = self.session_for(alias).filter(|s| reusable(s)) {
            return Some(view.id);
        }
        match self.engine.connect(ConnectRequest::new(alias)) {
            Ok(id) => {
                self.sessions.insert(
                    alias.to_owned(),
                    SessionView {
                        id,
                        phase: SessionPhase::Resolving,
                        latency: None,
                    },
                );
                Some(id)
            }
            Err(e) => {
                self.notify_error(e.summary());
                None
            }
        }
    }

    /// Requests a shell, served once the session is connected. `pane`:
    /// the pane waiting for it; `None` opens a new tab.
    pub(crate) fn request_terminal(
        &self,
        alias: String,
        session: SessionId,
        pane: Option<PaneId>,
        size: GridSize,
    ) {
        let engine = self.engine.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = engine
                .open_terminal(session, size.pty())
                .await
                .map(Box::new)
                .map_err(|e| e.summary());
            let _ = tx.send(Action::TerminalOpened {
                alias,
                pane,
                result,
            });
        });
    }

    /// Grid size of `pane` in tab `index`, as laid out now.
    fn pane_size(&self, index: usize, pane: PaneId) -> GridSize {
        let area = self.body_area();
        self.tabs
            .get(index)
            .and_then(|tab| {
                tab.grid_sizes(area)
                    .into_iter()
                    .find(|(id, ..)| *id == pane)
            })
            .map_or(self.term_size, |(_, cols, rows)| GridSize {
                cols: cols.max(1),
                rows: rows.max(1),
            })
    }

    // ── shells arriving ────────────────────────────────────────────────────

    pub(crate) fn terminal_opened(
        &mut self,
        alias: String,
        pane: Option<PaneId>,
        result: Result<Box<TerminalHandle>, ErrorSummary>,
    ) {
        let Some(pane) = pane else {
            match result {
                Ok(handle) => {
                    let view = TerminalView::spawn(*handle, alias, self.term_size, self.tx.clone());
                    self.tabs
                        .push(Tab::new(PaneId::next(), PaneSlot::Live(view)));
                    self.activate(self.tabs.len() - 1);
                }
                // The session's own failure is already reported.
                Err(error) if error.code == "session_ended" => {}
                Err(error) => self.notify_error(error),
            }
            return;
        };
        let waiting = self
            .tab_of(pane)
            .filter(|&i| {
                matches!(
                    self.tabs.get(i).and_then(|t| t.get(pane)),
                    Some(PaneSlot::Opening { .. })
                )
            })
            .map(|i| (i, self.pane_size(i, pane)));
        let Some((index, size)) = waiting else {
            // The pane was closed meanwhile: close its channel too.
            if let Ok(handle) = result {
                handle.input.close();
            }
            return;
        };
        let session = self
            .tabs
            .get(index)
            .and_then(|t| t.get(pane))
            .and_then(PaneSlot::session);
        let slot = match result {
            Ok(handle) => {
                PaneSlot::Live(TerminalView::spawn(*handle, alias, size, self.tx.clone()))
            }
            Err(error) => PaneSlot::Failed {
                alias,
                session,
                error,
            },
        };
        if let Some(target) = self.tabs.get_mut(index).and_then(|t| t.get_mut(pane)) {
            *target = slot;
        }
    }

    // ── pane commands ──────────────────────────────────────────────────────

    /// A refused split: one line in the status bar, for [`NOTE_TTL`]. Not a
    /// toast, which would cover the pane the next keys go to.
    pub(crate) fn refuse(&mut self, refusal: Refusal) {
        self.note = Some(StatusNote {
            message: refusal.message(),
            hint: refusal.hint(),
            until: Instant::now() + NOTE_TTL,
        });
    }

    // ── timed states ───────────────────────────────────────────────────────

    /// Pane mode is on at `now` (in a terminal, nothing over it).
    pub(crate) fn pane_repeat_active(&self, now: Instant) -> bool {
        self.pane_repeat.is_some_and(|until| now < until)
            && self.focus == Focus::Terminal
            && self.overlay.is_none()
            && self.dialogs.is_empty()
            && self.active_tab().is_some()
    }

    /// After a key: a pane move (with the prefix, or in pane mode) opens or
    /// extends the window; any other key ends it (and goes where it would
    /// have gone anyway).
    pub(crate) fn track_pane_repeat(
        &mut self,
        action: Option<&Action>,
        key: KeyEvent,
        now: Instant,
    ) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        match action {
            Some(Action::FocusPane(_) | Action::ResizePane(_)) => {
                self.pane_repeat = Some(now + PANE_REPEAT);
            }
            Some(_) => self.pane_repeat = None,
            None => {}
        }
    }

    /// The next instant a timed state runs out.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let note = self.note.as_ref().map(|n| n.until);
        let playback = self
            .player
            .as_ref()
            .and_then(crate::player::Player::next_due);
        [self.pane_repeat, note, playback]
            .into_iter()
            .flatten()
            .min()
    }

    /// Ends what ran out at `now`.
    pub(crate) fn expire(&mut self, now: Instant) {
        if self.pane_repeat.is_some_and(|until| until <= now) {
            self.pane_repeat = None;
        }
        if self.note.as_ref().is_some_and(|n| n.until <= now) {
            self.note = None;
        }
        if let Some(player) = &mut self.player {
            player.step(now);
        }
    }

    /// The session a new pane on the focused pane's host uses: the focused
    /// pane's own when it is alive, else the host's live one, else a new
    /// connection.
    fn session_for_split(&mut self, alias: &str, current: Option<SessionId>) -> Option<SessionId> {
        match current {
            Some(id) if self.session_by_id(id).is_some_and(reusable) => Some(id),
            _ => self.session_or_connect(alias),
        }
    }

    /// Splits the focused pane of `index` with a new pane on `alias`.
    fn split_with(&mut self, index: usize, split: Split, alias: String, session: SessionId) {
        let area = self.body_area();
        let id = PaneId::next();
        let slot = PaneSlot::Opening {
            alias: alias.clone(),
            session,
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if let Err((refusal, _)) = tab.split(area, split, id, slot) {
            self.refuse(refusal);
            self.release_session(session);
            return;
        }
        // An older refusal no longer applies.
        self.note = None;
        let size = self.pane_size(index, id);
        self.focus = Focus::Terminal;
        self.request_terminal(alias, session, Some(id), size);
    }

    /// `"` / `%`: a new pane on the focused pane's host. A zoomed tab is
    /// unzoomed, then split (the split is judged on the unzoomed layout).
    pub(crate) fn split_pane(&mut self, split: Split) {
        self.prefix = false;
        let Some(index) = self.active else { return };
        let area = self.body_area();
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if let Err(refusal) = tab.can_split(area, split) {
            self.refuse(refusal);
            return;
        }
        let Some((alias, current)) = tab.focused().map(|s| (s.alias().to_owned(), s.session()))
        else {
            return;
        };
        let Some(session) = self.session_for_split(&alias, current) else {
            return;
        };
        self.split_with(index, split, alias, session);
    }

    /// `S`: pick a host in the list, then split onto it.
    pub(crate) fn split_pane_host(&mut self) {
        self.prefix = false;
        let Some(index) = self.active else { return };
        let area = self.body_area();
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if let Err(refusal) = best_split(tab, area) {
            self.refuse(refusal);
            return;
        }
        let pane = tab.focus();
        self.go_home();
        self.close_panels();
        self.overlay = None;
        // Type the host's name at once: the search field has the focus,
        // over the whole list.
        let query = std::mem::take(&mut self.query);
        self.refilter();
        self.focus = Focus::Search;
        self.pane_pick = Some(PanePick { pane, query });
    }

    /// Ends the pick: the list gets its search back. The pane to split.
    pub(crate) fn end_pick(&mut self) -> Option<PaneId> {
        let pick = self.pane_pick.take()?;
        self.query = pick.query;
        self.refilter();
        Some(pick.pane)
    }

    /// `↵` while picking: the new pane connects to the selected host,
    /// reusing its live session when there is one.
    pub(crate) fn pick_host(&mut self) {
        let Some(alias) = self.selected_host().map(|h| h.alias.clone()) else {
            return;
        };
        let Some(pane) = self.end_pick() else {
            return;
        };
        let Some(index) = self.tab_of(pane) else {
            self.notify_warning(
                "The pane to split is gone".into(),
                Some("Its tab was closed. Open a terminal, then split again.".into()),
            );
            return;
        };
        self.show_pane(index, pane);
        let area = self.body_area();
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let split = match best_split(tab, area) {
            Ok(split) => split,
            Err(refusal) => {
                self.refuse(refusal);
                return;
            }
        };
        let Some(session) = self.session_or_connect(&alias) else {
            return;
        };
        self.split_with(index, split, alias, session);
    }

    /// `Esc` while picking: back to the pane, nothing split.
    pub(crate) fn cancel_pick(&mut self) {
        if let Some(pane) = self.end_pick()
            && let Some(index) = self.tab_of(pane)
        {
            self.show_pane(index, pane);
        }
    }

    pub(crate) fn focus_pane(&mut self, dir: Dir) {
        self.prefix = false;
        let area = self.body_area();
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            tab.focus_dir(area, dir);
        }
    }

    pub(crate) fn next_pane(&mut self) {
        self.prefix = false;
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            tab.focus_next();
        }
    }

    pub(crate) fn resize_pane(&mut self, dir: Dir) {
        self.prefix = false;
        let area = self.body_area();
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            tab.resize(area, dir);
        }
    }

    pub(crate) fn zoom_pane(&mut self) {
        self.prefix = false;
        if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
            tab.toggle_zoom();
        }
    }

    /// `x`: closes the focused pane (its PTY channel); the last pane closes
    /// the tab, and a session no pane uses any more is disconnected.
    pub(crate) fn close_pane(&mut self) {
        self.prefix = false;
        let Some(index) = self.active else { return };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let focus = tab.focus();
        let Some(mut slot) = tab.remove(focus) else {
            return;
        };
        let empty = tab.is_empty();
        if let Some(view) = slot.view_mut() {
            view.close();
        }
        if empty {
            self.tabs.remove(index);
            if self.tabs.is_empty() {
                self.go_home();
            } else {
                self.activate(index.min(self.tabs.len() - 1));
            }
        }
        if let Some(session) = slot.session() {
            self.release_session(session);
        }
    }

    /// `w`: closes the active tab and every pane in it.
    pub(crate) fn close_active_tab(&mut self) {
        let Some(index) = self.active else { return };
        if index >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(index);
        let mut sessions = Vec::new();
        for (_, slot) in tab.items_mut() {
            if let Some(view) = slot.view_mut() {
                view.close();
            }
            sessions.extend(slot.session());
        }
        sessions.sort_unstable();
        sessions.dedup();
        for session in sessions {
            self.release_session(session);
        }
        if self.tabs.is_empty() {
            self.go_home();
        } else {
            self.activate(index.min(self.tabs.len() - 1));
        }
    }

    /// Why the pane `slot` is down, and whether `↵` may reopen it. `None`
    /// while it is alive or opening, or after a plain shell exit.
    pub(crate) fn pane_failure(&self, slot: &PaneSlot) -> Option<PaneFailure> {
        let session_error = slot
            .session()
            .and_then(|id| self.session_by_id(id))
            .and_then(|view| match &view.phase {
                SessionPhase::Failed { error } => Some(error.clone()),
                _ => None,
            });
        let error = match slot {
            PaneSlot::Opening { .. } => return None,
            PaneSlot::Failed { error, .. } => session_error.or_else(|| Some(error.clone())),
            PaneSlot::Live(view) => match &view.ended {
                // The connection, not the shell, ended: worth a retry.
                Some(Ended::Closed) => session_error,
                _ => return None,
            },
        };
        let retry = !error.as_ref().is_some_and(crate::files::refuses_reconnect);
        Some(PaneFailure { error, retry })
    }

    /// `↵` in a pane whose connection failed or closed: reconnect if needed
    /// and open a new shell in the same pane.
    pub(crate) fn retry_pane(&mut self) {
        let Some(index) = self.active else { return };
        let Some(slot) = self.tabs.get(index).and_then(Tab::focused) else {
            return;
        };
        if !self.pane_failure(slot).is_some_and(|f| f.retry) {
            return;
        }
        let alias = slot.alias().to_owned();
        let current = slot.session();
        let Some(session) = self.session_for_split(&alias, current) else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let pane = tab.focus();
        if let Some(target) = tab.focused_mut() {
            // The old view (if any) closes its channel when dropped.
            *target = PaneSlot::Opening {
                alias: alias.clone(),
                session,
            };
        }
        let size = self.pane_size(index, pane);
        self.request_terminal(alias, session, Some(pane), size);
    }

    // ── sizes ──────────────────────────────────────────────────────────────

    /// Keeps every pane's grid (and, debounced, its PTY) at its drawn size.
    pub(crate) fn sync_terminal_sizes(&mut self) {
        let area = self.body_area();
        for tab in &mut self.tabs {
            let sizes = tab.grid_sizes(area);
            for (id, slot) in tab.items_mut() {
                let Some(view) = slot.view_mut() else {
                    continue;
                };
                if let Some((_, cols, rows)) = sizes.iter().find(|(p, ..)| *p == id) {
                    view.resize(GridSize {
                        cols: *cols,
                        rows: *rows,
                    });
                }
            }
        }
    }

    pub(crate) fn resizes_pending(&self) -> bool {
        self.views().any(TerminalView::resize_pending)
    }

    /// Sends the PTY sizes that have been still long enough.
    pub(crate) fn flush_resizes(&mut self) {
        let now = Instant::now();
        for view in self.views_mut() {
            view.flush_resize(now);
        }
    }
}

#[cfg(test)]
mod tests;
