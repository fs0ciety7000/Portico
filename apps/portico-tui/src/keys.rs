//! Keys panel state: local key pairs (`~/.ssh`), then the identities held by
//! `ssh-agent`, with the vault status in the footer.
//!
//! Everything here is pure state + key handling: a key press returns a
//! [`KeysOutcome`] and the app runs the engine call (in a task) and feeds
//! the result back. Sub-modes replace the list while they are open: the key
//! generation form, the agent lifetime choice, the removal confirmation, the
//! host picker of copy-id and the copy-id progress (whose job lives on the
//! app, so it survives closing the panel).

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::AgentKey;
use portico_core::ErrorSummary;
use portico_core::hosts::HostEntry;
use portico_core::keys::{CertValidity, Curve, KeyAlgorithm, KeyGenRequest, LocalKey};
use portico_core::search::{Hit, HostSearch};
use portico_core::vault::{MIN_PASSPHRASE_CHARS, VaultStatus};
use ratatui::widgets::ListState;
use secrecy::SecretString;
use zeroize::{Zeroize, Zeroizing};

use crate::form::{MAX_LINE, paste_line, push_char, secret_buffer};

/// Something loaded in the background.
#[derive(Debug, Clone)]
pub(crate) enum Load<T> {
    Loading,
    Ready(T),
    Failed(ErrorSummary),
}

impl<T> Load<T> {
    pub(crate) fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Local,
    Agent,
}

/// Agent lifetimes offered by `a` (`ssh-add -t`).
pub(crate) const LIFETIMES: [(&str, Option<Duration>); 3] = [
    ("Until removed", None),
    ("1 hour", Some(Duration::from_secs(3600))),
    ("8 hours", Some(Duration::from_secs(8 * 3600))),
];

/// One algorithm choice of the generation form.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AlgoChoice {
    pub(crate) label: &'static str,
    /// File name prefix: `id_<prefix>_<host>`.
    pub(crate) prefix: &'static str,
    pub(crate) algorithm: KeyAlgorithm,
    pub(crate) note: &'static str,
}

pub(crate) const ALGORITHMS: [AlgoChoice; 6] = [
    AlgoChoice {
        label: "Ed25519",
        prefix: "ed25519",
        algorithm: KeyAlgorithm::Ed25519,
        note: "recommended",
    },
    AlgoChoice {
        label: "RSA 3072",
        prefix: "rsa",
        algorithm: KeyAlgorithm::Rsa { bits: 3072 },
        note: "for older servers",
    },
    AlgoChoice {
        label: "RSA 4096",
        prefix: "rsa",
        algorithm: KeyAlgorithm::Rsa { bits: 4096 },
        note: "slow to generate",
    },
    AlgoChoice {
        label: "ECDSA P-256",
        prefix: "ecdsa",
        algorithm: KeyAlgorithm::Ecdsa { curve: Curve::P256 },
        note: "",
    },
    AlgoChoice {
        label: "ECDSA P-384",
        prefix: "ecdsa",
        algorithm: KeyAlgorithm::Ecdsa { curve: Curve::P384 },
        note: "",
    },
    AlgoChoice {
        label: "ECDSA P-521",
        prefix: "ecdsa",
        algorithm: KeyAlgorithm::Ecdsa { curve: Curve::P521 },
        note: "",
    },
];

/// Longest key file name (the core refuses longer ones).
const MAX_NAME: usize = 64;

/// `web-01.prod` → `web-01.prod`; anything outside `[a-z0-9._-]` becomes
/// `-`; empty → `portico`. Used in the suggested key file name.
pub(crate) fn host_hint(alias: Option<&str>) -> String {
    let cleaned: String = alias
        .unwrap_or_default()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    let cleaned = cleaned.trim_matches(['-', '.']).to_owned();
    if cleaned.is_empty() {
        "portico".to_owned()
    } else {
        cleaned
    }
}

/// `base`, or `base_2`, `base_3`… : the first name not in `taken`.
pub(crate) fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_owned();
    }
    (2..1000)
        .map(|n| format!("{base}_{n}"))
        .find(|candidate| !taken.iter().any(|t| t == candidate))
        .unwrap_or_else(|| base.to_owned())
}

/// File names already used in `~/.ssh` by the known key pairs.
pub(crate) fn taken_names(keys: &[LocalKey]) -> Vec<String> {
    keys.iter()
        .flat_map(|k| [&k.private_path, &k.public_path])
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .map(str::to_owned)
        .collect()
}

/// Why a key file name cannot be used, if it cannot.
pub(crate) fn name_problem(name: &str, taken: &[String]) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return Some("Name the key file.".to_owned());
    }
    if name.chars().count() > MAX_NAME {
        return Some(format!("Use at most {MAX_NAME} characters."));
    }
    if name.starts_with('.')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Some("Use letters, digits, dot, dash or underscore.".to_owned());
    }
    if taken
        .iter()
        .any(|t| t == name || *t == format!("{name}.pub"))
    {
        return Some(format!("~/.ssh/{name} already exists."));
    }
    None
}

/// File name of a path, for display.
pub(crate) fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GenField {
    Name,
    Algorithm,
    Comment,
    Passphrase,
    Confirm,
}

