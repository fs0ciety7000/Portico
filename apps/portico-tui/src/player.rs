//! Playback of a session recording (asciicast v2, phase 6).
//!
//! A recording file is **untrusted** (it may come from anyone). Invariants:
//!
//! - **Its own emulator**: each playback has a dedicated
//!   `alacritty_terminal::Term`, sized from the header and the `r` events
//!   (clamped to [`MAX_PLAYBACK_DIMENSION`]), never shared with a live
//!   terminal. Its listener is [`PlayerListener`], which drops every
//!   emulator event: replies to queries (`PtyWrite`: DSR, DA…), titles,
//!   clipboard stores and loads go nowhere. OSC 52 is disabled in its
//!   config, and the kitty keyboard protocol is off.
//! - **Modes stay inside**: mouse reporting, bracketed paste, focus events
//!   or the alternate screen set by a recording only change this emulator's
//!   `TermMode`, which Portico reads for nothing but drawing; the host
//!   terminal's real modes (crossterm) are never touched from here.
//! - **No input**: the player holds no `TerminalInputs`; keys only drive
//!   the controls, and pastes are dropped by the app while it is open.
//! - **One sanitiser**: every `Output` goes through one
//!   [`PlaybackSanitizer`] for the whole playback (sequences split across
//!   events stay neutralised), and `finish()` is called at the end.
//! - **Off the UI thread**: the blocking [`CastReader`] is iterated on a
//!   dedicated thread (`portico-play`) that hands events to the UI loop
//!   through a **bounded** channel ([`PLAYBACK_QUEUE`]). The UI keeps one
//!   event ahead and waits for its time ([`PlaybackTiming`]): no redraw
//!   while nothing is due, none at all while paused.
//! - Output text is never logged, toasted or put in an error: errors are
//!   the engine's `ErrorSummary`.

use std::io::BufRead;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
#[cfg(test)]
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::{Config, Osc52, Term};
use alacritty_terminal::vte::ansi::Processor;
use portico_core::ErrorSummary;
use portico_core::recording::{
    CastEvent, CastHeader, CastReader, MAX_PLAYBACK_DIMENSION, PlaybackSanitizer, PlaybackTiming,
    TimedEvent,
};
use tokio::sync::mpsc;

use crate::terminal::GridSize;

/// Events buffered between the reader thread and the UI loop.
pub(crate) const PLAYBACK_QUEUE: usize = 256;
/// Most events applied in one step (a burst of zero-delay events): the UI
/// stays responsive, the rest follows on the next wake-up.
const MAX_EVENTS_PER_STEP: usize = 512;
/// Scrollback of a playback emulator.
const PLAYBACK_SCROLLBACK: usize = 1_000;
/// Speeds offered by `+` / `-`.
pub(crate) const SPEEDS: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];
/// Name of the reader thread.
const READER_THREAD: &str = "portico-play";

