//! Everything that can change [`crate::app::App`] state.

use std::time::Duration;

use crossterm::event::KeyEvent;
use portico_core::effective::EffectiveConfig;
use portico_core::engine::{HostInventory, TerminalHandle};
use portico_core::forward::ForwardSpec;
use portico_core::hosts::HostMeta;
use portico_core::ids::{SessionId, TerminalId, TunnelId};
use portico_core::keys::LocalKey;
use portico_core::recent::RecentHost;
use portico_core::settings::Settings;
use portico_core::snippets::Snippet;
use portico_core::vault::VaultStatus;
use portico_core::{AgentKey, CopyIdOutcome, EngineEvent, ErrorSummary};

use crate::files::FilesEvent;
use crate::panes::{Dir, PaneId, Split};

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
    /// Closes the help (only: a panel under it stays).
    CloseHelp,
    /// The other help page (when the help does not fit at once).
    HelpPage,
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
        /// The pane waiting for this shell; `None` opens a new tab.
        pane: Option<PaneId>,
        result: Result<Box<TerminalHandle>, ErrorSummary>,
    },
    // Split panes (Ctrl-Space prefix)
    /// A new pane on the focused pane's host.
    SplitPane(Split),
    /// Pick a host in the list, then split onto it.
    SplitPaneHost,
    /// Leave the host picker, nothing split.
    CancelPanePick,
    FocusPane(Dir),
    NextPane,
    ResizePane(Dir),
    ClosePane,
    ZoomPane,
    // SFTP explorer
    OpenFiles,
    FilesKey(KeyEvent),
    Files(FilesEvent),
    // Tunnels panel
    OpenTunnels,
    TunnelsKey(KeyEvent),
    TunnelStarted {
        session: SessionId,
        alias: String,
        spec: ForwardSpec,
        result: Result<TunnelId, ErrorSummary>,
    },
    // Keys panel
    OpenKeys,
    KeysKey(KeyEvent),
    /// Host name of this machine (for the suggested key comment).
    MachineName(Option<String>),
    LocalKeys(Result<Vec<LocalKey>, ErrorSummary>),
    AgentKeys(Result<Vec<AgentKey>, ErrorSummary>),
    KeyGenerated(Result<LocalKey, ErrorSummary>),
    AgentAdded {
        key: LocalKey,
        lifetime: Option<Duration>,
        result: Result<AgentKey, ErrorSummary>,
    },
    AgentRemoved {
        label: String,
        result: Result<(), ErrorSummary>,
    },
    PublicKeyRead(Result<String, ErrorSummary>),
    CopyInstalled {
        generation: u64,
        result: Result<CopyIdOutcome, ErrorSummary>,
    },
    CopyVerified {
        generation: u64,
        result: Result<(), ErrorSummary>,
    },
    // Vault
    VaultStatus {
        startup: bool,
        result: Result<VaultStatus, ErrorSummary>,
    },
    /// Answer to the secret dialog in front (vault or agent passphrase).
    SecretDone(Result<(), ErrorSummary>),
    VaultLocked(Result<(), ErrorSummary>),
    // Snippets panel
    OpenSnippets,
    SnippetsKey(KeyEvent),
    SnippetsLoaded(Result<Vec<Snippet>, ErrorSummary>),
    SnippetSaved(Result<Snippet, ErrorSummary>),
    SnippetDeleted {
        name: String,
        result: Result<(), ErrorSummary>,
    },
    // Host metadata (phase 4)
    ToggleFavorite,
    EditHost,
    EditKey(KeyEvent),
    /// A favorite toggle came back (the `HostMeta` event updates the list).
    MetaSaved {
        alias: String,
        result: Result<HostMeta, ErrorSummary>,
    },
    /// The edit form was saved (or the field that failed, with why).
    HostEdited {
        alias: String,
        result: Result<HostMeta, (crate::edit_host::EditField, ErrorSummary)>,
    },
    // Recent hosts
    RecentLoaded(Result<Vec<RecentHost>, ErrorSummary>),
    ForgetRecent,
    AskClearRecent,
    RecentEdited(Result<(), ErrorSummary>),
    // Debug panel
    OpenDebug,
    DebugKey(KeyEvent),
    EffectiveLoaded {
        alias: String,
        result: Result<EffectiveConfig, ErrorSummary>,
    },
    // Settings
    SettingsLoaded(Result<Settings, ErrorSummary>),
    ThemeSaved(Result<(), ErrorSummary>),
}