const GEN_FIELDS: [GenField; 5] = [
    GenField::Name,
    GenField::Algorithm,
    GenField::Comment,
    GenField::Passphrase,
    GenField::Confirm,
];

/// The key generation form. `Debug` never shows the passphrases.
pub(crate) struct GenForm {
    pub(crate) name: String,
    /// The user typed in the name: changing the algorithm keeps it.
    name_edited: bool,
    host: String,
    pub(crate) algorithm: usize,
    pub(crate) comment: String,
    pub(crate) passphrase: Zeroizing<String>,
    pub(crate) confirm: Zeroizing<String>,
    pub(crate) focus: GenField,
    taken: Vec<String>,
    /// Error from the engine (refused name, unsafe directory…).
    pub(crate) error: Option<ErrorSummary>,
    /// `Some(label)` while the key is being generated.
    pub(crate) busy: Option<String>,
    /// `↵` was pressed: problems are shown even on untouched fields.
    pub(crate) submitted: bool,
    /// What became of the last passphrase handed to the engine.
    handed: Handed,
}

/// The passphrase of the last request, once handed over (and wiped here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handed {
    /// No request yet, or one without a passphrase.
    Nothing,
    Passphrase,
    /// The engine refused a request that had a passphrase: generating
    /// again needs it typed again, or an explicit "no passphrase"
    /// (`Ctrl-N`), never a silent unencrypted key.
    Refused,
}

impl std::fmt::Debug for GenForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted = |typed: bool| if typed { "[redacted]" } else { "" };
        f.debug_struct("GenForm")
            .field("name", &self.name)
            .field("algorithm", &self.algorithm)
            .field("comment", &self.comment)
            .field("passphrase", &redacted(!self.passphrase.is_empty()))
            .field("confirm", &redacted(!self.confirm.is_empty()))
            .field("focus", &self.focus)
            .field("error", &self.error)
            .field("busy", &self.busy)
            .field("submitted", &self.submitted)
            .field("handed", &self.handed)
            .finish_non_exhaustive()
    }
}

pub(crate) enum GenOutcome {
    Pending,
    Cancel,
    Submit(KeyGenRequest),
}

impl GenForm {
    pub(crate) fn new(host: String, comment: String, taken: Vec<String>) -> Self {
        let mut form = Self {
            name: String::new(),
            name_edited: false,
            host,
            algorithm: 0,
            comment,
            passphrase: secret_buffer(),
            confirm: secret_buffer(),
            focus: GenField::Name,
            taken,
            error: None,
            busy: None,
            submitted: false,
            handed: Handed::Nothing,
        };
        form.suggest_name();
        form
    }

    pub(crate) fn choice(&self) -> AlgoChoice {
        ALGORITHMS
            .get(self.algorithm)
            .copied()
            .unwrap_or(ALGORITHMS[0])
    }

    fn suggest_name(&mut self) {
        if !self.name_edited {
            let base = format!("id_{}_{}", self.choice().prefix, self.host);
            self.name = unique_name(&base, &self.taken);
        }
    }

    pub(crate) fn name_problem(&self) -> Option<String> {
        name_problem(&self.name, &self.taken)
    }