/// The playback emulator's listener: **drops every event**. There is no
/// PTY behind a recording, so replies to queries (DSR, DA, colour and size
/// requests, clipboard loads) must go nowhere; a file sets no title and
/// writes no clipboard. Never the live terminals' `EventProxy`, which
/// forwards `PtyWrite` to the remote shell.
#[derive(Debug, Clone, Default)]
pub(crate) struct PlayerListener {
    /// Replies the emulator produced (and that were dropped): tests prove
    /// the queries were answered by alacritty, and the answers lost.
    #[cfg(test)]
    pub(crate) dropped_replies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl EventListener for PlayerListener {
    // Each kind is named on purpose: this is the list of what a playback
    // drops, whatever the arm bodies look like.
    #[allow(clippy::match_same_arms)]
    fn send_event(&self, event: TermEvent) {
        match event {
            // Replies for a PTY that does not exist.
            TermEvent::PtyWrite(_)
            | TermEvent::ColorRequest(..)
            | TermEvent::TextAreaSizeRequest(_)
            | TermEvent::ClipboardLoad(..) => {
                #[cfg(test)]
                self.dropped_replies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // A third-party file sets no title and no clipboard.
            TermEvent::Title(_) | TermEvent::ResetTitle | TermEvent::ClipboardStore(..) => {}
            // Bell, wake-ups, cursor blinking: nothing to do.
            _ => {}
        }
    }
}

/// What the reader thread sends.
#[derive(Debug)]
pub(crate) enum ReaderMsg {
    /// `TimedEvent`'s `Debug` never shows output text.
    Event(TimedEvent),
    /// End of file, or the error that ended the reading (what came before
    /// it was played).
    End(Option<ErrorSummary>),
}

/// A recording ready to play: its header and the reader's channel.
pub(crate) struct PlaybackStart {
    pub(crate) header: CastHeader,
    rx: mpsc::Receiver<ReaderMsg>,
}

impl std::fmt::Debug for PlaybackStart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackStart")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

/// Starts the reader thread of `reader` (blocking reads, off the runtime).
/// The thread ends at the end of the file, on an error, or as soon as the
/// player is dropped (its channel closes).
pub(crate) fn spawn_reader<R>(reader: CastReader<R>) -> Result<PlaybackStart, ErrorSummary>
where
    R: BufRead + Send + 'static,
{
    let header = reader.header().clone();
    let (tx, rx) = mpsc::channel(PLAYBACK_QUEUE);
    std::thread::Builder::new()
        .name(READER_THREAD.to_owned())
        .spawn(move || {
            for item in reader {
                let msg = match item {
                    Ok(event) => ReaderMsg::Event(event),
                    Err(error) => {
                        let _ = tx.blocking_send(ReaderMsg::End(Some(error.summary())));
                        return;
                    }
                };
                // Waits while the UI is behind (bounded); stops once the
                // player is gone.
                if tx.blocking_send(msg).is_err() {
                    return;
                }
            }
            let _ = tx.blocking_send(ReaderMsg::End(None));
        })
        .map_err(|_| ErrorSummary {
            code: "playback_thread".into(),
            message: "cannot start the playback reader".into(),
            hint: Some(
                "The system is out of threads or memory: close something, then retry.".into(),
            ),
        })?;
    Ok(PlaybackStart { header, rx })
}

/// When the next event shows.
#[derive(Debug, Clone, Copy)]
enum Due {
    At(Instant),
    /// Paused: what was left to wait.
    Left(Duration),
}

/// The next event, waiting for its time.
#[derive(Debug)]
struct Head {
    event: TimedEvent,
    due: Due,
}

/// Where the playback is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayerState {
    /// Nothing shown yet.
    Loading,
    Playing,
    Paused,
    Ended,
}

/// How far the playback got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// No event shown yet.
    Waiting,
    Showing,
    /// The stream ended (the sanitiser was finished).
    Done,
}

/// One playback.
pub(crate) struct Player {
    /// File name (from the listing): always shown, since the header title
    /// of a third-party file can say anything.
    pub(crate) name: String,
    /// Header title, as sanitised by the engine.
    pub(crate) title: Option<String>,
    term: Term<PlayerListener>,
    parser: Processor,
    sanitizer: PlaybackSanitizer,
    timing: PlaybackTiming,
    pub(crate) size: GridSize,
    head: Option<Head>,
    paused: bool,
    /// Time (in the recording) of the last event shown.
    pub(crate) position: f64,
    /// Duration from the listing, then the last event's time once read.
    pub(crate) total: Option<f64>,
    rx: mpsc::Receiver<ReaderMsg>,
    reading: bool,
    stage: Stage,
    /// The error that ended the reading early.
    pub(crate) error: Option<ErrorSummary>,
    #[cfg(test)]
    pub(crate) listener: PlayerListener,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("name", &self.name)
            .field("size", &self.size)
            .field("position", &self.position)
            .field("paused", &self.paused)
            .finish_non_exhaustive()
    }
}

