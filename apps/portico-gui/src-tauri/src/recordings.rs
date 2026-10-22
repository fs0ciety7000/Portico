//! Session recording (asciicast v2, phase 6): start and stop on a terminal
//! the view opened, the recordings list, and playback streamed to a
//! dedicated, read-only xterm.js instance.
//!
//! The file side lives in `portico_core::recording` (ADR "Enregistrement de
//! sessions, asciicast v2", docs/STACK.md). Here:
//!
//! - **Names only**: the webview designates a recording by the file name a
//!   listing gave it; it is checked with
//!   [`portico_core::recording::validate_name`] before any engine call (the
//!   core checks it again). No command takes or returns a full path: the
//!   folder is shown as a display string (`~/…`) built here.
//! - **Playback**: one blocking task per playback (`spawn_blocking`) iterates
//!   the `CastReader`, passes every `Output` through **one**
//!   `PlaybackSanitizer` for the whole playback (`finish()` when the stream
//!   ends or restarts), applies `PlaybackTiming`, and sends already
//!   sanitised frames on a `tauri::ipc::Channel`. Pause, speed, skip and seek
//!   are decided here; the view only renders.
//! - **Back-pressure**: the view acknowledges the bytes xterm.js has parsed
//!   (`playback_ack`); the task stops reading while more than
//!   [`PLAYBACK_HIGH_WATER`] bytes are unacknowledged. Nothing is queued
//!   without bound, and closing the view (`playback_close`, a page reload, or
//!   a failed send) ends the task.
//! - **No terminal content in logs**: frames carrying output have a redacted
//!   `Debug`; errors cross as `ErrorSummary` only.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::fmt;
use std::io::BufRead;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use portico_core::ids::TerminalId;
use portico_core::recording::{
    CastEvent, CastReader, MAX_PLAYBACK_SPEED, MIN_PLAYBACK_SPEED, PlaybackSanitizer,
    PlaybackTiming, RecordingEntry, TimedEvent,
};
use portico_core::{Engine, ErrorSummary};
use serde::{Deserialize, Serialize};
use tauri::State;
use tauri::ipc::Channel;

use crate::dto::{display_path, escape_display, safe_summary, unix_ms};
use crate::terminals::{Terminals, next_ack};

/// Unacknowledged playback bytes allowed in flight towards the view.
pub(crate) const PLAYBACK_HIGH_WATER: u64 = 256 * 1024;
/// Largest output frame (bytes of UTF-8 text). Fast-forwarding coalesces
/// events up to this size.
const MAX_FRAME: usize = 64 * 1024;
/// Playbacks open at once (each holds a blocking thread).
const MAX_PLAYBACKS: usize = 4;
/// Longest alias shown in the list (a third-party file sets any title).
const DISPLAY_ALIAS: usize = 128;
/// Speeds closer than this are the same speed.
const SPEED_EPSILON: f64 = 1e-6;

// ── Names ───────────────────────────────────────────────────────────────

/// `name` as the core accepts it (a plain `*.cast` file name), or the
/// core's `recording_invalid_name` summary.
pub(crate) fn checked_name(name: &str) -> Result<&str, ErrorSummary> {
    portico_core::recording::validate_name(name)
        .map(|()| name)
        .map_err(|e| safe_summary(&portico_core::Error::Recording(e).summary()))
}

// ── DTOs ────────────────────────────────────────────────────────────────

/// A recording file, as listed (`portico_core::recording::RecordingEntry`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordingDto {
    /// File name: the only id the view passes back (play, delete).
    pub name: String,
    /// Header title (escaped: a third-party file sets any title), or the
    /// file name without its extension. Always shown next to `name`.
    pub alias: String,
    /// Unix milliseconds (header timestamp).
    pub started: Option<i64>,
    pub size: u64,
    /// Seconds (time of the last event), when it could be read cheaply.
    pub duration: Option<f64>,
    pub width: Option<u16>,
    pub height: Option<u16>,
    /// The header is a valid asciicast v2 header.
    pub valid: bool,
}

impl RecordingDto {
    /// `None` for a name the core would refuse (never listed by it).
    fn from_entry(e: &RecordingEntry) -> Option<Self> {
        checked_name(&e.name).ok()?;
        Some(Self {
            name: e.name.clone(),
            alias: escape_display(&e.alias, DISPLAY_ALIAS),
            started: e.started.map(unix_ms),
            size: e.size,
            duration: e.duration.filter(|d| d.is_finite() && *d >= 0.0),
            width: e.width,
            height: e.height,
            valid: e.valid,
        })
    }
}

/// What the player needs before the first frame.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaybackDto {
    pub id: u64,
    pub name: String,
    /// Header title, escaped.
    pub title: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub paused: bool,
    pub speed: f64,
}