    pub(crate) fn confirm_problem(&self) -> Option<&'static str> {
        let typed = !self.confirm.is_empty() || self.submitted;
        (typed && *self.passphrase != *self.confirm).then_some("The passphrases do not match.")
    }

    pub(crate) fn comment_problem(&self) -> Option<&'static str> {
        (self.comment.chars().count() > MAX_LINE).then_some("Keep the comment short.")
    }

    /// A refused request had a passphrase: it must be typed again (or
    /// "no passphrase" said with `Ctrl-N`).
    pub(crate) fn retype(&self) -> bool {
        self.handed == Handed::Refused
    }

    /// Characters of the passphrase, capped at the minimum (the counter
    /// never tells more than "long enough").
    pub(crate) fn passphrase_progress(&self) -> usize {
        self.passphrase.chars().count().min(MIN_PASSPHRASE_CHARS)
    }

    pub(crate) fn passphrase_problem(&self) -> Option<&'static str> {
        if self.retype() && self.passphrase.is_empty() {
            return Some("Type the passphrase again.");
        }
        let short =
            !self.passphrase.is_empty() && self.passphrase.chars().count() < MIN_PASSPHRASE_CHARS;
        (short && self.submitted).then_some("Use at least 8 characters, or none.")
    }

    /// Ready to generate (the primary action is enabled).
    pub(crate) fn valid(&self) -> bool {
        let short =
            !self.passphrase.is_empty() && self.passphrase.chars().count() < MIN_PASSPHRASE_CHARS;
        self.name_problem().is_none()
            && !short
            && !(self.retype() && self.passphrase.is_empty())
            && *self.passphrase == *self.confirm
            && self.comment_problem().is_none()
    }

    /// The engine refused the request (name taken, unsafe directory…).
    /// The passphrase it carried was wiped: ask for it again.
    pub(crate) fn refused(&mut self, error: ErrorSummary) {
        self.busy = None;
        self.error = Some(error);
        if self.handed == Handed::Passphrase {
            self.handed = Handed::Refused;
            self.focus = GenField::Passphrase;
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let i = GEN_FIELDS
            .iter()
            .position(|f| *f == self.focus)
            .unwrap_or(0);
        let n = GEN_FIELDS.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus = GEN_FIELDS.get(next).copied().unwrap_or(GenField::Name);
    }

    fn cycle_algorithm(&mut self, forward: bool) {
        let n = ALGORITHMS.len();
        self.algorithm = if forward {
            (self.algorithm + 1) % n
        } else {
            (self.algorithm + n - 1) % n
        };
        self.suggest_name();
    }

    fn text_field(&mut self) -> Option<&mut String> {
        match self.focus {
            GenField::Name => Some(&mut self.name),
            GenField::Comment => Some(&mut self.comment),
            GenField::Passphrase => Some(&mut self.passphrase),
            GenField::Confirm => Some(&mut self.confirm),
            GenField::Algorithm => None,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> GenOutcome {
        if self.busy.is_some() {
            return GenOutcome::Pending;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return GenOutcome::Cancel,
            KeyCode::Enter => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(false),
            KeyCode::Left if self.focus == GenField::Algorithm => self.cycle_algorithm(false),
            KeyCode::Right | KeyCode::Char(' ') if self.focus == GenField::Algorithm => {
                self.cycle_algorithm(true);
            }
            // After a refusal: generate without a passphrase, explicitly.
            KeyCode::Char('n') if ctrl && self.retype() => {
                self.handed = Handed::Nothing;
                self.passphrase.zeroize();
                self.confirm.zeroize();
            }
            KeyCode::Char('u') if ctrl => {
                if self.focus == GenField::Name {
                    self.name_edited = true;
                }
                if let Some(v) = self.text_field() {
                    v.zeroize();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if self.focus == GenField::Name {
                    self.name_edited = true;
                }
                if let Some(v) = self.text_field() {
                    push_char(v, c, MAX_LINE);
                }
            }
            KeyCode::Backspace => {
                if self.focus == GenField::Name {
                    self.name_edited = true;
                }
                if let Some(v) = self.text_field() {
                    v.pop();
                }
            }
            _ => {}
        }
        // A new edit supersedes the engine's last refusal.
        self.error = None;
        GenOutcome::Pending
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.busy.is_some() {
            return;
        }
        if self.focus == GenField::Name {
            self.name_edited = true;
        }
        if let Some(v) = self.text_field() {
            paste_line(v, text, MAX_LINE);
        }
    }

    fn submit(&mut self) -> GenOutcome {
        self.submitted = true;
        if !self.valid() {
            // Jump to the first field that needs attention.
            self.focus = if self.name_problem().is_some() {
                GenField::Name
            } else if self.comment_problem().is_some() {
                GenField::Comment
            } else if self.passphrase_problem().is_some() {
                GenField::Passphrase
            } else {
                GenField::Confirm
            };
            return GenOutcome::Pending;
        }
        let passphrase =
            (!self.passphrase.is_empty()).then(|| SecretString::from(self.passphrase.as_str()));
        self.handed = if passphrase.is_some() {
            Handed::Passphrase
        } else {
            Handed::Nothing
        };
        // Handed over: wipe the typed copies.
        self.passphrase.zeroize();
        self.confirm.zeroize();
        self.submitted = false;
        self.busy = Some(format!("Generating {} key…", self.choice().label));
        GenOutcome::Submit(KeyGenRequest {
            name: self.name.trim().to_owned(),
            algorithm: self.choice().algorithm,
            comment: self.comment.trim().to_owned(),
            passphrase,
        })
    }
}

/// Fuzzy host picker of copy-id.
#[derive(Debug)]
pub(crate) struct HostPicker {
    pub(crate) key: LocalKey,
    hosts: Vec<HostEntry>,
    search: HostSearch,
    pub(crate) query: String,
    pub(crate) hits: Vec<Hit>,
    pub(crate) list: ListState,
}

impl HostPicker {
    pub(crate) fn new(key: LocalKey, hosts: Vec<HostEntry>) -> Self {
        let mut picker = Self {
            key,
            hosts,
            search: HostSearch::default(),
            query: String::new(),
            hits: Vec::new(),
            list: ListState::default(),
        };
        picker.refilter();
        picker
    }

    fn refilter(&mut self) {
        self.hits = self.search.search(&self.hosts, &self.query);
        self.list.select((!self.hits.is_empty()).then_some(0));
    }

    /// `(alias, user@host)` of a visible row.
    pub(crate) fn row(&self, hit: &Hit) -> Option<(&str, String)> {
        let host = self.hosts.get(hit.index)?;
        let r = &host.resolved;
        let target = match &r.user {
            Some(user) => format!("{user}@{}", r.hostname),
            None => r.hostname.clone(),
        };
        Some((host.alias.as_str(), target))
    }

    pub(crate) fn selected_alias(&self) -> Option<String> {
        let hit = self.hits.get(self.list.selected()?)?;
        self.hosts.get(hit.index).map(|h| h.alias.clone())
    }

    pub(crate) fn has_hosts(&self) -> bool {
        !self.hosts.is_empty()
    }
}

/// What the panel shows instead of (or over) the lists.
#[derive(Debug)]
pub(crate) enum Mode {
    Browse,
    Generate(GenForm),
    /// `a`: how long the agent keeps the key.
    Lifetime {
        key: LocalKey,
        choice: usize,
    },
    /// `x`: remove this identity from the agent?
    ConfirmRemove {
        fingerprint: String,
        label: String,
    },
    PickHost(HostPicker),
    /// Copy-id progress (the job is `App::copy_job`).
    Copy,
}

/// A one-line result shown above the key hints until the next key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) ok: bool,
    pub(crate) message: String,
    pub(crate) hint: Option<String>,
}

impl Notice {
    pub(crate) fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn error(error: ErrorSummary) -> Self {
        Self {
            ok: false,
            message: error.message,
            hint: error.hint,
        }
    }
}

/// What the app should do after a key in the panel.
#[derive(Debug)]
pub(crate) enum KeysOutcome {
    None,
    Close,
    Refresh,
    /// `n`: open the form (the app fills in the suggestions).
    NewKey,
    Generate(KeyGenRequest),
    AgentAdd {
        key: LocalKey,
        lifetime: Option<Duration>,
    },
    AgentRemove {
        fingerprint: String,
        label: String,
    },
    /// `c`: choose a host for this key (the app opens the picker).
    PickHost(LocalKey),
    CopyId {
        key: LocalKey,
        alias: String,
    },
    CopyPublic(LocalKey),
    /// `v`: unlock or lock the vault.
    VaultToggle,
    /// `V`: create the vault or change its passphrase.
    VaultPassphrase,
    CopyRetry,
    /// Leave the copy-id view (a running job continues).
    CopyLeave,
}

/// State of the copy-id job, as far as the panel's keys are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyState {
    None,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug)]
