//! Key → [`Action`] mapping. The single source of truth for shortcuts; keep it
//! in sync with `docs/KEYBINDINGS.md` and the help overlay ([`HELP`]).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::action::Action;
use crate::app::{Focus, Overlay};

/// (keys, description) rows shown in the `?` overlay, by section.
pub(crate) const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Hosts",
        &[
            ("j k  ↓ ↑", "Move"),
            ("g · G", "First · last"),
            ("/ · Ctrl-K", "Search (#tag, *fav)"),
            ("Enter", "Connect"),
            ("d", "Disconnect"),
            ("f · p", "Files · tunnels"),
            ("1-9  ] [", "Tab · next · prev"),
            ("r · t · q", "Reload, theme, quit"),
        ],
    ),
    ("Terminal", &[("Shift-PgUp/Dn", "Scroll back")]),
    (
        "Ctrl-Space, then…",
        &[
            ("h · 0", "Back to hosts"),
            ("1-9  ] [", "Tab · next · prev"),
            ("n · w", "New tab · close tab"),
            ("d", "Disconnect host"),
            ("f · p", "Files · tunnels"),
            ("Ctrl-Space", "Send it to remote"),
        ],
    ),
    (
        "Files",
        &[
            ("Tab", "Switch pane"),
            ("↵ · Bksp", "Open · parent"),
            ("c  F5", "Copy across"),
            ("n F7 · r F2", "New folder · rename"),
            ("d  F8", "Delete (asks)"),
            ("x · .", "Cancel · hidden"),
            ("Ctrl-R · Esc", "Refresh · back"),
        ],
    ),
    (
        "Tunnels",
        &[("a", "Add -L / -R"), ("↵ · x", "Start · stop")],
    ),
];

pub(crate) fn map_key(
    key: KeyEvent,
    focus: Focus,
    overlay: Option<Overlay>,
    dialog_open: bool,
    prefix: bool,
) -> Option<Action> {
    let overlay_open = overlay.is_some();
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Dialogs grab every key (Ctrl-C cancels instead of quitting).
    if dialog_open {
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::DialogKey(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )));
        }
        return Some(Action::DialogKey(key));
    }
    if key.kind != KeyEventKind::Press && focus != Focus::Terminal {
        return None;
    }
    let is_prefix_key = ctrl && matches!(key.code, KeyCode::Char(' ' | '@'));
    // The tunnels panel takes every key (Ctrl-C closes it).
    if overlay == Some(Overlay::Tunnels) {
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Action::CloseOverlay);
        }
        return Some(Action::TunnelsKey(key));
    }
    // The prefix works from every view, so tab commands are the same everywhere.
    if prefix {
        return Some(prefix_command(key, is_prefix_key, focus));
    }
    if focus == Focus::Files && !overlay_open {
        return Some(match key.code {
            _ if is_prefix_key => Action::Prefix,
            // Ctrl-C leaves the explorer rather than quitting Portico.
            KeyCode::Char('c') if ctrl => {
                Action::FilesKey(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            }
            KeyCode::Char('?') => Action::ToggleHelp,
            _ => Action::FilesKey(key),
        });
    }
    if focus == Focus::Terminal {
        if overlay_open {
            return Some(Action::CloseOverlay);
        }
        return Some(match key.code {
            _ if is_prefix_key => Action::Prefix,
            KeyCode::PageUp if shift => Action::ScrollUp,
            KeyCode::PageDown if shift => Action::ScrollDown,
            _ => Action::TerminalKey(key),
        });
    }
    if ctrl && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    if overlay_open {
        return match key.code {
            KeyCode::Esc | KeyCode::Char('?' | 'q') => Some(Action::CloseOverlay),
            _ => None,
        };
    }
    match focus {
        Focus::Search => match key.code {
            KeyCode::Esc => Some(Action::BlurSearch),
            KeyCode::Enter => Some(Action::Connect),
            KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Up => Some(Action::SelectPrev),
            KeyCode::Backspace => Some(Action::SearchBackspace),
            KeyCode::Char('u') if ctrl => Some(Action::SearchClear),
            KeyCode::Char('n') if ctrl => Some(Action::SelectNext),
            KeyCode::Char('p') if ctrl => Some(Action::SelectPrev),
            KeyCode::Char(c) if !ctrl => Some(Action::SearchInput(c)),
            _ => None,
        },
        Focus::Hosts | Focus::Terminal | Focus::Files => match key.code {
            KeyCode::Char('k') if ctrl => Some(Action::FocusSearch),
            KeyCode::Char('u') if ctrl => Some(Action::SearchClear),
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrev),
            KeyCode::Char('g') | KeyCode::Home => Some(Action::SelectFirst),
            KeyCode::Char('G') | KeyCode::End => Some(Action::SelectLast),
            KeyCode::Char('/') => Some(Action::FocusSearch),
            KeyCode::Char('?') => Some(Action::ToggleHelp),
            KeyCode::Char('t') => Some(Action::CycleTheme),
            KeyCode::Char('r') => Some(Action::Reload),
            KeyCode::Char('d') => Some(Action::Disconnect),
            KeyCode::Char('f') => Some(Action::OpenFiles),
            KeyCode::Char('p') => Some(Action::OpenTunnels),
            KeyCode::Char(']') | KeyCode::Tab => Some(Action::NextTab),
            KeyCode::Char('[') | KeyCode::BackTab => Some(Action::PrevTab),
            KeyCode::Char(c @ '1'..='9') => Some(Action::GoTab(usize::from(c as u8 - b'1'))),
            KeyCode::Enter => Some(Action::Connect),
            KeyCode::Esc => Some(Action::SearchClear),
            _ if is_prefix_key => Some(Action::Prefix),
            _ => None,
        },
    }
}

