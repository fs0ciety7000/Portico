//! Everything that can change [`crate::app::App`] state.

use std::time::Duration;

use crossterm::event::KeyEvent;
use portico_core::engine::{HostInventory, TerminalHandle};
use portico_core::forward::ForwardSpec;
use portico_core::ids::{SessionId, TerminalId, TunnelId};
use portico_core::keys::LocalKey;
use portico_core::snippets::Snippet;
use portico_core::vault::VaultStatus;
use portico_core::{AgentKey, CopyIdOutcome, EngineEvent, ErrorSummary};

use crate::files::FilesEvent;

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
        result: Result<Box<TerminalHandle>, ErrorSummary>,
    },
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
}