pub(crate) struct KeysPanel {
    pub(crate) local: Load<Vec<LocalKey>>,
    pub(crate) agent: Load<Vec<AgentKey>>,
    pub(crate) section: Section,
    pub(crate) local_sel: usize,
    pub(crate) agent_sel: usize,
    /// Scroll state of the rendered list (rows of both sections).
    pub(crate) list: ListState,
    pub(crate) mode: Mode,
    pub(crate) notice: Option<Notice>,
    /// Public key line shown after `y` (when the terminal may not support
    /// OSC 52, it can be selected from here).
    pub(crate) public_line: Option<String>,
    /// Host alias used for the suggested key name.
    pub(crate) host: Option<String>,
    /// Unix time the certificates are judged against (set at load).
    pub(crate) now: u64,
}

impl KeysPanel {
    pub(crate) fn new(host: Option<String>, now: u64) -> Self {
        Self {
            local: Load::Loading,
            agent: Load::Loading,
            section: Section::Local,
            local_sel: 0,
            agent_sel: 0,
            list: ListState::default(),
            mode: Mode::Browse,
            notice: None,
            public_line: None,
            host,
            now,
        }
    }

    /// Something is loading or being generated (spinner).
    pub(crate) fn busy(&self) -> bool {
        matches!(self.local, Load::Loading)
            || matches!(self.agent, Load::Loading)
            || matches!(&self.mode, Mode::Generate(form) if form.busy.is_some())
    }