fn prefix_command(key: KeyEvent, is_prefix_key: bool, focus: Focus) -> Action {
    if is_prefix_key {
        // Ctrl-Space twice: deliver it to the remote program, if any.
        return if focus == Focus::Terminal {
            Action::TerminalKey(key)
        } else {
            Action::CancelPrefix
        };
    }
    match key.code {
        KeyCode::Char('h' | '0') => Action::GoHome,
        KeyCode::Char(c @ '1'..='9') => Action::GoTab(usize::from(c as u8 - b'1')),
        KeyCode::Char(']') | KeyCode::Tab => Action::NextTab,
        KeyCode::Char('[') | KeyCode::BackTab => Action::PrevTab,
        KeyCode::Char('n') => Action::NewTab,
        KeyCode::Char('w') => Action::CloseTab,
        KeyCode::Char('d') => Action::Disconnect,
        KeyCode::Char('f') => Action::OpenFiles,
        KeyCode::Char('p') => Action::OpenTunnels,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char('/') => Action::FocusSearch,
        _ => Action::CancelPrefix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn terminal_focus_sends_everything_but_the_prefix() {
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(
            map_key(ctrl_c, Focus::Terminal, None, false, false),
            Some(Action::TerminalKey(_))
        ));
        let prefix = key(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(matches!(
            map_key(prefix, Focus::Terminal, None, false, false),
            Some(Action::Prefix)
        ));
        assert!(matches!(
            map_key(
                key(KeyCode::Char('w'), KeyModifiers::NONE),
                Focus::Terminal,
                None,
                false,
                true
            ),
            Some(Action::CloseTab)
        ));
        assert!(matches!(
            map_key(prefix, Focus::Terminal, None, false, true),
            Some(Action::TerminalKey(_))
        ));
    }

    #[test]
    fn prefix_works_from_the_host_list() {
        let prefix = key(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(matches!(
            map_key(prefix, Focus::Hosts, None, false, false),
            Some(Action::Prefix)
        ));
        assert!(matches!(
            map_key(
                key(KeyCode::Char('2'), KeyModifiers::NONE),
                Focus::Hosts,
                None,
                false,
                true
            ),
            Some(Action::GoTab(1))
        ));
        // Twice from the host list: nothing to forward it to.
        assert!(matches!(
            map_key(prefix, Focus::Hosts, None, false, true),
            Some(Action::CancelPrefix)
        ));
    }

    #[test]
    fn files_and_tunnels_capture_their_keys() {
        let d = key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(
            map_key(d, Focus::Files, None, false, false),
            Some(Action::FilesKey(_))
        ));
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let Some(Action::FilesKey(k)) = map_key(ctrl_c, Focus::Files, None, false, false) else {
            panic!("ctrl-c should leave the explorer");
        };
        assert_eq!(k.code, KeyCode::Esc);
        let q = key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(matches!(
            map_key(q, Focus::Hosts, Some(Overlay::Tunnels), false, false),
            Some(Action::TunnelsKey(_))
        ));
        assert!(matches!(
            map_key(
                key(KeyCode::Char('f'), KeyModifiers::NONE),
                Focus::Terminal,
                None,
                false,
                true
            ),
            Some(Action::OpenFiles)
        ));
    }

    #[test]
    fn dialogs_capture_keys_and_ctrl_c_cancels() {
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let Some(Action::DialogKey(k)) = map_key(ctrl_c, Focus::Hosts, None, true, false) else {
            panic!("dialog should capture");
        };
        assert_eq!(k.code, KeyCode::Esc);
    }
}
