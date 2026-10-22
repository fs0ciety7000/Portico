//! App glue for session recording (phase 6): "Record session" / "Stop
//! recording" on the focused pane, the REC indicator, the "Recordings"
//! panel and the player.
//!
//! - Starting asks first (an armed confirmation: only `y` records). The
//!   REC badge turns on with `RecordingState::Started` and off with
//!   `Stopped`, never on the answer of `start_recording`; after a lagged
//!   engine bus every open terminal is resynchronised with `is_recording`.
//! - A requested stop says where the file went, on one neutral line of
//!   the status bar ("Recording saved · <file>", how to open Recordings).
//!   A recording that stops for another reason than a request or the
//!   terminal closing says so as a warning, with a hint.
//! - Engine errors are shown through their `ErrorSummary` only (never
//!   matched on text). Recorded output is never logged or shown outside
//!   the player.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::events::RecordingState;
use portico_core::ids::TerminalId;
use portico_core::recording::RecordingStopReason;

use crate::action::Action;
use crate::app::{App, Focus, Overlay};
use crate::app_panes::{NoteTone, PaneSlot, StatusNote};
use crate::dialog::{ConfirmAction, ConfirmDialog, Dialog};
use crate::keys::{Load, Notice};
use crate::player::{Player, PlayerState, Seek};
use crate::recordings::{RecOutcome, RecordingsPanel};
use crate::ui::fmt;

/// How long a recording notice stays in the status bar (longer than a
/// refused split: it reports something that happened on its own).
pub(crate) const RECORDING_NOTE_TTL: Duration = Duration::from_secs(8);
/// How long "Recording saved" stays in the status bar.
pub(crate) const SAVED_NOTE_TTL: Duration = Duration::from_secs(4);
/// The recordings folder, at most this wide in the confirmation (a middle
/// ellipsis keeps both ends).
const FOLDER_WIDTH: usize = 48;

/// "Stop recording" (the command name shared with the GUI) as a status-bar
/// hint: hints are lower case.
pub(crate) const STOP_HINT: &str = "stop recording";
/// Title of the start confirmation (shared with the GUI).
pub(crate) const CONFIRM_TITLE: &str = "Record this terminal?";
/// The same, when the tab is split (shared with the GUI).
pub(crate) const CONFIRM_TITLE_PANE: &str = "Record this pane?";

/// The start confirmation's text (shared with the GUI).
pub(crate) fn confirm_body(folder: &str) -> String {
    format!(
        "Everything the terminal shows is recorded, including the commands you type (the shell echoes them) and any secret a command prints. Keystrokes themselves are not stored, so a password typed at a prompt that does not echo stays out. Saved to {folder}, readable only by you."
    )
}

/// The neutral notice of a requested stop: `(message, hint)`.
/// `open_key`: the effective `Ctrl-Space` key of "Recordings".
pub(crate) fn saved_notice(name: &str, open_key: &str) -> (String, String) {
    (
        format!("Recording saved · {name}"),
        format!("Ctrl-Space {open_key} to open Recordings"),
    )
}

/// The recordings folder as shown: `~` for `$HOME`, a trailing `/`, the
/// middle cut when long.
pub(crate) fn folder_label(dir: &std::path::Path, width: usize) -> String {
    let folder = format!("{}/", fmt::display_path(&dir.display().to_string()));
    fmt::truncate_middle(&folder, width)
}

/// The status-bar notice of a recording that stopped on its own:
/// `(message, hint)`. `None` for a requested stop or a closed terminal.
pub(crate) fn stop_notice(reason: RecordingStopReason) -> Option<(&'static str, &'static str)> {
    match reason {
        RecordingStopReason::QueueFull => Some((
            "Recording stopped: the disk could not keep up",
            "What was written is kept. Free disk space or use a faster disk, then record again.",
        )),
        RecordingStopReason::WriteFailed => Some((
            "Recording stopped: the file could not be written",
            "Check the free disk space and the recordings folder, then record again.",
        )),
        RecordingStopReason::SizeLimit => Some((
            "Recording stopped: size limit reached (512 MiB)",
            "The file is complete up to the limit. Start a new recording to go on.",
        )),
        _ => None,
    }
}