/// A header or `r` size, made safe for the emulator.
fn clamp_size(cols: u16, rows: u16) -> GridSize {
    GridSize {
        cols: cols.clamp(1, MAX_PLAYBACK_DIMENSION),
        rows: rows.clamp(1, MAX_PLAYBACK_DIMENSION),
    }
}

impl Player {
    pub(crate) fn new(name: String, total: Option<f64>, start: PlaybackStart) -> Self {
        let PlaybackStart { header, rx } = start;
        let size = clamp_size(header.width, header.height);
        let listener = PlayerListener::default();
        let config = Config {
            scrolling_history: PLAYBACK_SCROLLBACK,
            // No clipboard access from a file (the sanitiser drops OSC 52
            // too).
            osc52: Osc52::Disabled,
            kitty_keyboard: false,
            ..Config::default()
        };
        #[cfg(test)]
        let term = Term::new(config, &size, listener.clone());
        #[cfg(not(test))]
        let term = Term::new(config, &size, listener);
        Self {
            name,
            title: header.title.filter(|t| !t.trim().is_empty()),
            term,
            parser: Processor::new(),
            sanitizer: PlaybackSanitizer::new(),
            timing: PlaybackTiming::new(1.0),
            size,
            head: None,
            paused: false,
            position: 0.0,
            total: total.filter(|t| t.is_finite() && *t >= 0.0),
            rx,
            reading: true,
            stage: Stage::Waiting,
            error: None,
            #[cfg(test)]
            listener,
        }
    }

    pub(crate) fn term(&self) -> &Term<PlayerListener> {
        &self.term
    }

    /// The playback emulator's modes (they never leave it).
    #[cfg(test)]
    pub(crate) fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub(crate) fn state(&self) -> PlayerState {
        match self.stage {
            Stage::Done => PlayerState::Ended,
            _ if self.paused => PlayerState::Paused,
            Stage::Showing => PlayerState::Playing,
            Stage::Waiting => PlayerState::Loading,
        }
    }

    pub(crate) fn speed(&self) -> f64 {
        self.timing.speed()
    }

    /// The UI loop should wait on [`Self::recv`]: no event is waiting for
    /// its time, and the reader has more.
    pub(crate) fn wants_input(&self) -> bool {
        self.reading && self.head.is_none()
    }

    /// The reader's next message (cancel-safe).
    pub(crate) async fn recv(&mut self) -> Option<ReaderMsg> {
        self.rx.recv().await
    }

    /// When the waiting event is due (`None`: paused, or nothing waits).
    pub(crate) fn next_due(&self) -> Option<Instant> {
        match self.head.as_ref().map(|h| h.due) {
            Some(Due::At(at)) if !self.paused => Some(at),
            _ => None,
        }
    }

    /// A message from the reader (or `None`: it is gone), then whatever
    /// is due at `now`.
    pub(crate) fn feed(&mut self, msg: Option<ReaderMsg>, now: Instant) {
        self.accept(msg, now);
        self.step(now);
    }

    fn accept(&mut self, msg: Option<ReaderMsg>, now: Instant) {
        match msg {
            Some(ReaderMsg::Event(event)) => {
                let delay = self.timing.delay_until(event.time);
                let due = if self.paused {
                    Due::Left(delay)
                } else {
                    Due::At(now + delay)
                };
                self.head = Some(Head { event, due });
            }
            Some(ReaderMsg::End(error)) => {
                self.reading = false;
                self.error = error;
            }
            None => self.reading = false,
        }
    }

    /// Shows every event due at `now` (bounded per step), pulling what the
    /// reader already sent. Returns whether the screen changed.
    pub(crate) fn step(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for _ in 0..MAX_EVENTS_PER_STEP {
            if self.head.is_none() {
                if !self.reading {
                    changed |= self.finish();
                    break;
                }
                match self.rx.try_recv() {
                    Ok(msg) => self.accept(Some(msg), now),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => self.accept(None, now),
                }
                continue;
            }
            let due = matches!(
                self.head.as_ref().map(|h| h.due),
                Some(Due::At(at)) if !self.paused && at <= now
            );
            if !due {
                break;
            }
            if let Some(head) = self.head.take() {
                self.apply(head.event);
                changed = true;
            }
        }
        changed
    }

