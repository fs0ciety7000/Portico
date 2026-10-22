//! "Add host…", "Edit host…" and "Duplicate as Portico host…" (phase 6b,
//! ADR "Hôtes créés dans Portico"), pure: the form state, its keys, and
//! the [`HostDraft`] it hands to the engine.
//!
//! - Hosts created here live in Portico's own `hosts.conf`; Portico never
//!   edits `~/.ssh/config`. A host of `~/.ssh/config` (or of the Termius
//!   import) opens the read-only view instead, which offers to duplicate it.
//! - Every value is checked by the engine, never here (the port only takes
//!   digits): a refusal comes back as an [`ErrorSummary`] and is shown under
//!   the field its **code** names ([`field_of`]), with its hint. Messages
//!   are never matched on text.
//! - The password is typed into a `Zeroizing` buffer that never reallocates,
//!   is never displayed (a fixed mask), never logged, wiped on cancel and
//!   handed to the engine as a `SecretString`.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::hosts::{
    HostEntry, HostOrigin, MAX_GROUP_CHARS, MAX_NOTES_CHARS, MAX_TAG_CHARS, MAX_TAGS,
};
use portico_core::import::ImportSource;
use portico_core::keys::LocalKey;
use portico_core::managed_hosts::{HostDraft, ManagedHostDetails, PasswordChange};
use secrecy::SecretString;
use zeroize::{Zeroize, Zeroizing};

use crate::edit_host::split_tags;
use crate::form::{MAX_LINE, paste_line, push_char, secret_buffer};

/// Command name shared with the GUI (the delete confirmation's title).
pub(crate) const DELETE_COMMAND: &str = "Delete host";
/// Read-only hint of a host of `~/.ssh/config` (shared with the GUI).
pub(crate) const READ_ONLY_HINT: &str = "Defined in ~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it.";
/// Read-only hint of a host of the old import file (`imports/termius.conf`,
/// written by Portico before imports became Portico hosts; shared with the
/// GUI).
pub(crate) const IMPORTED_HINT: &str = "Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it.";
/// Help of the form of a Portico host that came from an import (shared
/// with the GUI): an edit marks it, and later imports leave it alone.
pub(crate) const PROVENANCE_HELP: &str = "Editing it keeps later imports from updating it.";
/// Replaces the remembered-password lines once `HostName`, `User` or
/// `Port` changed (shared with the GUI): the core forgets a password whose
/// account changes.
pub(crate) const DESTINATION_CHANGE_NOTE: &str = "Changing the server or user forgets the remembered password. Choose Change to set one for the new destination.";
/// Edit: a password is remembered for the host's account, or not (shared
/// with the GUI).
pub(crate) const PASSWORD_REMEMBERED: &str = "A password is remembered";
pub(crate) const NO_PASSWORD_REMEMBERED: &str = "No password remembered";
/// The key picker's first choice (shared with the GUI): no `IdentityFile`,
/// so ssh-agent and the default keys are tried.
pub(crate) const KEY_NONE_LABEL: &str = "none · agent and default keys";

/// Under the password once one is set or kept (shared with the GUI): it
/// is not tied to a host key until the first connection (`SECURITY.md`,
/// risk 13).
pub(crate) const PASSWORD_DESTINATION_NOTE: &str = "Sent automatically to the server that answers at this address, after its host key is verified; then tied to that key.";

/// Longest alias the engine accepts (`[a-z0-9_.-]`, 64 characters).
const MAX_ALIAS: usize = 64;
/// Longest host name typed (the engine checks the rest).
const MAX_HOSTNAME: usize = 255;
/// Longest user name typed.
const MAX_USER: usize = 64;
/// Digits of a port (the engine refuses anything above 65535).
const MAX_PORT_DIGITS: usize = 5;
/// Room for the most tags, separators included.
const MAX_TAGS_INPUT: usize = (MAX_TAG_CHARS + 2) * MAX_TAGS;