/// Spawns `fut` and sends its result back as an action.
fn spawn<F>(app: &App, fut: F)
where
    F: Future<Output = Action> + Send + 'static,
{
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(fut.await);
    });
}

impl App {
    /// Recording actions (the rest is in `App::update` and the other glue
    /// modules).
    pub(crate) fn update_recording(&mut self, action: Action) {
        match action {
            Action::EngineLagged => {
                self.resync_recordings();
                // A `HostsChanged` may have been dropped with the rest.
                self.on_hosts_changed();
            }
            Action::ToggleRecording => self.toggle_recording(),
            // Success says nothing (falls through): the badge waits for
            // `Started`.
            Action::RecordingStart(Err(error)) => self.recording_note(error),
            Action::OpenRecordings => self.open_recordings(),
            Action::RecordingsKey(key) => self.recordings_key(key),
            Action::RecordingsLoaded(result) => {
                if let Some(panel) = &mut self.recordings_panel {
                    panel.loaded(result);
                }
            }
            Action::RecordingDeleted { name, result } => {
                if let Some(panel) = &mut self.recordings_panel {
                    panel.refused = None;
                    panel.notice = Some(match result {
                        Ok(()) => Notice::ok(format!("Deleted {}", fmt::truncate(&name, 48))),
                        Err(error) => Notice::error(error),
                    });
                }
                self.reload_recordings();
            }
            Action::PlaybackOpened {
                name,
                total,
                result,
            } => self.playback_opened(name, total, result),
            Action::PlayerKey(key) => self.player_key(key),
            Action::PlaybackSeek { generation, result } => {
                let now = Instant::now();
                if let Some(player) = &mut self.player {
                    match result {
                        // A stale start is dropped (its reader stops).
                        Ok(start) => {
                            player.restart(generation, start, now);
                        }
                        Err(error) => player.seek_failed(generation, error),
                    }
                }
            }
            Action::PlayerFeed(msg) => {
                if let Some(player) = &mut self.player {
                    player.feed(msg, Instant::now());
                }
            }
            other => self.update_hosts(other),
        }
    }

    /// `terminal` is being recorded, as the engine's events said.
    pub(crate) fn is_recorded(&self, terminal: TerminalId) -> bool {
        self.recording.contains_key(&terminal)
    }

    /// `RecordingState` of `terminal`: the only switch of the REC badge.
    pub(crate) fn on_recording_event(&mut self, terminal: TerminalId, state: RecordingState) {
        match state {
            RecordingState::Started { name, .. } => {
                self.recording.insert(terminal, Some(name));
            }
            RecordingState::Stopped { reason, name, .. } => {
                self.recording.remove(&terminal);
                if let Some((message, hint)) = stop_notice(reason) {
                    self.note = Some(StatusNote {
                        message: message.to_owned(),
                        hint: hint.to_owned(),
                        until: Instant::now() + RECORDING_NOTE_TTL,
                        tone: NoteTone::Warning,
                    });
                } else if reason == RecordingStopReason::Requested {
                    let open_key = self.keymap.shown("prefix.recordings", 1);
                    let (message, hint) = saved_notice(&name, &open_key);
                    self.note = Some(StatusNote {
                        message,
                        hint,
                        until: Instant::now() + SAVED_NOTE_TTL,
                        tone: NoteTone::Neutral,
                    });
                }
            }
            // A future state: the next resync tells.
            _ => {}
        }
        // Sizes, durations and the "recording" marks changed.
        self.sync_panel_active();
        self.reload_recordings();
    }

    /// The panel knows which files are being written (their delete is
    /// refused up front).
    fn sync_panel_active(&mut self) {
        let active: Vec<String> = self.recording.values().flatten().cloned().collect();
        if let Some(panel) = &mut self.recordings_panel {
            panel.active = active;
        }
    }