    pub(crate) fn locals(&self) -> &[LocalKey] {
        self.local.ready().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn agents(&self) -> &[AgentKey] {
        self.agent.ready().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn set_local(&mut self, result: Result<Vec<LocalKey>, ErrorSummary>) {
        self.local = match result {
            Ok(keys) => Load::Ready(keys),
            Err(e) => Load::Failed(e),
        };
        self.local_sel = self.local_sel.min(self.locals().len().saturating_sub(1));
    }

    pub(crate) fn set_agent(&mut self, result: Result<Vec<AgentKey>, ErrorSummary>) {
        self.agent = match result {
            Ok(keys) => Load::Ready(keys),
            Err(e) => Load::Failed(e),
        };
        self.agent_sel = self.agent_sel.min(self.agents().len().saturating_sub(1));
    }

    /// Selects the local key at `path` (after generating it).
    pub(crate) fn select_local(&mut self, path: &std::path::Path) {
        if let Some(i) = self.locals().iter().position(|k| k.private_path == path) {
            self.section = Section::Local;
            self.local_sel = i;
        }
    }

    pub(crate) fn selected_local(&self) -> Option<&LocalKey> {
        (self.section == Section::Local)
            .then(|| self.locals().get(self.local_sel))
            .flatten()
    }

    pub(crate) fn selected_agent(&self) -> Option<&AgentKey> {
        (self.section == Section::Agent)
            .then(|| self.agents().get(self.agent_sel))
            .flatten()
    }

    /// The agent holds this key (as a plain key or a certificate).
    pub(crate) fn in_agent(&self, key: &LocalKey) -> bool {
        self.agents()
            .iter()
            .any(|a| a.fingerprint == key.fingerprint)
    }

    /// The local key pair an agent identity comes from, if known.
    pub(crate) fn local_of(&self, agent: &AgentKey) -> Option<&LocalKey> {
        self.locals()
            .iter()
            .find(|k| k.fingerprint == agent.fingerprint)
    }

    /// Fingerprint and label of the selection, if the agent holds it.
    fn removable(&self) -> Option<(String, String)> {
        if let Some(key) = self.selected_local() {
            return self
                .in_agent(key)
                .then(|| (key.fingerprint.clone(), file_name(&key.private_path)));
        }
        let agent = self.selected_agent()?;
        let label = self.local_of(agent).map_or_else(
            || {
                if agent.comment.is_empty() {
                    agent.algorithm.clone()
                } else {
                    agent.comment.clone()
                }
            },
            |k| file_name(&k.private_path),
        );
        Some((agent.fingerprint.clone(), label))
    }

    /// The local key behind the selection (either section).
    fn selected_pair(&self) -> Option<&LocalKey> {
        self.selected_local()
            .or_else(|| self.selected_agent().and_then(|a| self.local_of(a)))
    }

    /// Keys that do something right now, most important first. `Esc` is
    /// always last.
    pub(crate) fn hints(
        &self,
        vault: Option<VaultStatus>,
        copy: CopyState,
    ) -> Vec<(&'static str, &'static str)> {
        let mut hints = Vec::new();
        match &self.mode {
            Mode::Generate(form) => {
                if form.busy.is_some() {
                    return hints;
                }
                hints.push(("↵", "generate"));
                hints.push(("Tab", "next"));
                if form.focus == GenField::Algorithm {
                    hints.push(("← →", "algorithm"));
                }
                if form.retype() && form.passphrase.is_empty() {
                    hints.push(("Ctrl-N", "no passphrase"));
                }
                hints.push(("Esc", "cancel"));
                return hints;
            }
            Mode::Lifetime { .. } => {
                return vec![("↵", "add"), ("← →", "lifetime"), ("Esc", "cancel")];
            }
            Mode::ConfirmRemove { .. } => return vec![("y", "remove"), ("Esc", "keep")],
            Mode::PickHost(picker) => {
                if picker.hits.is_empty() {
                    return vec![("Esc", "back")];
                }
                return vec![("↵", "install"), ("↑↓", "move"), ("Esc", "back")];
            }
            Mode::Copy => {
                return match copy {
                    CopyState::Failed => vec![("↵ / r", "retry"), ("Esc", "back")],
                    CopyState::Running => vec![("Esc", "hide")],
                    _ => vec![("Esc", "back")],
                };
            }
            Mode::Browse => {}
        }
        let local = self.selected_local();
        if let Some(key) = local {
            if key.has_private && !self.in_agent(key) {
                hints.push(("a", "add to agent"));
            }
            hints.push(("c", "copy to host"));
        }
        if self.removable().is_some() {
            hints.push(("x", "remove from agent"));
        }
        if self.selected_pair().is_some() {
            hints.push(("y", "copy public key"));
        }
        hints.push(("n", "new key"));
        hints.push(("Tab", "section"));
        match vault {
            Some(VaultStatus::Locked) => hints.push(("v", "unlock vault")),
            Some(VaultStatus::Unlocked) => {
                hints.push(("v", "lock vault"));
                hints.push(("V", "passphrase"));
            }
            Some(VaultStatus::Absent) => hints.push(("V", "create vault")),
            Some(VaultStatus::NotInUse) | None => {}
        }
        hints.push(("Ctrl-R", "refresh"));
        hints.push(("Esc", "close"));
        hints
    }

    fn move_selection(&mut self, delta: isize) {
        let (local_len, agent_len) = (self.locals().len(), self.agents().len());
        let (sel, len) = match self.section {
            Section::Local => (&mut self.local_sel, local_len),
            Section::Agent => (&mut self.agent_sel, agent_len),
        };
        if len == 0 {
            return;
        }
        *sel = sel.saturating_add_signed(delta).min(len - 1);
    }

    // One arm per mode; splitting it would scatter the key table.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        vault: Option<VaultStatus>,
        copy: CopyState,
    ) -> KeysOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            Mode::Generate(form) => {
                return match form.handle_key(key) {
                    GenOutcome::Pending => KeysOutcome::None,
                    GenOutcome::Cancel => {
                        self.mode = Mode::Browse;
                        KeysOutcome::None
                    }
                    GenOutcome::Submit(request) => KeysOutcome::Generate(request),
                };
            }
            Mode::Lifetime { key: local, choice } => {
                let n = LIFETIMES.len();
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Browse,
                    KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                        *choice = (*choice + n - 1) % n;
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab | KeyCode::Char(' ') => {
                        *choice = (*choice + 1) % n;
                    }
                    KeyCode::Enter => {
                        let lifetime = LIFETIMES.get(*choice).and_then(|(_, d)| *d);
                        let key = local.clone();
                        self.mode = Mode::Browse;
                        return KeysOutcome::AgentAdd { key, lifetime };
                    }
                    _ => {}
                }
                return KeysOutcome::None;
            }
            Mode::ConfirmRemove { fingerprint, label } => {
                let outcome = match key.code {
                    KeyCode::Char('y' | 'Y') => KeysOutcome::AgentRemove {
                        fingerprint: fingerprint.clone(),
                        label: label.clone(),
                    },
                    _ => KeysOutcome::None,
                };
                self.mode = Mode::Browse;
                return outcome;
            }
            Mode::PickHost(picker) => {
                let len = picker.hits.len();
                let current = picker.list.selected().unwrap_or(0);
                match key.code {
                    KeyCode::Esc => self.mode = Mode::Browse,
                    KeyCode::Enter => {
                        if let Some(alias) = picker.selected_alias() {
                            let key = picker.key.clone();
                            self.mode = Mode::Copy;
                            return KeysOutcome::CopyId { key, alias };
                        }
                    }
                    KeyCode::Down | KeyCode::Tab if len > 0 => {
                        picker.list.select(Some((current + 1).min(len - 1)));
                    }
                    KeyCode::Char('n') if ctrl && len > 0 => {
                        picker.list.select(Some((current + 1).min(len - 1)));
                    }
                    KeyCode::Up | KeyCode::BackTab if len > 0 => {
                        picker.list.select(Some(current.saturating_sub(1)));
                    }
                    KeyCode::Char('p') if ctrl && len > 0 => {
                        picker.list.select(Some(current.saturating_sub(1)));
                    }
                    KeyCode::Char('u') if ctrl => {
                        picker.query.clear();
                        picker.refilter();
                    }
                    KeyCode::Backspace => {
                        picker.query.pop();
                        picker.refilter();
                    }
                    KeyCode::Char(c) if !ctrl => {
                        push_char(&mut picker.query, c, 64);
                        picker.refilter();
                    }
                    _ => {}
                }
                return KeysOutcome::None;
            }
            Mode::Copy => {
                return match key.code {
                    KeyCode::Char('r' | 'R') | KeyCode::Enter if copy == CopyState::Failed => {
                        KeysOutcome::CopyRetry
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.mode = Mode::Browse;
                        KeysOutcome::CopyLeave
                    }
                    _ => KeysOutcome::None,
                };
            }
            Mode::Browse => {}
        }
        // A notice stays until the next key.
        self.notice = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.public_line.take().is_some() {
                    return KeysOutcome::None;
                }
                return KeysOutcome::Close;
            }
            KeyCode::Char('r') if ctrl => return KeysOutcome::Refresh,
            KeyCode::Tab | KeyCode::BackTab => {
                self.section = match self.section {
                    Section::Local => Section::Agent,
                    Section::Agent => Section::Local,
                };
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX),
            KeyCode::Char('n') if !ctrl => return KeysOutcome::NewKey,
            KeyCode::Char('a') => {
                if let Some(local) = self
                    .selected_local()
                    .filter(|k| k.has_private && !self.in_agent(k))
                {
                    self.mode = Mode::Lifetime {
                        key: local.clone(),
                        choice: 0,
                    };
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => {
                if let Some((fingerprint, label)) = self.removable() {
                    self.mode = Mode::ConfirmRemove { fingerprint, label };
                }
            }
            KeyCode::Char('c') if !ctrl => {
                if let Some(local) = self.selected_local() {
                    return KeysOutcome::PickHost(local.clone());
                }
            }
            KeyCode::Char('y') => {
                if let Some(pair) = self.selected_pair() {
                    return KeysOutcome::CopyPublic(pair.clone());
                }
            }
            KeyCode::Char('v')
                if matches!(vault, Some(VaultStatus::Locked | VaultStatus::Unlocked)) =>
            {
                return KeysOutcome::VaultToggle;
            }
            KeyCode::Char('V')
                if matches!(vault, Some(VaultStatus::Absent | VaultStatus::Unlocked)) =>
            {
                return KeysOutcome::VaultPassphrase;
            }
            _ => {}
        }
        KeysOutcome::None
    }

    pub(crate) fn paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Generate(form) => form.paste(text),
            Mode::PickHost(picker) => {
                paste_line(&mut picker.query, text, 64);
                picker.refilter();
            }
            _ => {}
        }
    }
}