/// The origin label shown in the list and the details (shared with the GUI).
pub(crate) fn origin_label(origin: HostOrigin) -> &'static str {
    match origin {
        HostOrigin::Portico => "Portico",
        HostOrigin::SshConfig => "~/.ssh/config",
        HostOrigin::Imported => "Imported",
    }
}

/// `Portico · from Termix` for a Portico host that came from an import,
/// else the plain origin label (shared with the GUI).
pub(crate) fn origin_with_source(origin: HostOrigin, source: Option<ImportSource>) -> String {
    match (origin, source) {
        (HostOrigin::Portico, Some(source)) => {
            format!("{} · from {}", origin_label(origin), source.label())
        }
        _ => origin_label(origin).to_owned(),
    }
}

/// The hint of a host Portico does not edit.
pub(crate) fn read_only_hint(origin: HostOrigin) -> &'static str {
    match origin {
        HostOrigin::Imported => IMPORTED_HINT,
        HostOrigin::SshConfig | HostOrigin::Portico => READ_ONLY_HINT,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostField {
    Alias,
    HostName,
    User,
    Port,
    Key,
    Jump,
    Tags,
    Group,
    Favorite,
    Notes,
    /// Edit only: keep, change or forget the remembered password.
    PasswordMode,
    Password,
}

/// The field a core error code concerns (`None`: shown above the keys).
pub(crate) fn field_of(code: &str) -> Option<HostField> {
    Some(match code {
        "host_invalid_alias" | "host_alias_taken" => HostField::Alias,
        "host_invalid_hostname" => HostField::HostName,
        "host_invalid_user" => HostField::User,
        "host_invalid_port" => HostField::Port,
        "host_invalid_identity_file" => HostField::Key,
        "host_invalid_proxy_jump" | "host_proxy_jump_cycle" => HostField::Jump,
        "host_tag" | "host_tags" => HostField::Tags,
        "host_group" => HostField::Group,
        "host_notes" => HostField::Notes,
        // A password to remember, and no vault to hold it.
        "vault_missing" | "vault_locked" => HostField::Password,
        _ => return None,
    })
}

/// What happens to the remembered password on edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordMode {
    Keep,
    Change,
    Forget,
}

impl PasswordMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Keep => "Keep",
            Self::Change => "Change",
            Self::Forget => "Forget",
        }
    }

    fn cycle(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Keep, true) | (Self::Forget, false) => Self::Change,
            (Self::Change, true) | (Self::Keep, false) => Self::Forget,
            (Self::Forget, true) | (Self::Change, false) => Self::Keep,
        }
    }
}

/// One choice of the key picker: `None` is "none".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyChoice {
    pub(crate) path: Option<PathBuf>,
    /// `~/.ssh/id_ed25519`, as shown.
    pub(crate) label: String,
    /// `ED25519`, or `not discovered` for a key kept from the host.
    pub(crate) note: String,
}

impl KeyChoice {
    fn none() -> Self {
        Self {
            path: None,
            label: KEY_NONE_LABEL.into(),
            note: String::new(),
        }
    }
}

/// What the form is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FormMode {
    Create,
    /// Editing the Portico host `original` (renaming allowed).
    Edit {
        original: String,
    },
    /// A copy of `source` (any origin) under a new alias.
    Duplicate {
        source: String,
    },
    /// A host Portico does not edit: the hint, and the way to duplicate it.
    ReadOnly {
        alias: String,
        origin: HostOrigin,
    },
}

/// A save for the engine.
#[derive(Debug)]
pub(crate) enum HostRequest {
    Create(HostDraft),
    Update { original: String, draft: HostDraft },
    Duplicate { source: String, new_alias: String },
}

// One outcome per key, moved once to its handler: boxing the draft would
// only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(crate) enum FormOutcome {
    None,
    /// Closed (cancelled, or nothing to save).
    Close,
    Save(HostRequest),
    /// The read-only view asks for the tags, group and notes editor.
    EditMeta(String),
}

/// Everything compared to tell whether the form holds unsaved changes
/// (the password is checked apart, never copied).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    alias: String,
    hostname: String,
    user: String,
    port: String,
    key: Option<PathBuf>,
    jump: Option<String>,
    tags: String,
    group: String,
    favorite: bool,
    notes: String,
}