    /// After a lagged bus: every open terminal is asked again.
    fn resync_recordings(&mut self) {
        let ids: Vec<TerminalId> = self.views().map(|v| v.id).collect();
        self.recording.retain(|id, _| ids.contains(id));
        for id in ids {
            if self.engine.is_recording(id) {
                self.recording.entry(id).or_insert(None);
            } else {
                self.recording.remove(&id);
            }
        }
        self.sync_panel_active();
        self.reload_recordings();
    }

    /// An engine refusal, on one line of the status bar (never over the
    /// pane the next keys go to).
    fn recording_note(&mut self, error: ErrorSummary) {
        self.note = Some(StatusNote {
            message: fmt::sentence(&error.message),
            hint: error.hint.unwrap_or_default(),
            until: Instant::now() + RECORDING_NOTE_TTL,
            tone: NoteTone::Warning,
        });
    }

    /// `Ctrl-Space R`: "Record session" (asks first) or, when the focused
    /// pane is being recorded, "Stop recording" (at once).
    fn toggle_recording(&mut self) {
        self.prefix = false;
        let slot = self
            .focused_slot()
            .filter(|_| self.focus == Focus::Terminal && self.overlay.is_none());
        let Some((terminal, live)) = slot.and_then(|s| s.view().map(|v| (v.id, s.live()))) else {
            self.note = Some(StatusNote {
                message: "No terminal to record".into(),
                hint: "Open a terminal tab, then Ctrl-Space R.".into(),
                until: Instant::now() + crate::app_panes::NOTE_TTL,
                tone: NoteTone::Warning,
            });
            return;
        };
        if self.is_recorded(terminal) || self.engine.is_recording(terminal) {
            if let Err(error) = self.engine.stop_recording(terminal) {
                self.recording_note(error.summary());
            }
            return;
        }
        if !live {
            self.note = Some(StatusNote {
                message: "This pane's shell has ended".into(),
                hint: "Only a live terminal can be recorded.".into(),
                until: Instant::now() + crate::app_panes::NOTE_TTL,
                tone: NoteTone::Warning,
            });
            return;
        }
        let folder = folder_label(&self.engine.recordings_dir(), FOLDER_WIDTH);
        // A split tab: which pane, by its host and its place.
        let (title, subject) = match self.active_tab().filter(|t| t.len() > 1) {
            Some(tab) => {
                let number = tab
                    .ids()
                    .iter()
                    .position(|id| *id == tab.focus())
                    .map_or(1, |i| i + 1);
                let alias = tab.focused().map(PaneSlot::alias).unwrap_or_default();
                (
                    CONFIRM_TITLE_PANE,
                    Some(format!(
                        "{} · pane {number}/{}",
                        fmt::truncate(alias, 32),
                        tab.len()
                    )),
                )
            }
            None => (CONFIRM_TITLE, None),
        };
        self.dialogs.push_back(Dialog::Confirm(ConfirmDialog {
            title: title.into(),
            subject,
            body: confirm_body(&folder),
            confirm: "record",
            action: ConfirmAction::StartRecording(terminal),
            armed: Some(Instant::now()),
        }));
    }

