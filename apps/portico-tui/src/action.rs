//! Everything that can change [`crate::app::App`] state.

use crossterm::event::KeyEvent;
use portico_core::EngineEvent;
use portico_core::engine::{HostInventory, TerminalHandle};
use portico_core::ids::TerminalId;

#[derive(Debug)]
pub(crate) enum Action {
    Quit,
    Tick,
    // Host list navigation
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    // Search
    FocusSearch,
    SearchInput(char),
    SearchBackspace,
    SearchClear,
    BlurSearch,
    // Overlays & preferences
    ToggleHelp,
    CloseOverlay,
    CycleTheme,
    // Data
    Reload,
    HostsLoaded(Result<HostInventory, String>),
    Connect,
    Disconnect,
    Engine(EngineEvent),
    // Dialogs
    DialogKey(KeyEvent),
    // Tabs (Ctrl-Space prefix)
    Prefix,
    CancelPrefix,
    GoHome,
    GoTab(usize),
    NextTab,
    PrevTab,
    NewTab,
    CloseTab,
    // Terminal
    TerminalKey(KeyEvent),
    Paste(String),
    ScrollUp,
    ScrollDown,
    TerminalDirty(TerminalId),
    TerminalOpened {
        alias: String,
        result: Result<Box<TerminalHandle>, portico_core::ErrorSummary>,
    },
}