// Independent form flags (favorite, keys listed, busy, discard question),
// not one state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(crate) struct HostForm {
    pub(crate) mode: FormMode,
    pub(crate) alias: String,
    pub(crate) hostname: String,
    pub(crate) user: String,
    pub(crate) port: String,
    pub(crate) keys: Vec<KeyChoice>,
    pub(crate) key: usize,
    /// `false` until the local keys are listed.
    pub(crate) keys_loaded: bool,
    /// Edit: `true` until the host's own values are read from `hosts.conf`
    /// ([`Self::set_managed`]); nothing can be typed or saved meanwhile.
    pub(crate) loading: bool,
    /// Every other host alias (`ProxyJump` choices), `none` first.
    pub(crate) jumps: Vec<Option<String>>,
    pub(crate) jump: usize,
    pub(crate) tags: String,
    pub(crate) group: String,
    pub(crate) favorite: bool,
    pub(crate) notes: String,
    pub(crate) password_mode: PasswordMode,
    /// The mode the form opened with (`Change` when nothing is remembered).
    initial_password_mode: PasswordMode,
    /// Edit: a password is remembered for the host's account (`None` until
    /// `hosts.conf` is read). Without one, only "Set" is offered.
    pub(crate) password_remembered: Option<bool>,
    /// Edit: the import the host came from (`Portico · from Termix`).
    pub(crate) imported_from: Option<ImportSource>,
    /// The save was refused for want of a vault (`vault_missing` /
    /// `vault_locked`): the create / unlock dialog is open, and this
    /// refusal is shown under the password if the user says "not now".
    pub(crate) vault_wait: Option<ErrorSummary>,
    /// Never shown, never logged (redacted `Debug`).
    password: PasswordInput,
    pub(crate) focus: HostField,
    /// Refusal from the engine: its field (`None`: the whole form).
    pub(crate) error: Option<(Option<HostField>, ErrorSummary)>,
    pub(crate) busy: bool,
    /// `Esc` with unsaved changes: `y` discards, `n` / `Esc` go back.
    pub(crate) confirm_discard: bool,
    initial: Snapshot,
}

/// The password being typed: `Zeroizing`, pre-sized (no reallocation, so
/// no stray copy), and a `Debug` that never shows it.
pub(crate) struct PasswordInput(Zeroizing<String>);

impl std::fmt::Debug for PasswordInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PasswordInput([redacted])")
    }
}

/// `~/…` for a path under `home`.
pub(crate) fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// A suggested alias for a copy of `alias`: the characters Portico
/// accepts, lowercase, then `-copy`.
pub(crate) fn copy_alias(alias: &str) -> String {
    let mut out: String = alias
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with(['-', '.']) {
        out.remove(0);
    }
    let room = MAX_ALIAS.saturating_sub("-copy".len());
    out = out.chars().take(room).collect();
    if out.is_empty() {
        "host-copy".into()
    } else {
        format!("{out}-copy")
    }
}

impl HostForm {
    fn blank(mode: FormMode, jumps: Vec<Option<String>>) -> Self {
        let mut form = Self {
            mode,
            alias: String::new(),
            hostname: String::new(),
            user: String::new(),
            port: String::new(),
            keys: vec![KeyChoice::none()],
            key: 0,
            keys_loaded: false,
            loading: false,
            jumps,
            jump: 0,
            tags: String::new(),
            group: String::new(),
            favorite: false,
            notes: String::new(),
            password_mode: PasswordMode::Keep,
            initial_password_mode: PasswordMode::Keep,
            password_remembered: None,
            imported_from: None,
            vault_wait: None,
            password: PasswordInput(secret_buffer()),
            focus: HostField::Alias,
            error: None,
            busy: false,
            confirm_discard: false,
            initial: Snapshot {
                alias: String::new(),
                hostname: String::new(),
                user: String::new(),
                port: String::new(),
                key: None,
                jump: None,
                tags: String::new(),
                group: String::new(),
                favorite: false,
                notes: String::new(),
            },
        };
        form.initial = form.snapshot();
        form
    }