    /// The confirmation said `y`.
    pub(crate) fn start_recording(&self, terminal: TerminalId) {
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine
                .start_recording(terminal)
                .await
                .map(|_| ())
                .map_err(|e| e.summary());
            Action::RecordingStart(result)
        });
    }

    // ── panel ──────────────────────────────────────────────────────────────

    fn open_recordings(&mut self) {
        self.prefix = false;
        self.close_panels();
        // "Recording saved · … to open Recordings" was just followed: it
        // gives the bar back to the panel's keys.
        if self
            .note
            .as_ref()
            .is_some_and(|n| n.tone == NoteTone::Neutral)
        {
            self.note = None;
        }
        let folder = folder_label(&self.engine.recordings_dir(), usize::MAX);
        self.recordings_panel = Some(RecordingsPanel::new(folder));
        self.overlay = Some(Overlay::Recordings);
        self.sync_panel_active();
        self.reload_recordings();
    }

    /// Lists the recordings again (when the panel is open).
    fn reload_recordings(&self) {
        if self.recordings_panel.is_none() {
            return;
        }
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::RecordingsLoaded(engine.list_recordings().await.map_err(|e| e.summary()))
        });
    }

    fn recordings_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.recordings_panel else {
            return;
        };
        match panel.handle_key(key) {
            RecOutcome::None => {}
            RecOutcome::Close => {
                self.overlay = None;
                self.recordings_panel = None;
            }
            RecOutcome::Reload => {
                panel.entries = Load::Loading;
                self.reload_recordings();
            }
            RecOutcome::Play(name, total) => {
                panel.opening = true;
                let engine = self.engine.clone();
                spawn(self, async move {
                    let result = engine
                        .open_recording(&name)
                        .await
                        .map_err(|e| e.summary())
                        .and_then(crate::player::spawn_reader);
                    Action::PlaybackOpened {
                        name,
                        total,
                        result,
                    }
                });
            }
            RecOutcome::Delete(name) => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    let result = engine
                        .delete_recording(&name)
                        .await
                        .map_err(|e| e.summary());
                    Action::RecordingDeleted { name, result }
                });
            }
        }
    }

    fn playback_opened(
        &mut self,
        name: String,
        total: Option<f64>,
        result: Result<crate::player::PlaybackStart, ErrorSummary>,
    ) {
        // The panel was closed meanwhile: dropping the start stops its
        // reader thread.
        let Some(panel) = self
            .recordings_panel
            .as_mut()
            .filter(|_| self.overlay == Some(Overlay::Recordings))
        else {
            return;
        };
        panel.opening = false;
        match result {
            Ok(start) => {
                let mut player = Player::new(name, total, start);
                player.step(Instant::now());
                self.player = Some(player);
                self.overlay = Some(Overlay::Player);
            }
            Err(error) => panel.notice = Some(Notice::error(error)),
        }
    }

    // ── player ─────────────────────────────────────────────────────────────

    /// Player controls. No key is ever forwarded anywhere.
    fn player_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let now = Instant::now();
        let Some(player) = &mut self.player else {
            return;
        };
        let seek = match key.code {
            // Ended: Space plays it again from the start.
            KeyCode::Char(' ') if !ctrl && player.state() == PlayerState::Ended => {
                Some(Seek::Restart)
            }
            KeyCode::Char(' ') if !ctrl => {
                player.toggle_pause(now);
                None
            }
            KeyCode::Char('+' | '=') if !ctrl => {
                player.change_speed(true, now);
                None
            }
            KeyCode::Char('-' | '_') if !ctrl => {
                player.change_speed(false, now);
                None
            }
            KeyCode::Right => {
                player.skip_idle(now);
                None
            }
            KeyCode::Left => Some(Seek::Back),
            KeyCode::Home => Some(Seek::Restart),
            KeyCode::Char('q') | KeyCode::Esc => {
                self.close_player();
                return;
            }
            _ => None,
        };
        if let Some(seek) = seek {
            self.seek_player(seek);
        }
        if let Some(player) = &mut self.player {
            player.step(now);
        }
    }

    /// Reopens the playing file for a seek (off the UI thread); the player
    /// starts over once it is open ([`Action::PlaybackSeek`]).
    fn seek_player(&mut self, seek: Seek) {
        let Some(player) = &mut self.player else {
            return;
        };
        let generation = player.request_seek(seek);
        let name = player.name.clone();
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine
                .open_recording(&name)
                .await
                .map_err(|e| e.summary())
                .and_then(crate::player::spawn_reader);
            Action::PlaybackSeek { generation, result }
        });
    }

    /// Back to the recordings list (the reader thread stops).
    fn close_player(&mut self) {
        self.player = None;
        self.overlay = self.recordings_panel.as_ref().map(|_| Overlay::Recordings);
        self.reload_recordings();
    }

    /// The pane's terminal is being recorded (REC badge).
    pub(crate) fn recorded_in(&self, slot: &PaneSlot) -> bool {
        slot.view().is_some_and(|v| self.is_recorded(v.id))
    }
}

#[cfg(test)]
mod tests;
