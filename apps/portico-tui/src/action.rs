//! Everything that can change [`crate::app::App`] state.

use portico_core::engine::HostInventory;

#[derive(Debug)]
pub(crate) enum Action {
    Quit,
    Tick,
    // Navigation
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
    ConnectFailed(String),
}