    /// `none`, then every alias but `except` (the host itself).
    fn jump_choices(aliases: &[String], except: Option<&str>) -> Vec<Option<String>> {
        std::iter::once(None)
            .chain(
                aliases
                    .iter()
                    .filter(|a| except.is_none_or(|e| !a.eq_ignore_ascii_case(e)))
                    .cloned()
                    .map(Some),
            )
            .collect()
    }

    /// "Add host…": an empty form. `aliases`: every known host.
    pub(crate) fn create(aliases: &[String]) -> Self {
        Self::blank(FormMode::Create, Self::jump_choices(aliases, None))
    }

    /// "Edit host…" on a Portico host: its metadata and the password kept;
    /// its own values come next from `hosts.conf` ([`Self::set_managed`]),
    /// never from the resolved options (an earlier block of
    /// `~/.ssh/config` can hide them, and saving would erase them).
    pub(crate) fn edit(host: &HostEntry, aliases: &[String]) -> Self {
        let mut form = Self::blank(
            FormMode::Edit {
                original: host.alias.clone(),
            },
            Self::jump_choices(aliases, Some(&host.alias)),
        );
        form.alias.clone_from(&host.alias);
        form.tags = host.meta.tags.join(" ");
        form.group = host.meta.group.clone().unwrap_or_default();
        form.favorite = host.meta.favorite;
        form.notes = host.meta.notes.clone().unwrap_or_default();
        form.imported_from = host.imported_from;
        form.loading = true;
        form.initial = form.snapshot();
        form
    }

    /// The values `hosts.conf` holds for the edited host
    /// (`Engine::managed_host_details`): host name, user, port, key, jump
    /// host, whether a password is remembered and the import it came from.
    /// The form becomes editable; what it holds now is "unchanged".
    pub(crate) fn set_managed(&mut self, details: &ManagedHostDetails, home: &Path) {
        let host = &details.host;
        self.password_remembered = Some(details.password_remembered);
        // Nothing to keep or forget: "Set", with an empty field.
        self.password_mode = if details.password_remembered {
            PasswordMode::Keep
        } else {
            PasswordMode::Change
        };
        self.initial_password_mode = self.password_mode;
        if details.imported_from.is_some() {
            self.imported_from = details.imported_from;
        }
        self.hostname.clone_from(&host.hostname);
        self.user = host.user.clone().unwrap_or_default();
        self.port = host.port.map(|p| p.to_string()).unwrap_or_default();
        if let Some(path) = &host.identity_file {
            let known = self.keys.iter().position(|k| k.path.as_ref() == Some(path));
            self.key = known.unwrap_or_else(|| {
                self.keys.push(KeyChoice {
                    path: Some(path.clone()),
                    label: tilde(home, path),
                    // Listed already: this key was not among them.
                    note: if self.keys_loaded {
                        "not discovered".into()
                    } else {
                        String::new()
                    },
                });
                self.keys.len().saturating_sub(1)
            });
        }
        if let Some(jump) = &host.proxy_jump {
            let known = self
                .jumps
                .iter()
                .position(|j| j.as_deref().is_some_and(|j| j.eq_ignore_ascii_case(jump)));
            if let Some(i) = known {
                self.jump = i;
            } else {
                // Kept as written, even if that host is gone.
                self.jumps.push(Some(jump.clone()));
                self.jump = self.jumps.len().saturating_sub(1);
            }
        }
        self.loading = false;
        self.initial = self.snapshot();
    }

    /// "Duplicate as Portico host…": only the new alias is asked.
    pub(crate) fn duplicate(source: &str) -> Self {
        let mut form = Self::blank(
            FormMode::Duplicate {
                source: source.to_owned(),
            },
            vec![None],
        );
        form.alias = copy_alias(source);
        form.initial = form.snapshot();
        form
    }

    /// "Edit host…" on a host Portico does not edit.
    pub(crate) fn read_only(alias: &str, origin: HostOrigin) -> Self {
        Self::blank(
            FormMode::ReadOnly {
                alias: alias.to_owned(),
                origin,
            },
            vec![None],
        )
    }

