//! Everything that can change [`crate::app::App`] state.

use std::time::Duration;

use crossterm::event::KeyEvent;
use portico_core::effective::EffectiveConfig;
use portico_core::engine::{HostInventory, TerminalHandle};
use portico_core::forward::ForwardSpec;
use portico_core::hosts::HostMeta;
use portico_core::ids::{SessionId, TerminalId, TunnelId};
use portico_core::keys::LocalKey;
use portico_core::managed_hosts::{HostSummary, ManagedHostDetails};
use portico_core::recent::RecentHost;
use portico_core::recording::RecordingEntry;
use portico_core::settings::Settings;
use portico_core::snippets::Snippet;
use portico_core::vault::VaultStatus;
use portico_core::{AgentKey, CopyIdOutcome, EngineEvent, ErrorSummary};

use crate::files::FilesEvent;
use crate::panes::{Dir, PaneId, Split};
use crate::player::{PlaybackStart, ReaderMsg};

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
    /// `Ctrl-Space /`: opens the focused pane's scrollback search.
    ScrollbackSearch,
    /// A key for the focused pane's open search bar (never the remote).
    ScrollbackKey(KeyEvent),
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
    /// A timed state ran out (pane repeat window, status-bar note).
    Expire,
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
    /// The engine bus dropped events for us: state is resynchronised
    /// through requests.
    EngineLagged,
    // Session recording (phase 6)
    /// "Record session" / "Stop recording" on the focused pane.
    ToggleRecording,
    /// `start_recording` answered. Never turns the REC badge on: only
    /// `RecordingState::Started` does.
    RecordingStart(Result<(), ErrorSummary>),
    OpenRecordings,
    RecordingsKey(KeyEvent),
    RecordingsLoaded(Result<Vec<RecordingEntry>, ErrorSummary>),
    RecordingDeleted {
        name: String,
        result: Result<(), ErrorSummary>,
    },
    /// A recording opened for playback (its reader thread is running).
    PlaybackOpened {
        name: String,
        total: Option<f64>,
        result: Result<PlaybackStart, ErrorSummary>,
    },
    PlayerKey(KeyEvent),
    /// The recording reopened for a seek (`←`, `Home`, replay): the
    /// player starts over and fast-replays to the target. `generation`
    /// tells a stale reopening from the last one asked.
    PlaybackSeek {
        generation: u64,
        result: Result<PlaybackStart, ErrorSummary>,
    },
    /// Next message of the playback reader (`None`: the reader is gone).
    PlayerFeed(Option<ReaderMsg>),
    // Hosts created in Portico (phase 6b)
    /// "Add host…".
    AddHost,
    /// "Duplicate as Portico host…" on the selected host.
    DuplicateHost,
    /// "Delete host" on the selected host (asks first).
    DeleteHost,
    HostFormKey(KeyEvent),
    /// Local keys for the form's key picker.
    HostFormKeys(Result<Vec<LocalKey>, ErrorSummary>),
    /// "Edit host…": the values `hosts.conf` holds for `alias`
    /// (`Engine::managed_host_details`; `None`: no longer in that file).
    HostFormManaged {
        alias: String,
        result: Result<Option<ManagedHostDetails>, ErrorSummary>,
    },
    /// A create, edit or duplicate came back.
    HostSaved(Result<HostSummary, ErrorSummary>),
    HostDeleted {
        alias: String,
        result: Result<(), ErrorSummary>,
    },
    /// Whether `~/.ssh/config` already includes `hosts.conf`.
    IncludeChecked(Result<bool, ErrorSummary>),
    /// Copies the proposed `Include` line (OSC 52).
    CopyInclude,
    /// Hides the `Include` banner until Portico restarts.
    HideInclude,
    // Opt-in update check (0.3.0)
    /// The status at launch (`start_update_checks`) or after a lagged bus.
    UpdateLoaded(Result<portico_core::update::UpdateStatus, ErrorSummary>),
    /// How to update this install, for `version`.
    UpdateInstruction {
        version: String,
        instruction: portico_core::update::UpdateInstruction,
    },
    /// "Update": the update view (the release, or the setting).
    ShowUpdate,
    /// "Toggle update check": the daily check on / off.
    ToggleUpdateCheck,
    UpdateCheckSaved(Result<portico_core::update::UpdateStatus, ErrorSummary>),
}