/// How a certificate stands, for its badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CertBadge {
    /// `valid until 2026-11-02` (or `no expiry`).
    Valid(String),
    /// `expires in 3 days` (warning colour).
    Soon(String),
    /// `expired` / `not yet valid` (error / warning colour).
    Expired,
    NotYet,
}

/// Badge text for a certificate validity.
pub(crate) fn cert_badge(validity: CertValidity, valid_before: u64) -> CertBadge {
    match validity {
        CertValidity::Valid {
            expires_in_secs: None,
        } => CertBadge::Valid("cert · no expiry".to_owned()),
        CertValidity::Valid { .. } => {
            CertBadge::Valid(format!("cert · valid until {}", date(valid_before)))
        }
        CertValidity::ExpiresSoon { expires_in_secs } => CertBadge::Soon(format!(
            "cert · expires in {}",
            duration_words(expires_in_secs)
        )),
        CertValidity::Expired { .. } => CertBadge::Expired,
        CertValidity::NotYetValid { .. } => CertBadge::NotYet,
    }
}

/// `3 days`, `1 day`, `5 hours`, `12 minutes`.
pub(crate) fn duration_words(secs: u64) -> String {
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit}")
        } else {
            format!("{n} {unit}s")
        }
    };
    if secs >= 86_400 {
        plural(secs / 86_400, "day")
    } else if secs >= 3600 {
        plural(secs / 3600, "hour")
    } else {
        plural((secs / 60).max(1), "minute")
    }
}

/// `YYYY-MM-DD` (UTC) of a Unix time.
pub(crate) fn date(unix: u64) -> String {
    i64::try_from(unix)
        .ok()
        .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok())
        .map_or_else(
            || "a far date".to_owned(),
            |d| format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day()),
        )
}