    /// The local private keys: `none`, then each one (the key the host
    /// uses stays selected, listed even when it was not discovered).
    pub(crate) fn set_keys(&mut self, keys: &[LocalKey], home: &Path) {
        let current = self.keys.get(self.key).and_then(|k| k.path.clone());
        let mut choices = vec![KeyChoice::none()];
        choices.extend(keys.iter().filter(|k| k.has_private).map(|k| KeyChoice {
            path: Some(k.private_path.clone()),
            label: tilde(home, &k.private_path),
            note: k.algorithm.clone(),
        }));
        if let Some(path) = &current
            && !choices.iter().any(|c| c.path.as_ref() == Some(path))
        {
            choices.push(KeyChoice {
                path: Some(path.clone()),
                label: tilde(home, path),
                note: "not discovered".into(),
            });
        }
        self.key = current
            .and_then(|p| choices.iter().position(|c| c.path.as_ref() == Some(&p)))
            .unwrap_or(0);
        self.keys = choices;
        self.keys_loaded = true;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            alias: self.alias.clone(),
            hostname: self.hostname.clone(),
            user: self.user.clone(),
            port: self.port.clone(),
            key: self.keys.get(self.key).and_then(|k| k.path.clone()),
            jump: self.jumps.get(self.jump).cloned().flatten(),
            tags: self.tags.clone(),
            group: self.group.clone(),
            favorite: self.favorite,
            notes: self.notes.clone(),
        }
    }

    /// Something typed or chosen would be lost on cancel.
    pub(crate) fn dirty(&self) -> bool {
        self.snapshot() != self.initial
            || !self.password.0.is_empty()
            || self.password_mode != self.initial_password_mode
    }

    /// Edit: `HostName`, `User` or `Port` differ from the values read, so
    /// the remembered password (tied to the old account) is forgotten.
    pub(crate) fn destination_changed(&self) -> bool {
        matches!(self.mode, FormMode::Edit { .. })
            && (self.hostname.trim() != self.initial.hostname.trim()
                || self.user.trim() != self.initial.user.trim()
                || self.port.trim() != self.initial.port.trim())
    }

    /// Edit: a password is remembered (Keep and Forget make sense).
    pub(crate) fn remembered(&self) -> bool {
        self.password_remembered == Some(true)
    }

    /// The label of the password choice: "Set" when nothing is remembered.
    pub(crate) fn password_mode_label(&self) -> &'static str {
        match self.password_mode {
            PasswordMode::Change if !self.remembered() => "Set",
            mode => mode.label(),
        }
    }

    /// The focus is on a picker or toggle (`←` / `→` choose).
    pub(crate) fn on_picker(&self) -> bool {
        matches!(
            self.focus,
            HostField::Key | HostField::Jump | HostField::Favorite | HostField::PasswordMode
        )
    }

    /// A password has been typed (the mask is shown, never its length).
    pub(crate) fn password_typed(&self) -> bool {
        !self.password.0.is_empty()
    }

    /// Saving sets a password (typed) or keeps the remembered one (edit,
    /// "Keep", same destination): the form then says where it will be sent.
    pub(crate) fn password_set_or_kept(&self) -> bool {
        match (&self.mode, self.password_mode) {
            (FormMode::ReadOnly { .. }, _) | (FormMode::Edit { .. }, PasswordMode::Forget) => false,
            (FormMode::Edit { .. }, PasswordMode::Keep) => {
                self.remembered() && !self.destination_changed()
            }
            _ => self.password_typed(),
        }
    }

    /// Edit, "Keep", a password remembered, and the destination changed:
    /// the form says it will be forgotten instead of where it goes.
    pub(crate) fn forgets_on_destination_change(&self) -> bool {
        matches!(self.mode, FormMode::Edit { .. })
            && self.password_mode == PasswordMode::Keep
            && self.remembered()
            && self.destination_changed()
    }

    /// The fields, in `Tab` order.
    pub(crate) fn fields(&self) -> Vec<HostField> {
        use HostField::{
            Alias, Favorite, Group, HostName, Jump, Key, Notes, Password, PasswordMode as Mode,
            Port, Tags, User,
        };
        match &self.mode {
            FormMode::ReadOnly { .. } => Vec::new(),
            FormMode::Duplicate { .. } => vec![Alias],
            FormMode::Create => vec![
                Alias, HostName, User, Port, Key, Jump, Password, Tags, Group, Favorite, Notes,
            ],
            FormMode::Edit { .. } => {
                let mut fields = vec![Alias, HostName, User, Port, Key, Jump];
                // Without a remembered password, "Set" is the only choice:
                // the field itself, as when adding.
                if self.remembered() {
                    fields.push(Mode);
                }
                if self.password_mode == PasswordMode::Change {
                    fields.push(Password);
                }
                fields.extend([Tags, Group, Favorite, Notes]);
                fields
            }
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let fields = self.fields();
        let n = fields.len();
        if n == 0 {
            return;
        }
        let i = fields.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus = fields.get(next).copied().unwrap_or(HostField::Alias);
    }

    /// The focused text input and its limit (`None`: a picker or toggle).
    fn text(&mut self) -> Option<(&mut String, usize)> {
        Some(match self.focus {
            HostField::Alias => (&mut self.alias, MAX_ALIAS),
            HostField::HostName => (&mut self.hostname, MAX_HOSTNAME),
            HostField::User => (&mut self.user, MAX_USER),
            HostField::Port => (&mut self.port, MAX_PORT_DIGITS),
            HostField::Tags => (&mut self.tags, MAX_TAGS_INPUT),
            HostField::Group => (&mut self.group, MAX_GROUP_CHARS),
            HostField::Notes => (&mut self.notes, MAX_NOTES_CHARS),
            HostField::Password => (&mut *self.password.0, MAX_LINE),
            HostField::Key | HostField::Jump | HostField::Favorite | HostField::PasswordMode => {
                return None;
            }
        })
    }

    /// `←` / `→` (and `Space`) on a picker or toggle.
    fn choose(&mut self, forward: bool) {
        let step = |index: usize, len: usize| {
            let len = len.max(1);
            if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            }
        };
        match self.focus {
            HostField::Key => self.key = step(self.key, self.keys.len()),
            HostField::Jump => self.jump = step(self.jump, self.jumps.len()),
            HostField::Favorite => self.favorite = !self.favorite,
            // Keep and Forget only when a password is remembered.
            HostField::PasswordMode if self.remembered() => {
                self.password_mode = self.password_mode.cycle(forward);
                if self.password_mode != PasswordMode::Change {
                    self.password.0.zeroize();
                }
            }
            _ => {}
        }
    }

    /// Type-ahead in a picker: the next choice starting with `c`.
    fn jump_to(&mut self, c: char) {
        let c = c.to_ascii_lowercase();
        let starts = |label: &str| label.chars().next().map(|f| f.to_ascii_lowercase()) == Some(c);
        match self.focus {
            HostField::Key => {
                let labels: Vec<String> = self
                    .keys
                    .iter()
                    .map(|k| {
                        k.path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map_or_else(|| k.label.clone(), |n| n.to_string_lossy().into_owned())
                    })
                    .collect();
                if let Some(i) = next_match(&labels, self.key, starts) {
                    self.key = i;
                }
            }
            HostField::Jump => {
                let labels: Vec<String> = self
                    .jumps
                    .iter()
                    .map(|j| j.clone().unwrap_or_else(|| "none".into()))
                    .collect();
                if let Some(i) = next_match(&labels, self.jump, starts) {
                    self.jump = i;
                }
            }
            _ => {}
        }
    }

    fn draft(&self) -> HostDraft {
        let some = |text: &str| Some(text.trim().to_owned()).filter(|t| !t.is_empty());
        let password = match (&self.mode, self.password_mode) {
            (FormMode::Edit { .. }, PasswordMode::Forget) => PasswordChange::Forget,
            (FormMode::Edit { .. }, PasswordMode::Keep) => PasswordChange::Keep,
            _ if self.password.0.is_empty() => PasswordChange::Keep,
            _ => PasswordChange::Set(SecretString::from(self.password.0.as_str())),
        };
        HostDraft {
            alias: self.alias.trim().to_owned(),
            hostname: self.hostname.trim().to_owned(),
            user: some(&self.user),
            // Digits only, at most five: always a `u32`.
            port: some(&self.port).and_then(|p| p.parse().ok()),
            identity_file: self.keys.get(self.key).and_then(|k| k.path.clone()),
            proxy_jump: self.jumps.get(self.jump).cloned().flatten(),
            tags: split_tags(&self.tags),
            group: some(&self.group),
            favorite: self.favorite,
            notes: Some(self.notes.replace("\r\n", "\n").trim().to_owned())
                .filter(|n| !n.is_empty()),
            password,
        }
    }

    fn submit(&mut self) -> FormOutcome {
        let request = match &self.mode {
            FormMode::ReadOnly { .. } => return FormOutcome::None,
            FormMode::Create => HostRequest::Create(self.draft()),
            FormMode::Edit { original } => {
                if !self.dirty() {
                    return FormOutcome::Close;
                }
                HostRequest::Update {
                    original: original.clone(),
                    draft: self.draft(),
                }
            }
            FormMode::Duplicate { source } => HostRequest::Duplicate {
                source: source.clone(),
                new_alias: self.alias.trim().to_owned(),
            },
        };
        self.busy = true;
        self.error = None;
        FormOutcome::Save(request)
    }

    /// Wipes the password and closes.
    fn close(&mut self) -> FormOutcome {
        self.password.0.zeroize();
        FormOutcome::Close
    }

    fn read_only_key(&mut self, key: KeyEvent) -> FormOutcome {
        let FormMode::ReadOnly { alias, .. } = &self.mode else {
            return FormOutcome::None;
        };
        match key.code {
            KeyCode::Enter | KeyCode::Char('D' | 'd') => {
                let alias = alias.clone();
                *self = Self::duplicate(&alias);
                FormOutcome::None
            }
            KeyCode::Char('t') => FormOutcome::EditMeta(alias.clone()),
            KeyCode::Esc | KeyCode::Char('q') => FormOutcome::Close,
            _ => FormOutcome::None,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        if self.busy || key.kind == KeyEventKind::Release {
            return FormOutcome::None;
        }
        if self.loading {
            // Nothing typed yet, nothing to lose: only `Esc` does something.
            return match key.code {
                KeyCode::Esc => self.close(),
                _ => FormOutcome::None,
            };
        }
        if matches!(self.mode, FormMode::ReadOnly { .. }) {
            return self.read_only_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.confirm_discard {
            // `y` discards, `n` / `Esc` go back to the form; any other key
            // leaves the question up.
            return match key.code {
                KeyCode::Char('y') if !ctrl && !alt => self.close(),
                KeyCode::Char('n') if !ctrl && !alt => {
                    self.confirm_discard = false;
                    FormOutcome::None
                }
                KeyCode::Esc => {
                    self.confirm_discard = false;
                    FormOutcome::None
                }
                _ => FormOutcome::None,
            };
        }
        let editing = |form: &mut Self| {
            if form
                .error
                .as_ref()
                .is_some_and(|(f, _)| *f == Some(form.focus))
            {
                form.error = None;
            }
        };
        let picker = self.text().is_none();
        match key.code {
            KeyCode::Esc => {
                if self.dirty() {
                    self.confirm_discard = true;
                    return FormOutcome::None;
                }
                return self.close();
            }
            KeyCode::Enter if alt && self.focus == HostField::Notes => {
                editing(self);
                if self.notes.chars().count() < MAX_NOTES_CHARS {
                    self.notes.push('\n');
                }
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Tab | KeyCode::Down => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(false),
            KeyCode::Left | KeyCode::Right if picker => {
                editing(self);
                self.choose(key.code == KeyCode::Right);
            }
            KeyCode::Char(' ') if picker => {
                editing(self);
                self.choose(true);
            }
            KeyCode::Char(c) if picker && !ctrl && !alt => {
                editing(self);
                self.jump_to(c);
            }
            KeyCode::Char('u') if ctrl => {
                editing(self);
                if let Some((value, _)) = self.text() {
                    value.zeroize();
                }
            }
            KeyCode::Backspace => {
                editing(self);
                if let Some((value, _)) = self.text() {
                    value.pop();
                }
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                editing(self);
                let port = self.focus == HostField::Port;
                if let Some((value, max)) = self.text()
                    && (!port || c.is_ascii_digit())
                {
                    push_char(value, c, max);
                }
            }
            _ => {}
        }
        FormOutcome::None
    }

    /// A paste goes to the focused text input (first line; digits only
    /// for the port), never to a picker.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.busy
            || self.loading
            || self.confirm_discard
            || matches!(self.mode, FormMode::ReadOnly { .. })
        {
            return;
        }
        let port = self.focus == HostField::Port;
        if let Some((value, max)) = self.text() {
            if port {
                let digits: String = text
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect();
                paste_line(value, &digits, max);
            } else {
                paste_line(value, text, max);
            }
        }
    }

    /// The save sets a password: a `vault_missing` / `vault_locked`
    /// refusal then asks for the vault and retries.
    pub(crate) fn sets_password(&self) -> bool {
        matches!(self.draft().password, PasswordChange::Set(_))
    }

    /// The save was refused for want of a vault: waits for the create /
    /// unlock dialog (`error` is shown under the password if it is declined).
    pub(crate) fn wait_for_vault(&mut self, error: ErrorSummary) {
        self.busy = false;
        self.vault_wait = Some(error);
    }

    /// The vault is ready: the same save again (`None` if nothing waited).
    pub(crate) fn retry_after_vault(&mut self) -> Option<HostRequest> {
        self.vault_wait.take()?;
        match self.submit() {
            FormOutcome::Save(request) => Some(request),
            _ => None,
        }
    }

    /// "Not now": back to the form, the password still typed and the
    /// refusal under it.
    pub(crate) fn vault_declined(&mut self, home: &Path) {
        if let Some(error) = self.vault_wait.take() {
            self.refused(error, home);
        }
    }

    /// The engine refused the save: back to the field its code names, the
    /// message ready to show (see [`Self::shown_message`]).
    pub(crate) fn refused(&mut self, error: ErrorSummary, home: &Path) {
        self.busy = false;
        let field = field_of(&error.code).filter(|f| self.fields().contains(f));
        if let Some(field) = field {
            self.focus = field;
        }
        let message = self.shown_message(&error.message, home);
        self.error = Some((field, ErrorSummary { message, ..error }));
    }

    /// A core message as a sentence: paths under `home` as `~/…`, a first
    /// capital, unless it starts with a host alias (whose case is its own).
    fn shown_message(&self, message: &str, home: &Path) -> String {
        let home = home.to_string_lossy();
        let message = if home.len() > 1 {
            message.replace(&format!("{home}/"), "~/")
        } else {
            message.to_owned()
        };
        let first = message
            .split(char::is_whitespace)
            .next()
            .unwrap_or_default();
        let is_alias = !first.is_empty()
            && (first == self.alias.trim()
                || matches!(&self.mode, FormMode::Edit { original } if original == first)
                || matches!(&self.mode, FormMode::Duplicate { source } if source == first)
                || self.jumps.iter().flatten().any(|j| j == first));
        if is_alias {
            message
        } else {
            crate::ui::fmt::sentence(&message)
        }
    }
}

/// The first index after `from` (wrapping) whose label matches.
fn next_match(labels: &[String], from: usize, matches: impl Fn(&str) -> bool) -> Option<usize> {
    let n = labels.len();
    (1..=n)
        .map(|step| (from + step) % n.max(1))
        .find(|&i| labels.get(i).is_some_and(|l| matches(l)))
}

impl Drop for HostForm {
    fn drop(&mut self) {
        // `Zeroizing` wipes on drop too; said here so it is never forgotten.
        self.password.0.zeroize();
    }
}

#[cfg(test)]
mod tests;