    fn apply(&mut self, event: TimedEvent) {
        self.stage = Stage::Showing;
        self.position = self.position.max(event.time);
        match event.event {
            CastEvent::Output(text) => {
                let clean = self.sanitizer.sanitize(&text);
                self.parser.advance(&mut self.term, clean.as_bytes());
            }
            CastEvent::Resize { cols, rows } => {
                let size = clamp_size(cols, rows);
                if size != self.size {
                    self.size = size;
                    self.term.resize(size);
                }
            }
        }
    }

    /// End of the stream: the sanitiser drops any unfinished sequence.
    fn finish(&mut self) -> bool {
        if self.stage == Stage::Done {
            return false;
        }
        self.sanitizer.finish();
        self.stage = Stage::Done;
        self.total = Some(self.total.unwrap_or(0.0).max(self.position));
        true
    }

    /// Space: pause, or resume where it stopped.
    pub(crate) fn toggle_pause(&mut self, now: Instant) {
        if self.stage == Stage::Done {
            return;
        }
        self.paused = !self.paused;
        if let Some(head) = &mut self.head {
            head.due = match (head.due, self.paused) {
                (Due::At(at), true) => Due::Left(at.saturating_duration_since(now)),
                (Due::Left(left), false) => Due::At(now + left),
                (due, _) => due,
            };
        }
    }

    /// `+` / `-`: the next speed up or down (0.5 to 8). The wait in
    /// progress is rescaled.
    pub(crate) fn change_speed(&mut self, faster: bool, now: Instant) {
        let current = self.timing.speed();
        let next = if faster {
            SPEEDS.iter().copied().find(|s| *s > current + f64::EPSILON)
        } else {
            SPEEDS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < current - f64::EPSILON)
        };
        let Some(next) = next else { return };
        self.timing.set_speed(next);
        let ratio = current / self.timing.speed();
        if let Some(head) = &mut self.head {
            let scale =
                |d: Duration| Duration::try_from_secs_f64(d.as_secs_f64() * ratio).unwrap_or(d);
            head.due = match head.due {
                Due::At(at) => Due::At(now + scale(at.saturating_duration_since(now))),
                Due::Left(left) => Due::Left(scale(left)),
            };
        }
    }

    /// `→`: the silence in progress ends now.
    pub(crate) fn skip_idle(&mut self, now: Instant) {
        if let Some(head) = &mut self.head {
            head.due = if self.paused {
                Due::Left(Duration::ZERO)
            } else {
                Due::At(now)
            };
        }
    }
}

/// `1:05`, `12:30`, `1:02:03` (recording time, seconds).
pub(crate) fn clock(seconds: f64) -> String {
    let total = if seconds.is_finite() && seconds > 0.0 {
        // Display only: whole seconds, saturating.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = seconds.floor().min(f64::from(u32::MAX)) as u64;
        whole
    } else {
        0
    };
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// `×1`, `×0.5`, `×8`.
pub(crate) fn speed_label(speed: f64) -> String {
    if (speed - speed.round()).abs() < f64::EPSILON {
        format!("×{speed:.0}")
    } else {
        format!("×{speed}")
    }
}

/// Every mode a recording can turn on that would matter outside its
/// emulator (tests check none of them leaks).
#[cfg(test)]
pub(crate) const HOST_AFFECTING_MODES: TermMode = TermMode::MOUSE_MODE
    .union(TermMode::BRACKETED_PASTE)
    .union(TermMode::FOCUS_IN_OUT)
    .union(TermMode::ALT_SCREEN)
    .union(TermMode::KITTY_KEYBOARD_PROTOCOL);

#[cfg(test)]
pub(crate) mod tests;