/// Playback state after a control.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaybackStateDto {
    pub paused: bool,
    pub speed: f64,
}

/// A control sent by the player.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PlaybackControlDto {
    Pause,
    Resume,
    /// Speed, clamped to 0.5–8 (a non-finite value is refused).
    Speed {
        value: f64,
    },
    /// Ends the current silence now.
    Skip,
    /// Jumps to `time` seconds (clamped at 0): backwards replays from the
    /// start without delays, forwards fast-forwards.
    Seek {
        time: f64,
    },
}

/// One message of the playback channel. Output is already sanitised.
#[derive(Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PlaybackFrame {
    Output {
        data: String,
        /// UTF-8 bytes of `data`: what the view acknowledges.
        size: u64,
        time: f64,
    },
    Resize {
        cols: u16,
        rows: u16,
        time: f64,
    },
    /// A backwards seek: the view resets its emulator; frames replay from
    /// the start.
    Reset {
        time: f64,
    },
    /// Nothing more to play (a seek can restart it).
    End {
        time: f64,
    },
    /// The file could not be read further (what came before was played).
    Error {
        error: ErrorSummary,
    },
}

// Output is recorded terminal content: never in `Debug`.
impl fmt::Debug for PlaybackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output { size, time, .. } => write!(f, "Output({size} bytes @ {time})"),
            Self::Resize { cols, rows, time } => write!(f, "Resize({cols}x{rows} @ {time})"),
            Self::Reset { time } => write!(f, "Reset(@ {time})"),
            Self::End { time } => write!(f, "End(@ {time})"),
            Self::Error { error } => write!(f, "Error({})", error.code),
        }
    }
}

// ── Playback control (shared by the commands and the task) ─────────────

#[derive(Debug)]
struct Ctl {
    paused: bool,
    speed: f64,
    /// One-shot: end the current (or next) silence.
    skip: bool,
    seek: Option<f64>,
    /// Bytes sent to the view, and acknowledged by it.
    sent: u64,
    acked: u64,
    closed: bool,
}

/// State of one playback, shared between its task and the commands.
#[derive(Debug)]
pub(crate) struct Playback {
    ctl: Mutex<Ctl>,
    wake: Condvar,
}

fn clamp_speed(value: f64) -> Option<f64> {
    value
        .is_finite()
        .then(|| value.clamp(MIN_PLAYBACK_SPEED, MAX_PLAYBACK_SPEED))
}

