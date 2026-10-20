//! Embedded terminal model: one [`TerminalView`] per tab.
//!
//! The `alacritty_terminal::Term` (grid + scrollback + VT state) is shared
//! with a reader task that parses PTY output off the UI loop. The UI is woken
//! through a coalesced `dirty` flag: a flood of output produces one redraw
//! per frame, never an unbounded queue of actions. Because the reader only
//! pulls from the engine's bounded channel as fast as it parses, SSH
//! backpressure is preserved end to end.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::engine::{PtySize, TerminalHandle, TerminalInput};
use portico_core::ids::{SessionId, TerminalId};
use tokio::sync::mpsc;

use crate::action::Action;

/// Lines of scrollback kept per terminal.
const SCROLLBACK: usize = 10_000;

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
    input: mpsc::Sender<TerminalInput>,
    title: Arc<Mutex<Option<String>>>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        match event {
            // Device status / attribute reports must reach the remote program.
            TermEvent::PtyWrite(text) => {
                let _ = self.input.try_send(TerminalInput::Data(Bytes::from(text)));
            }
            TermEvent::Title(title) => {
                *self.title.lock().unwrap_or_else(PoisonError::into_inner) = Some(title);
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// The shell exited with this status.
    Exited(u32),
    /// Killed by a signal, or the channel / connection closed.
    Closed,
}

/// A terminal tab.
pub(crate) struct TerminalView {
    pub(crate) id: TerminalId,
    pub(crate) session: SessionId,
    pub(crate) alias: String,
    pub(crate) term: SharedTerm,
    title: Arc<Mutex<Option<String>>>,
    input: mpsc::Sender<TerminalInput>,
    pub(crate) size: GridSize,
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
            dirty,
            ended: None,
            closed: false,
        }
    }

    /// Tab label: OSC title if the remote set one, else the host alias.
    pub(crate) fn label(&self) -> String {
        self.title
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| self.alias.clone())
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
        let _ = self.input.try_send(TerminalInput::Data(Bytes::from(bytes)));
    }

    pub(crate) fn scroll(&self, scroll: Scroll) {
        self.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scroll_display(scroll);
    }

    pub(crate) fn resize(&mut self, size: GridSize) {
        if size == self.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size = size;
        self.term
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .resize(size);
        let _ = self.input.try_send(TerminalInput::Resize(size.pty()));
    }

    pub(crate) fn paste(&self, text: &str) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        let mut out = Vec::with_capacity(text.len() + 12);
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
            // A pasted end marker would let the clipboard escape the bracket.
            out.extend_from_slice(text.replace("\x1b[201~", "").as_bytes());
            out.extend_from_slice(b"\x1b[201~");
        } else {
            out.extend_from_slice(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        }
        self.send(out);
    }

    pub(crate) fn close(&mut self) {
        let _ = self.input.try_send(TerminalInput::Close);
        self.closed = true;
    }
}

/// Encodes a key press the way xterm does. `None` for keys with no byte form.
pub(crate) fn encode_key(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
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
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
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
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter
        )
    {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> Option<Vec<u8>> {
        encode_key(KeyEvent::new(code, m), false)
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
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
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