/// `SHA256:abcdefgh…uvwxyz` when the full fingerprint does not fit `max`.
pub(crate) fn short_fingerprint(fingerprint: &str, max: usize) -> String {
    let len = fingerprint.chars().count();
    if len <= max {
        return fingerprint.to_owned();
    }
    let (prefix, body) = fingerprint
        .split_once(':')
        .map_or(("", fingerprint), |(p, b)| (p, b));
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}:")
    };
    let room = max.saturating_sub(prefix.chars().count() + 1);
    let head = room.div_ceil(2);
    let tail = room / 2;
    let chars: Vec<char> = body.chars().collect();
    let start: String = chars.iter().take(head).collect();
    let end: String = chars
        .iter()
        .skip(chars.len().saturating_sub(tail))
        .collect();
    format!("{prefix}{start}…{end}")
}

/// `ssh-ed25519` → `ED25519`, `ecdsa-sha2-nistp256` → `ECDSA P-256`.
pub(crate) fn algorithm_label(algorithm: &str) -> String {
    let base = algorithm
        .trim_end_matches("-cert-v01@openssh.com")
        .trim_start_matches("sk-");
    let label = match base {
        "ssh-ed25519" | "ssh-ed25519@openssh.com" => "ED25519".to_owned(),
        "ssh-rsa" | "rsa-sha2-256" | "rsa-sha2-512" => "RSA".to_owned(),
        "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp256@openssh.com" => "ECDSA P-256".to_owned(),
        "ecdsa-sha2-nistp384" => "ECDSA P-384".to_owned(),
        "ecdsa-sha2-nistp521" => "ECDSA P-521".to_owned(),
        "ssh-dss" => "DSA".to_owned(),
        other => crate::form::display_safe(other),
    };
    if algorithm.starts_with("sk-") {
        format!("{label}-SK")
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn press(form: &mut GenForm, code: KeyCode) -> GenOutcome {
        form.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(form: &mut GenForm, text: &str) {
        for c in text.chars() {
            press(form, KeyCode::Char(c));
        }
    }

    fn local(name: &str, fingerprint: &str) -> LocalKey {
        LocalKey {
            private_path: PathBuf::from(format!("/h/.ssh/{name}")),
            public_path: PathBuf::from(format!("/h/.ssh/{name}.pub")),
            algorithm: "ssh-ed25519".into(),
            fingerprint: fingerprint.into(),
            comment: "me@laptop".into(),
            has_private: true,
            certificate: None,
        }
    }

    #[test]
    fn suggested_names_are_unique_and_follow_the_algorithm() {
        assert_eq!(host_hint(Some("Web 01/prod")), "web-01-prod");
        assert_eq!(host_hint(None), "portico");
        let taken = taken_names(&[local("id_ed25519_web", "SHA256:a")]);
        let mut form = GenForm::new("web".into(), "me@laptop".into(), taken);
        assert_eq!(form.name, "id_ed25519_web_2");
        form.focus = GenField::Algorithm;
        press(&mut form, KeyCode::Right);
        assert_eq!(form.name, "id_rsa_web");
        // Once typed in, the name is the user's.
        form.focus = GenField::Name;
        press(&mut form, KeyCode::Backspace);
        form.focus = GenField::Algorithm;
        press(&mut form, KeyCode::Right);
        assert_eq!(form.name, "id_rsa_we");
    }

    #[test]
    fn form_validation() {
        let taken = vec!["id_work".to_owned(), "id_work.pub".to_owned()];
        assert!(name_problem("id_work", &taken).is_some());
        assert!(name_problem("../evil", &taken).is_some());
        assert!(name_problem(".hidden", &taken).is_some());
        assert!(name_problem("", &taken).is_some());
        assert!(name_problem("id_new-1.key", &taken).is_none());

        let mut form = GenForm::new("web".into(), "me@laptop".into(), taken);
        form.focus = GenField::Passphrase;
        typed(&mut form, "correct horse");
        form.focus = GenField::Confirm;
        typed(&mut form, "correct hors");
        assert!(form.confirm_problem().is_some());
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            GenOutcome::Pending
        ));
        assert_eq!(form.focus, GenField::Confirm);
        typed(&mut form, "e");
        let GenOutcome::Submit(request) = press(&mut form, KeyCode::Enter) else {
            panic!("valid form");
        };
        assert_eq!(request.name, "id_ed25519_web");
        assert!(request.passphrase.is_some());
        assert!(form.passphrase.is_empty() && form.confirm.is_empty());
        assert!(form.busy.is_some());
    }

    fn refusal() -> ErrorSummary {
        ErrorSummary {
            code: "key_exists".into(),
            message: "~/.ssh/id_ed25519_web already exists".into(),
            hint: None,
        }
    }

    #[test]
    fn a_refused_generation_requires_the_passphrase_again() {
        let mut form = GenForm::new("web".into(), "me@laptop".into(), Vec::new());
        form.focus = GenField::Passphrase;
        typed(&mut form, "correct horse");
        form.focus = GenField::Confirm;
        typed(&mut form, "correct horse");
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            GenOutcome::Submit(_)
        ));
        form.refused(refusal());
        assert!(form.busy.is_none() && form.error.is_some());
        // The fields were wiped: Enter must not generate an unencrypted key.
        assert!(form.passphrase.is_empty() && form.confirm.is_empty());
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            GenOutcome::Pending
        ));
        assert_eq!(
            form.passphrase_problem(),
            Some("Type the passphrase again.")
        );
        assert_eq!(form.focus, GenField::Passphrase);
        // Typed again: generates, with the passphrase.
        typed(&mut form, "correct horse");
        form.focus = GenField::Confirm;
        typed(&mut form, "correct horse");
        let GenOutcome::Submit(request) = press(&mut form, KeyCode::Enter) else {
            panic!("passphrase typed again");
        };
        assert!(request.passphrase.is_some());

        // Or no passphrase, said explicitly (Ctrl-N).
        form.refused(refusal());
        assert!(form.retype());
        form.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        let GenOutcome::Submit(request) = press(&mut form, KeyCode::Enter) else {
            panic!("no passphrase, confirmed");
        };
        assert!(request.passphrase.is_none());

        // A refusal of a request without a passphrase asks nothing more.
        form.refused(refusal());
        assert!(!form.retype());
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            GenOutcome::Submit(_)
        ));
    }

    #[test]
    fn a_short_key_passphrase_is_refused() {
        let mut form = GenForm::new("web".into(), "me@laptop".into(), Vec::new());
        form.focus = GenField::Passphrase;
        typed(&mut form, "short");
        assert_eq!(form.passphrase_progress(), 5);
        form.focus = GenField::Confirm;
        typed(&mut form, "short");
        assert!(matches!(
            press(&mut form, KeyCode::Enter),
            GenOutcome::Pending
        ));
        assert!(form.passphrase_problem().is_some());
        form.focus = GenField::Passphrase;
        typed(&mut form, " but longer now");
        assert_eq!(form.passphrase_progress(), MIN_PASSPHRASE_CHARS);
    }

    #[test]
    fn empty_passphrase_is_allowed() {
        let mut form = GenForm::new("web".into(), "me@laptop".into(), Vec::new());
        let GenOutcome::Submit(request) = press(&mut form, KeyCode::Enter) else {
            panic!("valid form");
        };
        assert!(request.passphrase.is_none());
    }

    #[test]
    fn actions_follow_the_selection() {
        let mut panel = KeysPanel::new(None, 0);
        panel.set_local(Ok(vec![
            local("id_a", "SHA256:a"),
            local("id_b", "SHA256:b"),
        ]));
        panel.set_agent(Ok(vec![AgentKey {
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:b".into(),
            comment: String::new(),
            certificate: None,
        }]));
        let hints = panel.hints(None, CopyState::None);
        assert!(hints.contains(&("a", "add to agent")));
        assert!(!hints.iter().any(|(k, _)| *k == "x"));
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        panel.handle_key(key('j'), None, CopyState::None);
        let hints = panel.hints(None, CopyState::None);
        assert!(!hints.contains(&("a", "add to agent")));
        assert!(hints.contains(&("x", "remove from agent")));
        panel.handle_key(key('x'), None, CopyState::None);
        assert!(matches!(panel.mode, Mode::ConfirmRemove { .. }));
        assert!(matches!(
            panel.handle_key(key('y'), None, CopyState::None),
            KeysOutcome::AgentRemove { .. }
        ));
        // Vault keys only when the vault is in use.
        assert!(matches!(
            panel.handle_key(key('v'), Some(VaultStatus::NotInUse), CopyState::None),
            KeysOutcome::None
        ));
        assert!(matches!(
            panel.handle_key(key('v'), Some(VaultStatus::Locked), CopyState::None),
            KeysOutcome::VaultToggle
        ));
    }

    #[test]
    fn lifetime_choice_then_add() {
        let mut panel = KeysPanel::new(None, 0);
        panel.set_local(Ok(vec![local("id_a", "SHA256:a")]));
        panel.set_agent(Ok(Vec::new()));
        let k = |code| KeyEvent::new(code, KeyModifiers::NONE);
        panel.handle_key(k(KeyCode::Char('a')), None, CopyState::None);
        panel.handle_key(k(KeyCode::Right), None, CopyState::None);
        let KeysOutcome::AgentAdd { lifetime, .. } =
            panel.handle_key(k(KeyCode::Enter), None, CopyState::None)
        else {
            panic!("add");
        };
        assert_eq!(lifetime, Some(Duration::from_secs(3600)));
    }

    #[test]
    fn display_helpers() {
        let fp = "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefg";
        let short = short_fingerprint(fp, 24);
        assert_eq!(short.chars().count(), 24);
        assert!(short.starts_with("SHA256:AbCdEfGh") && short.contains('…'));
        assert_eq!(short_fingerprint(fp, 80), fp);
        assert_eq!(algorithm_label("ecdsa-sha2-nistp384"), "ECDSA P-384");
        assert_eq!(
            algorithm_label("ssh-ed25519-cert-v01@openssh.com"),
            "ED25519"
        );
        assert_eq!(duration_words(3 * 86_400 + 5), "3 days");
        assert_eq!(duration_words(3600), "1 hour");
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(
            cert_badge(
                CertValidity::ExpiresSoon {
                    expires_in_secs: 3 * 86_400
                },
                0
            ),
            CertBadge::Soon("cert · expires in 3 days".into())
        );
    }
}