impl Playback {
    fn new(speed: f64) -> Self {
        Self {
            ctl: Mutex::new(Ctl {
                paused: false,
                speed: clamp_speed(speed).unwrap_or(1.0),
                skip: false,
                seek: None,
                sent: 0,
                acked: 0,
                closed: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Ctl> {
        self.ctl.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self) -> PlaybackStateDto {
        let c = self.lock();
        PlaybackStateDto {
            paused: c.paused,
            speed: c.speed,
        }
    }

    /// Applies a control; a non-finite speed is refused.
    fn control(&self, control: PlaybackControlDto) -> Result<PlaybackStateDto, ErrorSummary> {
        {
            let mut c = self.lock();
            match control {
                PlaybackControlDto::Pause => c.paused = true,
                PlaybackControlDto::Resume => c.paused = false,
                PlaybackControlDto::Speed { value } => {
                    c.speed = clamp_speed(value).ok_or_else(|| ErrorSummary {
                        code: "playback_invalid_speed".into(),
                        message: "the playback speed must be a number".into(),
                        hint: None,
                    })?;
                }
                PlaybackControlDto::Skip => c.skip = true,
                PlaybackControlDto::Seek { time } => {
                    c.seek = Some(if time.is_finite() { time.max(0.0) } else { 0.0 });
                }
            }
        }
        self.wake.notify_all();
        Ok(self.state())
    }

    fn ack(&self, total: u64) {
        let mut c = self.lock();
        if let Some(next) = next_ack(c.acked, total, c.sent) {
            c.acked = next;
            drop(c);
            self.wake.notify_all();
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

/// Why a wait ended.
#[derive(Debug, PartialEq)]
enum Wake {
    Go,
    Closed,
    Seek(f64),
}

impl Playback {
    /// Waits `delay` (at the current speed), honouring pause, speed changes,
    /// skip, seek and close.
    fn wait(&self, mut delay: Duration, timing: &mut PlaybackTiming) -> Wake {
        let mut c = self.lock();
        loop {
            if c.closed {
                return Wake::Closed;
            }
            if let Some(t) = c.seek.take() {
                return Wake::Seek(t);
            }
            if (c.speed - timing.speed()).abs() > SPEED_EPSILON {
                // The rest of the silence, at the new speed.
                delay = delay.mul_f64(timing.speed() / c.speed);
                timing.set_speed(c.speed);
            }
            if c.skip {
                c.skip = false;
                return Wake::Go;
            }
            if c.paused {
                c = self.wake.wait(c).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            if delay.is_zero() {
                return Wake::Go;
            }
            let start = Instant::now();
            c = self
                .wake
                .wait_timeout(c, delay)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            delay = delay.saturating_sub(start.elapsed());
        }
    }

    /// Waits until the view has room for more (flow control), or the
    /// playback is closed (false).
    fn wait_room(&self) -> bool {
        let mut c = self.lock();
        while !c.closed && c.sent.saturating_sub(c.acked) > PLAYBACK_HIGH_WATER {
            c = self.wake.wait(c).unwrap_or_else(PoisonError::into_inner);
        }
        !c.closed
    }

    fn count_sent(&self, bytes: u64) {
        let mut c = self.lock();
        c.sent = c.sent.saturating_add(bytes);
    }

    /// After the end (or an error): wait for a seek (restart) or close.
    fn wait_after_end(&self) -> Option<f64> {
        let mut c = self.lock();
        loop {
            if c.closed {
                return None;
            }
            if let Some(t) = c.seek.take() {
                return Some(t);
            }
            c = self.wake.wait(c).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

// ── The playback loop (generic over the reader: testable without Tauri) ─

/// Sends frames, honouring flow control. False once the view is gone or
/// the playback closed.
struct Sender<'a, S> {
    playback: &'a Playback,
    sink: S,
    /// Output coalesced while fast-forwarding.
    batch: String,
    batch_time: f64,
}

impl<S: FnMut(PlaybackFrame) -> bool> Sender<'_, S> {
    fn send(&mut self, frame: PlaybackFrame) -> bool {
        if !self.playback.wait_room() {
            return false;
        }
        if let PlaybackFrame::Output { size, .. } = &frame {
            self.playback.count_sent(*size);
        }
        (self.sink)(frame)
    }

    fn output(&mut self, data: String, time: f64) -> bool {
        if data.is_empty() {
            return true;
        }
        let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        self.send(PlaybackFrame::Output { data, size, time })
    }

    fn push(&mut self, data: &str, time: f64) -> bool {
        self.batch.push_str(data);
        self.batch_time = time;
        self.batch.len() < MAX_FRAME || self.flush()
    }

    fn flush(&mut self) -> bool {
        let data = std::mem::take(&mut self.batch);
        self.output(data, self.batch_time)
    }

    /// Shows one event: output through the playback's sanitiser (coalesced
    /// while fast-forwarding), a resize after what came before it.
    fn event(
        &mut self,
        sanitizer: &mut PlaybackSanitizer,
        event: TimedEvent,
        coalesce: bool,
    ) -> bool {
        match event.event {
            CastEvent::Output(text) => {
                let clean = sanitizer.sanitize(&text);
                if coalesce {
                    self.push(&clean, event.time)
                } else {
                    self.output(clean, event.time)
                }
            }
            CastEvent::Resize { cols, rows } => {
                self.flush()
                    && self.send(PlaybackFrame::Resize {
                        cols,
                        rows,
                        time: event.time,
                    })
            }
        }
    }
}

/// Plays `reader` until the end, then waits for a seek or close. `reopen`
/// gives a fresh reader for a backwards seek. Every `Output` goes through
/// one `PlaybackSanitizer`, kept for the whole playback (`finish()` at each
/// end of stream and restart, so a sequence cut there is dropped).
pub(crate) fn run_playback<R, F, S>(
    mut reader: CastReader<R>,
    mut reopen: F,
    playback: &Playback,
    sink: S,
) where
    R: BufRead,
    F: FnMut() -> Result<CastReader<R>, ErrorSummary>,
    S: FnMut(PlaybackFrame) -> bool,
{
    let mut sanitizer = PlaybackSanitizer::new();
    let mut timing = PlaybackTiming::new(playback.state().speed);
    let mut out = Sender {
        playback,
        sink,
        batch: String::new(),
        batch_time: 0.0,
    };
    // Time of the last event shown, and the fast-forward target.
    let mut pos = 0.0_f64;
    let mut target: Option<f64> = None;
    let mut pending: Option<TimedEvent> = None;
    loop {
        if playback.is_closed() {
            return;
        }
        let next = match pending.take() {
            Some(event) => Some(Ok(event)),
            None => reader.next(),
        };
        let event = match next {
            Some(Ok(event)) => event,
            ended => {
                if !out.flush() {
                    return;
                }
                sanitizer.finish();
                let frame = match ended {
                    Some(Err(e)) => PlaybackFrame::Error {
                        error: safe_summary(&e.summary()),
                    },
                    _ => PlaybackFrame::End { time: pos },
                };
                if !out.send(frame) {
                    return;
                }
                let Some(t) = playback.wait_after_end() else {
                    return;
                };
                match restart(&mut reopen, &mut out, &mut sanitizer) {
                    Some(r) => reader = r,
                    None => return,
                }
                timing = PlaybackTiming::new(playback.state().speed);
                pos = 0.0;
                target = Some(t);
                continue;
            }
        };
        // Fast-forward: everything up to the target, without delays.
        if let Some(t) = target {
            if event.time <= t {
                pos = event.time;
                if !out.event(&mut sanitizer, event, true) {
                    return;
                }
                continue;
            }
            target = None;
            if !out.flush() {
                return;
            }
            // The clock resumes at the target.
            let _ = timing.delay_until(t);
            pos = t;
        }
        let delay = timing.delay_until(event.time);
        match playback.wait(delay, &mut timing) {
            Wake::Closed => return,
            Wake::Seek(t) if t >= pos => {
                // Forwards: this event is the first one of the fast-forward.
                target = Some(t);
                pending = Some(event);
                continue;
            }
            Wake::Seek(t) => {
                match restart(&mut reopen, &mut out, &mut sanitizer) {
                    Some(r) => reader = r,
                    None => return,
                }
                timing = PlaybackTiming::new(playback.state().speed);
                pos = 0.0;
                target = Some(t);
                continue;
            }
            Wake::Go => {}
        }
        pos = event.time;
        if !out.event(&mut sanitizer, event, false) {
            return;
        }
    }
}

/// Backwards seek: a fresh reader, the sanitiser back to its ground state,
/// and a `Reset` for the view. `None` when the view is gone (or the file
/// cannot be opened again: the error is sent).
fn restart<R, F, S>(
    reopen: &mut F,
    out: &mut Sender<'_, S>,
    sanitizer: &mut PlaybackSanitizer,
) -> Option<CastReader<R>>
where
    R: BufRead,
    F: FnMut() -> Result<CastReader<R>, ErrorSummary>,
    S: FnMut(PlaybackFrame) -> bool,
{
    sanitizer.finish();
    out.batch.clear();
    match reopen() {
        Ok(reader) => out
            .send(PlaybackFrame::Reset { time: 0.0 })
            .then_some(reader),
        Err(error) => {
            let _ = out.send(PlaybackFrame::Error { error });
            None
        }
    }
}

// ── Registry of playbacks ───────────────────────────────────────────────

/// Open playbacks, by id: the webview can only reach the ones it opened.
///
/// `epoch` counts [`Playbacks::close_all`] calls (page reloads): a playback
/// whose `play_recording` started before one is refused at registration,
/// so a page that went away cannot leave one holding a slot and a thread.
#[derive(Debug, Default)]
pub(crate) struct Playbacks {
    map: Mutex<HashMap<u64, Arc<Playback>>>,
    next: AtomicU64,
    epoch: AtomicU64,
}

impl Playbacks {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, Arc<Playback>>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(&self, id: u64) -> Result<Arc<Playback>, ErrorSummary> {
        self.lock().get(&id).cloned().ok_or_else(|| ErrorSummary {
            code: "playback_closed".into(),
            message: "this playback is closed".into(),
            hint: None,
        })
    }

    /// The current epoch, read by `play_recording` before it awaits.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Registers a playback opened in `epoch`; refused (and the playback
    /// closed) when [`Playbacks::close_all`] ran since, or beyond
    /// [`MAX_PLAYBACKS`]. The epoch is compared under the map's lock, the
    /// one `close_all` holds while it moves to the next epoch.
    fn register(&self, playback: Arc<Playback>, epoch: u64) -> Result<u64, ErrorSummary> {
        let mut map = self.lock();
        if self.epoch.load(Ordering::Acquire) != epoch {
            drop(map);
            playback.close();
            return Err(ErrorSummary {
                code: "playback_closed".into(),
                message: "the window reloaded while this recording was opening".into(),
                hint: Some("Open the recording again.".into()),
            });
        }
        if map.len() >= MAX_PLAYBACKS {
            return Err(ErrorSummary {
                code: "playback_limit".into(),
                message: format!("{MAX_PLAYBACKS} recordings are already playing"),
                hint: Some("Close a player first.".into()),
            });
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        map.insert(id, playback);
        Ok(id)
    }

    fn remove(&self, id: u64) {
        if let Some(p) = self.lock().remove(&id) {
            p.close();
        }
    }

    /// Closes every playback (page reload, exit) and starts a new epoch:
    /// a playback still opening is refused when it registers.
    pub(crate) fn close_all(&self) {
        let drained: Vec<Arc<Playback>> = {
            let mut map = self.lock();
            self.epoch.fetch_add(1, Ordering::AcqRel);
            map.drain().map(|(_, p)| p).collect()
        };
        for p in drained {
            p.close();
        }
    }
}

// ── Commands ────────────────────────────────────────────────────────────

fn terminal_gone() -> ErrorSummary {
    ErrorSummary {
        code: "terminal_closed".into(),
        message: "this terminal is closed".into(),
        hint: None,
    }
}

/// Starts recording a terminal the view opened. The view shows `REC` from
/// the `Started` event, never from this reply.
#[tauri::command]
pub(crate) async fn recording_start(
    engine: State<'_, Engine>,
    terminals: State<'_, Arc<Terminals>>,
    terminal: u64,
) -> Result<(), ErrorSummary> {
    if terminals.inputs(terminal).is_none() {
        return Err(terminal_gone());
    }
    engine
        .start_recording(TerminalId::from_raw(terminal))
        .await
        .map(|_| ())
        .map_err(|e| safe_summary(&e.summary()))
}

/// Stops the recording of a terminal the view opened (`Stopped` follows).
#[tauri::command]
pub(crate) fn recording_stop(
    engine: State<'_, Engine>,
    terminals: State<'_, Arc<Terminals>>,
    terminal: u64,
) -> Result<(), ErrorSummary> {
    if terminals.inputs(terminal).is_none() {
        return Err(terminal_gone());
    }
    engine
        .stop_recording(TerminalId::from_raw(terminal))
        .map_err(|e| safe_summary(&e.summary()))
}

/// Whether a terminal is being recorded: only to resynchronise after the
/// event stream lagged (`portico://engine-lagged`).
#[tauri::command]
pub(crate) fn recording_status(
    engine: State<'_, Engine>,
    terminals: State<'_, Arc<Terminals>>,
    terminal: u64,
) -> bool {
    terminals.inputs(terminal).is_some() && engine.is_recording(TerminalId::from_raw(terminal))
}

/// The recordings folder for display (`~/…`), shown in the start
/// confirmation: built here, never from a path the view supplies.
#[tauri::command]
pub(crate) fn recordings_folder(engine: State<'_, Engine>) -> String {
    let shown = display_path(&engine.recordings_dir());
    format!("{}/", escape_display(&shown, 512).trim_end_matches('/'))
}

#[tauri::command]
pub(crate) async fn list_recordings(
    engine: State<'_, Engine>,
) -> Result<Vec<RecordingDto>, ErrorSummary> {
    let entries = engine
        .list_recordings()
        .await
        .map_err(|e| safe_summary(&e.summary()))?;
    Ok(entries
        .iter()
        .filter_map(RecordingDto::from_entry)
        .collect())
}

/// Deletes a recording by its listed name (refused while it is written).
#[tauri::command]
pub(crate) async fn delete_recording(
    engine: State<'_, Engine>,
    name: String,
) -> Result<(), ErrorSummary> {
    let name = checked_name(&name)?;
    engine
        .delete_recording(name)
        .await
        .map_err(|e| safe_summary(&e.summary()))
}

/// Opens a recording (by its listed name) and starts streaming it on
/// `frames`. The header comes back at once; errors in the file arrive as
/// an `Error` frame after what could be played.
#[tauri::command]
pub(crate) async fn play_recording(
    engine: State<'_, Engine>,
    playbacks: State<'_, Arc<Playbacks>>,
    name: String,
    speed: f64,
    frames: Channel<PlaybackFrame>,
) -> Result<PlaybackDto, ErrorSummary> {
    let name = checked_name(&name)?.to_owned();
    // Before the await: a reload during it makes `register` refuse.
    let epoch = playbacks.epoch();
    let reader = engine
        .open_recording(&name)
        .await
        .map_err(|e| safe_summary(&e.summary()))?;
    let header = reader.header().clone();
    let playback = Arc::new(Playback::new(speed));
    let id = playbacks.register(Arc::clone(&playback), epoch)?;
    let state = playback.state();
    let engine = engine.inner().clone();
    let registry = Arc::clone(playbacks.inner());
    let file = name.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let reopen = || {
            tauri::async_runtime::block_on(engine.open_recording(&file))
                .map_err(|e| safe_summary(&e.summary()))
        };
        run_playback(reader, reopen, &playback, |frame| {
            frames.send(frame).is_ok()
        });
        registry.remove(id);
    });
    Ok(PlaybackDto {
        id,
        name,
        title: header.title.map(|t| escape_display(&t, DISPLAY_ALIAS)),
        cols: header.width,
        rows: header.height,
        paused: state.paused,
        speed: state.speed,
    })
}

#[tauri::command]
pub(crate) fn playback_control(
    playbacks: State<'_, Arc<Playbacks>>,
    id: u64,
    control: PlaybackControlDto,
) -> Result<PlaybackStateDto, ErrorSummary> {
    playbacks.get(id)?.control(control)
}

/// The view finished parsing `total` bytes of playback output.
#[tauri::command]
pub(crate) fn playback_ack(playbacks: State<'_, Arc<Playbacks>>, id: u64, total: u64) {
    if let Ok(p) = playbacks.get(id) {
        p.ack(total);
    }
}

/// The player closed: the task ends (idempotent).
#[tauri::command]
pub(crate) fn playback_close(playbacks: State<'_, Arc<Playbacks>>, id: u64) {
    playbacks.remove(id);
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    type Reader = CastReader<Cursor<Vec<u8>>>;

    const HEADER: &str = r#"{"version":2,"width":80,"height":24,"timestamp":1761132605,"title":"web-01","env":{"TERM":"xterm-256color"}}"#;

    fn cast(events: &[&str]) -> Vec<u8> {
        let mut text = format!("{HEADER}\n");
        for e in events {
            text.push_str(e);
            text.push('\n');
        }
        text.into_bytes()
    }

    fn reader(bytes: &[u8]) -> Reader {
        CastReader::new(Cursor::new(bytes.to_vec()), "web-01.cast").unwrap()
    }

    fn output(frames: &[PlaybackFrame]) -> String {
        frames
            .iter()
            .filter_map(|f| match f {
                PlaybackFrame::Output { data, .. } => Some(data.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Plays to the end (fast) and returns the frames up to `End`.
    fn play_all(bytes: &[u8]) -> Vec<PlaybackFrame> {
        let playback = Playback::new(MAX_PLAYBACK_SPEED);
        let mut frames = Vec::new();
        let source = bytes.to_vec();
        run_playback(
            reader(bytes),
            || Ok(reader(&source)),
            &playback,
            |f| {
                let end = matches!(f, PlaybackFrame::End { .. } | PlaybackFrame::Error { .. });
                frames.push(f);
                if end {
                    playback.close();
                }
                true
            },
        );
        frames
    }

    #[test]
    fn names_are_validated_before_the_engine() {
        for good in ["web-01-20261022T113005Z.cast", "a.cast", "db_prod.2.cast"] {
            assert_eq!(checked_name(good), Ok(good));
        }
        for bad in [
            "",
            ".cast",
            ".hidden.cast",
            "../web.cast",
            "a/../b.cast",
            "/etc/passwd",
            "dir/web.cast",
            "web.cast.txt",
            "web.txt",
            "we b.cast",
            "web\u{202e}.cast",
            "..cast",
        ] {
            let err = checked_name(bad).unwrap_err();
            assert_eq!(err.code, "recording_invalid_name", "{bad:?}");
            assert!(err.hint.is_some());
            // Escaped for display, like every summary.
            assert!(!err.message.contains('\u{202e}'));
        }
        assert!(checked_name(&format!("{}.cast", "a".repeat(300))).is_err());
    }

    #[test]
    fn dto_is_camel_case_and_escapes_the_title() {
        let entry = RecordingEntry {
            name: "web-01-20261022T113005Z.cast".into(),
            alias: "evil\u{1b}]52;c;AAAA\u{7}\u{202e}".into(),
            started: Some(time::OffsetDateTime::from_unix_timestamp(1_761_132_605).unwrap()),
            size: 2048,
            duration: Some(12.5),
            width: Some(80),
            height: Some(24),
            valid: true,
        };
        let dto = RecordingDto::from_entry(&entry).unwrap();
        assert!(!dto.alias.contains('\u{1b}') && !dto.alias.contains('\u{202e}'));
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["name"], "web-01-20261022T113005Z.cast");
        assert_eq!(json["started"], 1_761_132_605_000_i64);
        assert_eq!(json["duration"], 12.5);
        assert_eq!(json["valid"], true);
        // A name the core would refuse is never listed.
        let bad = RecordingEntry {
            name: "../x.cast".into(),
            ..entry
        };
        assert!(RecordingDto::from_entry(&bad).is_none());
    }

    #[test]
    fn frames_serialize_as_tagged_objects_and_debug_is_redacted() {
        let frame = PlaybackFrame::Output {
            data: "secret-token".into(),
            size: 12,
            time: 1.5,
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["kind"], "output");
        assert_eq!(json["size"], 12);
        assert!(!format!("{frame:?}").contains("secret"));
        let control: PlaybackControlDto =
            serde_json::from_str(r#"{"kind":"speed","value":4}"#).unwrap();
        assert!(
            matches!(control, PlaybackControlDto::Speed { value } if (value - 4.0).abs() < 1e-9)
        );
        let seek: PlaybackControlDto =
            serde_json::from_str(r#"{"kind":"seek","time":2.5}"#).unwrap();
        assert!(matches!(seek, PlaybackControlDto::Seek { .. }));
    }

    #[test]
    fn controls_clamp_speed_and_refuse_nan() {
        let p = Playback::new(100.0);
        assert!((p.state().speed - MAX_PLAYBACK_SPEED).abs() < 1e-9);
        let s = p
            .control(PlaybackControlDto::Speed { value: 0.01 })
            .unwrap();
        assert!((s.speed - MIN_PLAYBACK_SPEED).abs() < 1e-9);
        let err = p
            .control(PlaybackControlDto::Speed { value: f64::NAN })
            .unwrap_err();
        assert_eq!(err.code, "playback_invalid_speed");
        assert!(p.control(PlaybackControlDto::Pause).unwrap().paused);
        assert!(!p.control(PlaybackControlDto::Resume).unwrap().paused);
        // A bogus seek is clamped, not refused.
        p.control(PlaybackControlDto::Seek { time: -3.0 }).unwrap();
        assert_eq!(p.lock().seek, Some(0.0));
    }

    #[test]
    fn output_is_sanitised_with_one_sanitizer_across_events() {
        // OSC 52 (clipboard) split across two events, a title, a colour
        // setting that stays, DSR and DA queries that stay (the player has
        // no reply path), then a resize.
        let frames = play_all(&cast(&[
            r#"[0.1,"o","before\u001b]52;c;aGVsbG8"]"#,
            r#"[0.2,"o","=\u0007after"]"#,
            r#"[0.3,"o","\u001b]0;pwned\u0007\u001b]11;#000000\u0007"]"#,
            r#"[0.4,"o","\u001b[6n\u001b[c"]"#,
            r#"[0.5,"r","100x30"]"#,
        ]));
        let text = output(&frames);
        assert_eq!(
            text, "beforeafter\u{1b}]11;#000000\u{7}\u{1b}[6n\u{1b}[c",
            "{frames:?}"
        );
        assert!(frames.contains(&PlaybackFrame::Resize {
            cols: 100,
            rows: 30,
            time: 0.5
        }));
        assert!(matches!(frames.last(), Some(PlaybackFrame::End { .. })));
    }

    #[test]
    fn a_sequence_cut_by_the_end_of_the_file_is_dropped() {
        let frames = play_all(&cast(&[r#"[0.1,"o","ok\u001b]52;c;dGVzdA"]"#]));
        assert_eq!(output(&frames), "ok");
    }

    #[test]
    fn a_damaged_line_ends_with_an_error_frame() {
        let frames = play_all(&cast(&[r#"[0.1,"o","fine"]"#, "not json"]));
        assert_eq!(output(&frames), "fine");
        let Some(PlaybackFrame::Error { error }) = frames.last() else {
            panic!("expected an error frame: {frames:?}");
        };
        assert_eq!(error.code, "recording_malformed");
    }

    #[test]
    fn flow_control_holds_output_until_acknowledged_and_close_cancels() {
        // 10 events of 100 KiB, no delay between them.
        let chunk = "x".repeat(100 * 1024);
        let lines: Vec<String> = (0..10)
            .map(|i| format!(r#"[0.{i},"o","{chunk}"]"#))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let bytes = cast(&refs);
        let playback = Arc::new(Playback::new(MAX_PLAYBACK_SPEED));
        let (tx, rx) = mpsc::channel();
        let worker = {
            let playback = Arc::clone(&playback);
            thread::spawn(move || {
                let source = bytes.clone();
                run_playback(
                    reader(&bytes),
                    || Ok(reader(&source)),
                    &playback,
                    |f| tx.send(f).is_ok(),
                );
            })
        };
        // Without acknowledgements, at most HIGH_WATER + one frame is sent.
        thread::sleep(Duration::from_millis(300));
        let first: Vec<PlaybackFrame> = rx.try_iter().collect();
        let sent: u64 = first
            .iter()
            .map(|f| match f {
                PlaybackFrame::Output { size, .. } => *size,
                _ => 0,
            })
            .sum();
        assert!(
            sent > 0 && sent <= PLAYBACK_HIGH_WATER + 100 * 1024,
            "{sent}"
        );
        // Acknowledging lets it go on.
        playback.ack(sent);
        thread::sleep(Duration::from_millis(300));
        assert!(rx.try_iter().count() > 0);
        // A bogus ack can never exceed what was sent.
        playback.ack(u64::MAX);
        {
            let c = playback.lock();
            assert!(c.acked <= c.sent);
        }
        // Closing ends the task even while it waits for room or a seek.
        playback.close();
        worker.join().unwrap();
    }

    #[test]
    fn pause_holds_and_resume_and_skip_go_on() {
        let bytes = cast(&[r#"[0.0,"o","a"]"#, r#"[30.0,"o","b"]"#]);
        let playback = Arc::new(Playback::new(1.0));
        playback.control(PlaybackControlDto::Pause).unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = {
            let playback = Arc::clone(&playback);
            thread::spawn(move || {
                let source = bytes.clone();
                run_playback(
                    reader(&bytes),
                    || Ok(reader(&source)),
                    &playback,
                    |f| tx.send(f).is_ok(),
                );
            })
        };
        thread::sleep(Duration::from_millis(150));
        assert_eq!(rx.try_iter().count(), 0, "paused: nothing is sent");
        playback.control(PlaybackControlDto::Resume).unwrap();
        let first = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(first, PlaybackFrame::Output { ref data, .. } if data == "a"));
        // The 30 s silence is capped at 2 s; skip ends it at once.
        playback.control(PlaybackControlDto::Skip).unwrap();
        let second = rx.recv_timeout(Duration::from_millis(500)).unwrap();
        assert!(matches!(second, PlaybackFrame::Output { ref data, .. } if data == "b"));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            PlaybackFrame::End { .. }
        ));
        playback.close();
        worker.join().unwrap();
    }

    #[test]
    fn seeking_back_resets_and_fast_forwards() {
        let bytes = cast(&[
            r#"[0.0,"o","one "]"#,
            r#"[0.1,"o","two "]"#,
            r#"[4.0,"o","three "]"#,
            r#"[8.0,"o","four"]"#,
        ]);
        let playback = Arc::new(Playback::new(1.0));
        let (tx, rx) = mpsc::channel();
        let worker = {
            let playback = Arc::clone(&playback);
            thread::spawn(move || {
                let source = bytes.clone();
                run_playback(
                    reader(&bytes),
                    || Ok(reader(&source)),
                    &playback,
                    |f| tx.send(f).is_ok(),
                );
            })
        };
        let text = |f: PlaybackFrame| match f {
            PlaybackFrame::Output { data, .. } => data,
            other => panic!("expected output, got {other:?}"),
        };
        let next = || rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // "one ", "two " 0.1 s later; then a 2 s (capped) silence.
        assert_eq!(text(next()), "one ");
        assert_eq!(text(next()), "two ");
        playback
            .control(PlaybackControlDto::Seek { time: 0.05 })
            .unwrap();
        // Back to the start: Reset, everything up to 0.05 s at once, then
        // playing again from there.
        assert_eq!(next(), PlaybackFrame::Reset { time: 0.0 });
        assert_eq!(text(next()), "one ");
        assert_eq!(text(next()), "two ");
        // Forwards into the next silence: "three " at once.
        playback
            .control(PlaybackControlDto::Seek { time: 5.0 })
            .unwrap();
        assert_eq!(text(next()), "three ");
        playback.control(PlaybackControlDto::Skip).unwrap();
        assert_eq!(text(next()), "four");
        assert!(matches!(next(), PlaybackFrame::End { time } if (time - 8.0).abs() < 1e-9));
        // After the end, a seek replays from the start.
        playback
            .control(PlaybackControlDto::Seek { time: 0.0 })
            .unwrap();
        assert_eq!(next(), PlaybackFrame::Reset { time: 0.0 });
        assert_eq!(text(next()), "one ");
        playback.close();
        worker.join().unwrap();
    }

    #[test]
    fn registry_limits_and_closes() {
        let registry = Playbacks::default();
        let epoch = registry.epoch();
        let ids: Vec<u64> = (0..MAX_PLAYBACKS)
            .map(|_| {
                registry
                    .register(Arc::new(Playback::new(1.0)), epoch)
                    .unwrap()
            })
            .collect();
        let err = registry
            .register(Arc::new(Playback::new(1.0)), epoch)
            .unwrap_err();
        assert_eq!(err.code, "playback_limit");
        let first = registry.get(ids[0]).unwrap();
        registry.remove(ids[0]);
        assert!(first.is_closed());
        assert_eq!(registry.get(ids[0]).unwrap_err().code, "playback_closed");
        let second = registry.get(ids[1]).unwrap();
        registry.close_all();
        assert!(second.is_closed());
    }

    #[test]
    fn registry_refuses_a_playback_opened_before_close_all() {
        let registry = Playbacks::default();
        // `play_recording` reads the epoch, then awaits the file…
        let epoch = registry.epoch();
        let kept = Arc::new(Playback::new(1.0));
        let kept_id = registry.register(Arc::clone(&kept), epoch).unwrap();
        // …while the page reloads.
        registry.close_all();
        assert!(kept.is_closed());
        let late = Arc::new(Playback::new(1.0));
        let err = registry.register(Arc::clone(&late), epoch).unwrap_err();
        assert_eq!(err.code, "playback_closed");
        // Refused and closed: its task stops at once, no slot is held.
        assert!(late.is_closed());
        assert_eq!(registry.get(kept_id).unwrap_err().code, "playback_closed");
        // The new page opens all its slots.
        let now = registry.epoch();
        assert_ne!(now, epoch);
        for _ in 0..MAX_PLAYBACKS {
            registry
                .register(Arc::new(Playback::new(1.0)), now)
                .unwrap();
        }
    }
}
