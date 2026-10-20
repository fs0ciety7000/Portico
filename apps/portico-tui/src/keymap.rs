//! Key → [`Action`] mapping. The single source of truth for shortcuts; keep it
//! in sync with `docs/KEYBINDINGS.md` and the help overlay ([`HELP`]).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::action::Action;
use crate::app::Focus;

/// (keys, description) rows shown in the `?` overlay.
pub(crate) const HELP: &[(&str, &str)] = &[
    ("j / k  ↓ / ↑", "Move selection"),
    ("g / G", "First / last host"),
    ("/  or  Ctrl-K", "Search hosts (fuzzy, #tag, * favorites)"),
    ("Enter", "Connect"),
    ("d", "Disconnect the selected host"),
    ("Esc", "Leave search / close overlay"),
    ("Ctrl-U", "Clear search"),
    ("r", "Reload ~/.ssh/config"),
    ("t", "Cycle theme"),
    ("?", "Toggle this help"),
    ("q  /  Ctrl-C", "Quit"),
];

pub(crate) fn map_key(key: KeyEvent, focus: Focus, overlay_open: bool) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
        Focus::Hosts => match key.code {
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
            KeyCode::Enter => Some(Action::Connect),
            KeyCode::Esc => Some(Action::SearchClear),
            _ => None,
        },
    }
}
