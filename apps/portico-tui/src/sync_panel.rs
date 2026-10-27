//! The "Sync" panel (phase 11, S1c): the encrypted folder sync of this
//! device, as `docs/SYNC.md` §6 and §10 ("Ce que S1c attend de S1b")
//! describe it.
//!
//! - **Tabs** (same order and `1`–`5` as the GUI): Overview (status,
//!   folder, key, actions), Conflicts (per-field diff, pick a side, Union
//!   preselected for tags, Keep both for snippets), Received (the hosts
//!   waiting for this device's confirmation, F1, then the snippets, the
//!   whole command, F8), Issues (code message and hint, "Skip on this
//!   device") and Devices.
//! - **Decisions are bound to what is shown**: a host is confirmed or
//!   rejected with the `(alias, destination_hash)` on screen, a snippet
//!   with its `hash`, a conflict with its `sides_hash`, one at a time (no
//!   bulk confirmation). Keys are ignored for [`ARM_DELAY`] after the item
//!   on screen changed (new selection, new content, new conflict sides),
//!   and while a `SyncPendingChanged` reload is in flight, so a stray `y`
//!   never confirms something not yet read. A received item is decided
//!   only once its last line has been on screen (`seen_end`, set by the
//!   view): a long command cannot hide its end below the fold (A1, S1c
//!   review).
//! - **Passphrases** are typed into `Zeroizing` buffers (never shown, not
//!   even their length) and handed to the engine as `SecretString` or
//!   [`NewPassphrase`]; a generated one is shown once, in the form that
//!   asked for it, and dropped with it.
//!
//! The data the host list also needs (status, received hosts, unconfirmed
//! snippets) lives in [`SyncData`] on the app; the panel only keeps what it
//! loads for itself (devices, conflicts, issues) and its selections.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::sync::{
    AlertKind, DisableOptions, FolderReason, NewPassphrase, PassphraseStrength, PendingHostChange,
    SyncConflict, SyncDevice, SyncIssue, SyncOverview, SyncResolution, SyncStatus, SyncSubject,
    SyncValue, UnconfirmedSnippet, WaitReason, assess_passphrase,
};
use secrecy::SecretString;
use time::OffsetDateTime;
use zeroize::Zeroize;

use crate::dialog::ARM_DELAY;
use crate::form::{MAX_LINE, paste_line, push_char, secret_buffer};
use crate::keys::{Load, Notice};

/// Longest device name (the core's limit).
pub(crate) const DEVICE_NAME_MAX: usize = 64;

/// What sync shares with the host list, the snippets panel and the status
/// bar (kept up to date by events and requests, panel open or not).
#[derive(Debug, Default)]
pub(crate) struct SyncData {
    /// `None` until the engine answered.
    pub(crate) overview: Option<SyncOverview>,
    /// From `SyncConflicts` (and the last pass's status).
    pub(crate) conflicts: usize,
    /// Received hosts waiting for this device's confirmation (F1).
    pub(crate) pending: Vec<PendingHostChange>,
    /// Snippets changed by sync, not confirmed on this device (F8).
    pub(crate) snippets: Vec<UnconfirmedSnippet>,
    /// `SyncPendingChanged` came and the reload of the received hosts
    /// (`stale_hosts`) or snippets (`stale_snippets`) has not: what is on
    /// screen may be old, decisions wait.
    pub(crate) stale_hosts: bool,
    pub(crate) stale_snippets: bool,
    /// Last time the pending hosts or snippets changed (decisions are armed
    /// [`ARM_DELAY`] after it).
    pub(crate) changed_at: Option<Instant>,
}

impl SyncData {
    /// A reload is in flight: decisions wait for it.
    pub(crate) fn stale(&self) -> bool {
        self.stale_hosts || self.stale_snippets
    }

    pub(crate) fn status(&self) -> Option<&SyncStatus> {
        self.overview.as_ref().map(|o| &o.status)
    }

    /// The pending change of `alias`, if any.
    pub(crate) fn pending_for(&self, alias: &str) -> Option<&PendingHostChange> {
        self.pending.iter().find(|p| p.alias == alias)
    }

    /// Received hosts and snippets waiting for a decision here.
    pub(crate) fn to_review(&self) -> usize {
        self.pending.len().saturating_add(self.snippets.len())
    }

    /// New pending hosts / snippets: arms decisions again when the content
    /// changed.
    pub(crate) fn set_pending(&mut self, pending: Vec<PendingHostChange>, now: Instant) {
        if pending != self.pending {
            self.changed_at = Some(now);
        }
        self.pending = pending;
    }

    pub(crate) fn set_snippets(&mut self, snippets: Vec<UnconfirmedSnippet>, now: Instant) {
        if snippets != self.snippets {
            self.changed_at = Some(now);
        }
        self.snippets = snippets;
    }
}

// ── status words (status bar, panel, wizard) ─────────────────────────────

/// How a status reads: neutral, in progress, needs attention, failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Off, synced.
    Neutral,
    /// Syncing, waiting.
    Busy,
    /// Locked, folder unavailable.
    Warning,
    /// An alert, an error.
    Error,
}

/// The monochrome sync glyph of the status bar.
pub(crate) const GLYPH: &str = "⇅";

/// `Synced 2 min ago`, `Syncing…`, `Waiting for laptop (3f9c0ae1)`,
/// `Sync folder unavailable`, `Sync locked`, `Sync off`…
pub(crate) fn status_label(status: &SyncStatus, now: OffsetDateTime) -> (String, Tone) {
    match status {
        SyncStatus::Disabled => ("Sync off".into(), Tone::Neutral),
        SyncStatus::Locked => ("Sync locked".into(), Tone::Warning),
        SyncStatus::Idle { last_pass, .. } => {
            let label = OffsetDateTime::from_unix_timestamp(*last_pass).map_or_else(
                |_| "Synced".to_owned(),
                |at| match crate::app_update::ago(at, now).as_str() {
                    "just now" => "Synced just now".to_owned(),
                    ago => format!("Synced {ago}"),
                },
            );
            (label, Tone::Neutral)
        }
        SyncStatus::Syncing => ("Syncing…".into(), Tone::Busy),
        SyncStatus::Waiting { reason, device } => {
            let label = match (device, reason) {
                (Some(device), _) => format!("Waiting for {device}"),
                (None, WaitReason::PartialSync) => "Waiting for files to arrive".into(),
                (None, WaitReason::Placeholder) => "Waiting for downloads".into(),
                (None, WaitReason::Deferred) => "Waiting for other devices".into(),
                (None, WaitReason::Busy) => "Waiting for the other Portico".into(),
                (None, _) => "Waiting".into(),
            };
            (label, Tone::Busy)
        }
        SyncStatus::FolderUnavailable { .. } => ("Sync folder unavailable".into(), Tone::Warning),
        SyncStatus::Alert { .. } => ("Sync alert".into(), Tone::Error),
        SyncStatus::Error { .. } => ("Sync error".into(), Tone::Error),
        _ => ("Sync".into(), Tone::Neutral),
    }
}

/// Why the folder is unavailable, and what to do (the panel's status).
pub(crate) fn folder_reason(reason: FolderReason) -> (&'static str, &'static str) {
    match reason {
        FolderReason::Missing => (
            "The sync folder or its portico-sync folder is gone.",
            "Check that the drive or the sync app is running. Portico never recreates it.",
        ),
        FolderReason::NotMounted => (
            "The sync folder did not answer in time.",
            "A network drive or a cloud folder may be offline or still downloading.",
        ),
        FolderReason::PermissionDenied => (
            "Portico cannot read or write the sync folder.",
            "Check the folder's permissions.",
        ),
        FolderReason::BundleMissing => (
            "portico-sync is there, but not bundle.toml.",
            "It may not be downloaded yet: wait for the sync app. Nothing is written meanwhile.",
        ),
        FolderReason::Unsafe => (
            "The sync folder is a link, or sits inside a private folder.",
            "Choose a plain folder of your cloud or sync app.",
        ),
        FolderReason::TooManyEntries => (
            "The sync folder lists too many device files.",
            "Sync is paused; nothing is deleted. Remove stray files from portico-sync/devices.",
        ),
        _ => ("The sync folder is unavailable.", "Check the folder."),
    }
}

/// An alert: what happened (naming the device) and what to do.
pub(crate) fn alert_text(kind: AlertKind, device: Option<&str>) -> (String, &'static str) {
    let who = device.unwrap_or("a device");
    match kind {
        AlertKind::Rollback => (
            "An older copy of a sync file was put back.".into(),
            "Nothing older is applied. Check the folder's version history, then sync again.",
        ),
        AlertKind::Fork => (
            format!("{who} published two different versions of its data."),
            "It may have been copied or restored from a backup: its data is held back. Set up sync again on that device.",
        ),
        AlertKind::Restored => (
            "This device's sync state was restored from a backup.".into(),
            "It joined again under a new id; its data comes back from the other devices.",
        ),
        AlertKind::BundleTampered => (
            "bundle.toml was changed without the key.".into(),
            "Nothing is applied. Restore it from the folder's version history, or disable sync and create a new sync folder.",
        ),
        AlertKind::KeysRotatedElsewhere => (
            format!("The keys were rotated on {who}."),
            "Unlock with the current passphrase.",
        ),
        AlertKind::PassphraseChanged => (
            format!("The passphrase was changed or reset on {who}."),
            "If nobody you trust did it, disable sync and create a new sync folder. Otherwise acknowledge it.",
        ),
        AlertKind::ThirdPartyClaim => (
            format!("{who} claims changes this device never made."),
            "Its data is held back. If you do not know that device, disable sync and create a new sync folder.",
        ),
        AlertKind::MergeRefused => (
            format!("The data of {who} exceeds the sync limits."),
            "It is not merged. Check that device.",
        ),
        AlertKind::Divergent => (
            format!("{who} sent two different values for one change."),
            "Its data is held back until it publishes again.",
        ),
        AlertKind::DroppedValues => (
            format!("{who} removed values without replacing them."),
            "Its data is held back. Check that device.",
        ),
        _ => ("Sync needs your attention.".into(), "Open the Issues tab."),
    }
}

/// `Host web-01`, `Tags of web-01`, `Snippet deploy`, `Setting theme`.
pub(crate) fn subject_label(subject: &SyncSubject) -> String {
    match subject {
        SyncSubject::Host { alias } => format!("Host {alias}"),
        SyncSubject::HostMeta { alias, field } => format!("{} of {alias}", field_label(field)),
        SyncSubject::Snippet { id } => format!("Snippet {id}"),
        SyncSubject::Setting { key } => format!("Setting {key}"),
        SyncSubject::Other { kind } => format!("A {kind} record"),
        _ => "A record".into(),
    }
}

fn field_label(field: &str) -> String {
    match field {
        "tags" => "Tags".into(),
        "group" => "Group".into(),
        "notes" => "Notes".into(),
        "favorite" => "Favorite".into(),
        other => crate::ui::fmt::sentence(other),
    }
}

// ── conflict choices ─────────────────────────────────────────────────────

/// One way to settle a conflict, as listed (the preselected one first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Tags: every side's tags.
    Union,
    /// Snippets: keep every side; `keep` stays under the id.
    KeepBoth { keep: usize },
    /// One side's value (`ConflictSide::index`).
    Pick { index: usize },
}

impl Choice {
    pub(crate) fn resolution(&self) -> SyncResolution {
        match self {
            Self::Union => SyncResolution::Union,
            Self::KeepBoth { keep } => SyncResolution::KeepBoth { keep: *keep },
            Self::Pick { index } => SyncResolution::Pick { index: *index },
        }
    }
}

/// The choices of `conflict`: Union first for tags, Keep both first for
/// snippets (owner decision #10, §6), then each side.
pub(crate) fn choices(conflict: &SyncConflict) -> Vec<Choice> {
    let mut out = Vec::new();
    let tags = conflict
        .sides
        .iter()
        .any(|s| matches!(s.value, SyncValue::Tags(_)));
    let snippets = conflict
        .sides
        .iter()
        .any(|s| matches!(s.value, SyncValue::Snippet(_)));
    if tags {
        out.push(Choice::Union);
    }
    if snippets {
        // The local side keeps the id (what this device already runs).
        let keep = conflict
            .sides
            .iter()
            .find(|s| s.local)
            .or_else(|| conflict.sides.first())
            .map_or(0, |s| s.index);
        out.push(Choice::KeepBoth { keep });
    }
    out.extend(
        conflict
            .sides
            .iter()
            .map(|s| Choice::Pick { index: s.index }),
    );
    out
}

// ── passphrase forms ─────────────────────────────────────────────────────

/// A field of a passphrase form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassField {
    /// The current passphrase (unlock, change, join).
    Old,
    /// The new passphrase (or the generated one, shown).
    New,
    /// Its confirmation, or "I wrote it down" for a generated one.
    Confirm,
    Name,
    Remember,
    /// "I saved it" (create): gates the step that writes (Q1, S1c review).
    Saved,
}

/// What a passphrase form is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassKind {
    /// First device: new passphrase, device name, remember.
    Create,
    /// Join: the passphrase, device name, remember.
    Join,
    Unlock,
    /// Change: current, then new passphrase.
    Change,
    /// Reset from this device (no current passphrase).
    Reset,
}

impl PassKind {
    /// The fields, in order. A generated passphrase needs no confirmation
    /// field: on create, "I saved it" covers it.
    fn fields(self, generated: bool) -> &'static [PassField] {
        match self {
            Self::Create if generated => &[
                PassField::New,
                PassField::Name,
                PassField::Remember,
                PassField::Saved,
            ],
            Self::Create => &[
                PassField::New,
                PassField::Confirm,
                PassField::Name,
                PassField::Remember,
                PassField::Saved,
            ],
            Self::Join => &[PassField::Old, PassField::Name, PassField::Remember],
            Self::Unlock => &[PassField::Old],
            Self::Change => &[PassField::Old, PassField::New, PassField::Confirm],
            Self::Reset => &[PassField::New, PassField::Confirm],
        }
    }
}

/// What the app should do after a key in a passphrase form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassOutcome {
    None,
    Cancel,
    /// Valid: hand it over (see [`PassForm::take_old`], [`PassForm::take_new`]).
    Submit,
    /// Ask the engine for a generated passphrase.
    Generate,
    /// Copy the generated passphrase (OSC 52).
    Copy,
}

/// A passphrase form. Secrets are `Zeroizing`, pre-sized, never shown.
pub(crate) struct PassForm {
    pub(crate) kind: PassKind,
    pub(crate) focus: usize,
    pub(crate) old: zeroize::Zeroizing<String>,
    pub(crate) new: zeroize::Zeroizing<String>,
    pub(crate) confirm: zeroize::Zeroizing<String>,
    /// A generated passphrase (always `NewPassphrase::Generated`), shown
    /// in place of the input until it is used or discarded.
    pub(crate) generated: Option<NewPassphrase>,
    /// "I wrote it down" for a generated passphrase (change, reset), or
    /// "I saved it. A lost passphrase cannot be recovered…" (create): one
    /// box per form, reset with the passphrase it is about.
    pub(crate) saved: bool,
    /// The meter of the new passphrase (the core's own estimate).
    pub(crate) strength: Option<PassphraseStrength>,
    pub(crate) name: String,
    pub(crate) remember: bool,
    /// A problem shown under a field (local check, or the core's refusal).
    pub(crate) problem: Option<(PassField, String, Option<String>)>,
    /// Waiting for the engine (key derivation can take a few seconds).
    pub(crate) busy: bool,
}

impl std::fmt::Debug for PassForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassForm")
            .field("kind", &self.kind)
            .field("focus", &self.focus)
            .field("generated", &self.generated.is_some())
            .field("name", &self.name)
            .field("remember", &self.remember)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

impl PassForm {
    pub(crate) fn new(kind: PassKind, name: String) -> Self {
        Self {
            kind,
            focus: 0,
            old: secret_buffer(),
            new: secret_buffer(),
            confirm: secret_buffer(),
            generated: None,
            saved: false,
            strength: None,
            name,
            remember: true,
            problem: None,
            busy: false,
        }
    }

    pub(crate) fn fields(&self) -> &'static [PassField] {
        self.kind.fields(self.generated.is_some())
    }

    pub(crate) fn focused(&self) -> Option<PassField> {
        self.fields().get(self.focus).copied()
    }

    /// The generated passphrase's words (to show them once).
    pub(crate) fn generated_text(&self) -> Option<&SecretString> {
        match &self.generated {
            Some(NewPassphrase::Generated(g)) => Some(g.as_secret()),
            _ => None,
        }
    }

    /// The engine generated a passphrase for this form.
    pub(crate) fn set_generated(&mut self, generated: portico_core::sync::GeneratedPassphrase) {
        self.new.zeroize();
        self.confirm.zeroize();
        let generated = NewPassphrase::Generated(generated);
        self.strength = Some(generated.strength());
        self.generated = Some(generated);
        self.saved = false;
        self.problem = None;
        if let Some(i) = self.fields().iter().position(|f| *f == PassField::New) {
            self.focus = i;
        }
    }

    fn discard_generated(&mut self) {
        self.generated = None;
        self.saved = false;
        self.strength = None;
        // The field list changed (a confirmation comes back): stay on New.
        if let Some(i) = self.fields().iter().position(|f| *f == PassField::New) {
            self.focus = i;
        }
    }

    fn reassess(&mut self) {
        self.strength = if self.new.is_empty() {
            None
        } else {
            Some(assess_passphrase(&SecretString::from(self.new.as_str())))
        };
    }

    /// The first local problem, with the field it belongs to.
    pub(crate) fn local_problem(&self) -> Option<(PassField, &'static str)> {
        for field in self.fields() {
            let problem = match field {
                PassField::Old if self.old.is_empty() => Some(match self.kind {
                    PassKind::Change => "Type the current passphrase",
                    _ => "Type the passphrase",
                }),
                PassField::New if self.generated.is_none() && self.new.is_empty() => {
                    Some("Type a passphrase, or generate one (Ctrl-G)")
                }
                PassField::Confirm if self.generated.is_some() && !self.saved => {
                    Some("Write the passphrase down, then check this box")
                }
                PassField::Confirm if self.generated.is_none() && *self.confirm != *self.new => {
                    Some("The two passphrases differ")
                }
                PassField::Name if self.name.trim().is_empty() => Some("Name this device"),
                PassField::Saved if !self.saved => Some("Save the passphrase, then check this box"),
                _ => None,
            };
            if let Some(problem) = problem {
                return Some((*field, problem));
            }
        }
        None
    }

    /// The core refused the request: the problem goes under its field.
    pub(crate) fn refused(&mut self, error: ErrorSummary) {
        self.busy = false;
        let field = match error.code.as_str() {
            "sync_passphrase_weak" => PassField::New,
            "sync_device_name_invalid" => PassField::Name,
            _ if self.fields().contains(&PassField::Old) => PassField::Old,
            _ => PassField::New,
        };
        self.problem = Some((field, error.message, error.hint));
        if let Some(i) = self.fields().iter().position(|f| *f == field) {
            self.focus = i;
        }
    }

    fn text_field(&mut self) -> Option<(&mut String, bool)> {
        match self.focused()? {
            PassField::Old => Some((&mut self.old, true)),
            PassField::New if self.generated.is_none() => Some((&mut self.new, true)),
            PassField::Confirm if self.generated.is_none() => Some((&mut self.confirm, true)),
            PassField::Name => Some((&mut self.name, false)),
            _ => None,
        }
    }

    fn move_focus(&mut self, forward: bool) {
        let n = self.fields().len();
        if n == 0 {
            return;
        }
        self.focus = if forward {
            (self.focus + 1) % n
        } else {
            (self.focus + n - 1) % n
        };
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PassOutcome {
        if self.busy {
            return PassOutcome::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let focused = self.focused();
        let has_new = self.fields().contains(&PassField::New);
        match key.code {
            KeyCode::Esc => return PassOutcome::Cancel,
            KeyCode::Char('c') if ctrl => return PassOutcome::Cancel,
            KeyCode::Tab | KeyCode::Down => self.move_focus(true),
            KeyCode::BackTab | KeyCode::Up => self.move_focus(false),
            KeyCode::Enter => {
                if let Some((field, problem)) = self.local_problem() {
                    self.problem = Some((field, problem.to_owned(), None));
                    if let Some(i) = self.fields().iter().position(|f| *f == field) {
                        self.focus = i;
                    }
                    return PassOutcome::None;
                }
                self.problem = None;
                return PassOutcome::Submit;
            }
            KeyCode::Char('g') if ctrl && has_new => return PassOutcome::Generate,
            // The generated passphrase, shown: copy, another, or type one.
            KeyCode::Char('c') if focused == Some(PassField::New) && self.generated.is_some() => {
                return PassOutcome::Copy;
            }
            KeyCode::Char('g') if focused == Some(PassField::New) && self.generated.is_some() => {
                return PassOutcome::Generate;
            }
            KeyCode::Backspace | KeyCode::Char('u')
                if focused == Some(PassField::New)
                    && self.generated.is_some()
                    && (key.code == KeyCode::Backspace || ctrl) =>
            {
                self.discard_generated();
            }
            KeyCode::Char(' ')
                if focused == Some(PassField::Confirm) && self.generated.is_some() =>
            {
                self.saved = !self.saved;
                self.problem = None;
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right
                if focused == Some(PassField::Remember) =>
            {
                self.remember = !self.remember;
            }
            KeyCode::Char(' ') if focused == Some(PassField::Saved) => {
                self.saved = !self.saved;
                if self
                    .problem
                    .as_ref()
                    .is_some_and(|(f, ..)| *f == PassField::Saved)
                {
                    self.problem = None;
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some((value, _)) = self.text_field() {
                    value.zeroize();
                }
                self.after_edit(focused);
            }
            KeyCode::Backspace => {
                if let Some((value, _)) = self.text_field() {
                    value.pop();
                }
                self.after_edit(focused);
            }
            KeyCode::Char(c) if !ctrl => {
                let max = if focused == Some(PassField::Name) {
                    DEVICE_NAME_MAX
                } else {
                    MAX_LINE
                };
                if let Some((value, _)) = self.text_field() {
                    push_char(value, c, max);
                }
                self.after_edit(focused);
            }
            _ => {}
        }
        PassOutcome::None
    }

    fn after_edit(&mut self, field: Option<PassField>) {
        if self
            .problem
            .as_ref()
            .is_some_and(|(f, ..)| Some(*f) == field)
        {
            self.problem = None;
        }
        if field == Some(PassField::New) {
            self.reassess();
        }
    }

    /// A paste (a password manager): the first line, into the focused
    /// text field.
    pub(crate) fn paste(&mut self, text: &str) {
        let focused = self.focused();
        let max = if focused == Some(PassField::Name) {
            DEVICE_NAME_MAX
        } else {
            MAX_LINE
        };
        if let Some((value, _)) = self.text_field() {
            paste_line(value, text, max);
        }
        self.after_edit(focused);
    }

    /// The current passphrase, wiped from the form.
    pub(crate) fn take_old(&mut self) -> SecretString {
        let secret = SecretString::from(self.old.as_str());
        self.old.zeroize();
        secret
    }

    /// The new passphrase (generated or typed), wiped from the form.
    pub(crate) fn take_new(&mut self) -> NewPassphrase {
        let new = self
            .generated
            .take()
            .unwrap_or_else(|| NewPassphrase::Typed(SecretString::from(self.new.as_str())));
        self.new.zeroize();
        self.confirm.zeroize();
        self.saved = false;
        self.strength = None;
        new
    }
}

// ── the panel ────────────────────────────────────────────────────────────

/// The panel's tabs, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncTab {
    Overview,
    Conflicts,
    /// Received hosts (F1), then received snippets (F8).
    Received,
    Issues,
    Devices,
}

/// In the GUI's order (`1`–`5` pick the same tab in both UIs).
pub(crate) const TABS: [SyncTab; 5] = [
    SyncTab::Overview,
    SyncTab::Conflicts,
    SyncTab::Received,
    SyncTab::Issues,
    SyncTab::Devices,
];

impl SyncTab {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Conflicts => "Conflicts",
            Self::Received => "Received",
            Self::Issues => "Issues",
            Self::Devices => "Devices",
        }
    }
}

/// The selected item of the Received tab.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ReceivedItem<'a> {
    Host(&'a PendingHostChange),
    Snippet(&'a UnconfirmedSnippet),
}

impl ReceivedItem<'_> {
    /// What a decision names: the destination or command hash.
    pub(crate) fn hash(&self) -> &str {
        match self {
            Self::Host(h) => &h.destination_hash,
            Self::Snippet(s) => &s.hash,
        }
    }
}

/// A form or question over the panel's tab.
#[derive(Debug)]
pub(crate) enum PanelMode {
    Browse,
    Unlock(PassForm),
    Change(PassForm),
    /// Reset from this device; `confirm` once the form is filled (armed:
    /// only `y` resets).
    Reset {
        form: PassForm,
        confirm: Option<Instant>,
    },
    Rename {
        name: String,
        problem: Option<ErrorSummary>,
    },
    Disable {
        options: DisableOptions,
        /// 0: forget the key, 1: remove my file.
        focus: usize,
    },
    /// "Skip on this device" for an issue's record (armed, `y`).
    Skip {
        key: String,
        label: String,
        opened_at: Instant,
    },
}

/// What the app should do after a key in the panel.
#[derive(Debug)]
pub(crate) enum SyncOutcome {
    None,
    Close,
    OpenWizard,
    SyncNow,
    Unlock(SecretString),
    Change {
        old: SecretString,
        new: NewPassphrase,
    },
    Reset(NewPassphrase),
    Rename(String),
    SetRemember(bool),
    Disable(DisableOptions),
    Acknowledge,
    Resolve {
        id: String,
        resolution: SyncResolution,
        /// The sides shown ([`SyncConflict::sides_hash`]).
        sides_hash: String,
    },
    Skip(String),
    ConfirmHost {
        alias: String,
        hash: String,
    },
    RejectHost {
        alias: String,
        hash: String,
    },
    ConfirmSnippet {
        id: String,
        hash: String,
        name: String,
    },
    /// A generated passphrase for the open form.
    Generate,
    /// Copy the open form's generated passphrase.
    Copy,
    /// The help overlay, over the panel.
    Help,
}

#[derive(Debug)]
pub(crate) struct SyncPanel {
    pub(crate) tab: SyncTab,
    pub(crate) mode: PanelMode,
    pub(crate) devices: Load<Vec<SyncDevice>>,
    pub(crate) conflicts: Load<Vec<SyncConflict>>,
    pub(crate) issues: Load<Vec<SyncIssue>>,
    /// Selected conflict, and its selected choice.
    pub(crate) conflict: usize,
    pub(crate) choice: usize,
    pub(crate) issue: usize,
    /// Selected received item: hosts first, then snippets.
    pub(crate) received: usize,
    /// The hash of the received item whose last line has been on screen
    /// (set by the view): only that item can be decided (A1).
    pub(crate) seen_end: Option<String>,
    /// First line shown of the long text of the tab (snippet command,
    /// host details).
    pub(crate) scroll: u16,
    /// The last line that can be first (set by the view).
    pub(crate) max_scroll: u16,
    pub(crate) notice: Option<Notice>,
    /// An operation is in flight: its words (`Syncing…`).
    pub(crate) busy: Option<&'static str>,
    /// The selection last changed (decisions are armed after it).
    pub(crate) selected_at: Instant,
    /// Opened from a snippet run attempt: back to the snippets after.
    pub(crate) from_snippets: bool,
    /// This device's name (for the rename form).
    pub(crate) device_name: String,
}

impl SyncPanel {
    pub(crate) fn new(tab: SyncTab, device_name: String, now: Instant) -> Self {
        Self {
            tab,
            mode: PanelMode::Browse,
            devices: Load::Loading,
            conflicts: Load::Loading,
            issues: Load::Loading,
            conflict: 0,
            choice: 0,
            issue: 0,
            received: 0,
            seen_end: None,
            scroll: 0,
            max_scroll: 0,
            notice: None,
            busy: None,
            // Keys typed just before the panel opened decide nothing.
            selected_at: now,
            from_snippets: false,
            device_name,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.busy.is_some()
            || match &self.mode {
                PanelMode::Unlock(f) | PanelMode::Change(f) | PanelMode::Reset { form: f, .. } => {
                    f.busy
                }
                _ => false,
            }
    }

    /// The open passphrase form, if any.
    pub(crate) fn form_mut(&mut self) -> Option<&mut PassForm> {
        match &mut self.mode {
            PanelMode::Unlock(f) | PanelMode::Change(f) | PanelMode::Reset { form: f, .. } => {
                Some(f)
            }
            _ => None,
        }
    }

    pub(crate) fn form(&self) -> Option<&PassForm> {
        match &self.mode {
            PanelMode::Unlock(f) | PanelMode::Change(f) | PanelMode::Reset { form: f, .. } => {
                Some(f)
            }
            _ => None,
        }
    }

    /// The selected conflict (clamped).
    pub(crate) fn selected_conflict(&self) -> Option<&SyncConflict> {
        let list = self.conflicts.ready()?;
        list.get(self.conflict.min(list.len().saturating_sub(1)))
    }

    pub(crate) fn selected_issue(&self) -> Option<&SyncIssue> {
        let list = self.issues.ready()?;
        list.get(self.issue.min(list.len().saturating_sub(1)))
    }

    /// The selected received item (clamped): hosts, then snippets.
    pub(crate) fn selected_received<'a>(&self, data: &'a SyncData) -> Option<ReceivedItem<'a>> {
        let total = data.to_review();
        let index = self.received.min(total.saturating_sub(1));
        match data.pending.get(index) {
            Some(host) => Some(ReceivedItem::Host(host)),
            None => data
                .snippets
                .get(index.saturating_sub(data.pending.len()))
                .map(ReceivedItem::Snippet),
        }
    }

    /// Selects snippet `id` (the run attempt that opened the panel).
    pub(crate) fn select_snippet(&mut self, data: &SyncData, id: &str) {
        if let Some(i) = data.snippets.iter().position(|s| s.id == id) {
            self.received = data.pending.len().saturating_add(i);
        }
    }

    /// The selected received item has been read to its last line.
    pub(crate) fn read_to_end(&self, data: &SyncData) -> bool {
        self.selected_received(data)
            .is_some_and(|item| self.seen_end.as_deref() == Some(item.hash()))
    }

    /// Decisions on received hosts and snippets: the item has been on
    /// screen long enough and no reload is pending.
    pub(crate) fn armed(&self, data: &SyncData, now: Instant) -> bool {
        let since = |at: Instant| now.saturating_duration_since(at) >= ARM_DELAY;
        !data.stale() && since(self.selected_at) && data.changed_at.is_none_or(since)
    }

    /// New conflicts: the selection stays in range, Union / Keep both
    /// preselected again for another conflict. When the conflict on screen
    /// changed (another one, or new sides), `↵` waits [`ARM_DELAY`] (A5).
    pub(crate) fn set_conflicts(
        &mut self,
        result: Result<Vec<SyncConflict>, ErrorSummary>,
        now: Instant,
    ) {
        let shown = |panel: &Self| {
            panel
                .selected_conflict()
                .map(|c| (c.id.clone(), c.sides_hash.clone()))
        };
        let before = shown(self);
        self.conflicts = match result {
            Ok(list) => Load::Ready(list),
            Err(e) => Load::Failed(e),
        };
        if let Some(list) = self.conflicts.ready() {
            self.conflict = self.conflict.min(list.len().saturating_sub(1));
        }
        let after = shown(self);
        if before.as_ref().map(|b| &b.0) != after.as_ref().map(|a| &a.0) {
            self.choice = 0;
        }
        if before.is_some() && before != after {
            self.selected_at = now;
        }
    }

    /// `↵` on the Conflicts tab: the conflict has been on screen long
    /// enough.
    pub(crate) fn conflict_armed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.selected_at) >= ARM_DELAY
    }

    pub(crate) fn set_issues(&mut self, result: Result<Vec<SyncIssue>, ErrorSummary>) {
        self.issues = match result {
            Ok(list) => Load::Ready(list),
            Err(e) => Load::Failed(e),
        };
        if let Some(list) = self.issues.ready() {
            self.issue = self.issue.min(list.len().saturating_sub(1));
        }
    }

    fn reselect(&mut self, now: Instant) {
        self.selected_at = now;
        self.scroll = 0;
    }

    fn switch_tab(&mut self, delta: isize, now: Instant) {
        let current = TABS.iter().position(|t| *t == self.tab).unwrap_or(0);
        let n = TABS.len();
        let next = (current + n).saturating_add_signed(delta) % n;
        if let Some(tab) = TABS.get(next) {
            self.tab = *tab;
        }
        self.notice = None;
        self.reselect(now);
    }

    /// Moves the selection of the current tab.
    fn step(&mut self, data: &SyncData, delta: isize, now: Instant) {
        let (index, len) = match self.tab {
            SyncTab::Overview | SyncTab::Devices => return,
            SyncTab::Conflicts => {
                let len = self.selected_conflict().map_or(0, |c| choices(c).len());
                (&mut self.choice, len)
            }
            SyncTab::Issues => (&mut self.issue, self.issues.ready().map_or(0, Vec::len)),
            SyncTab::Received => (&mut self.received, data.to_review()),
        };
        if len == 0 {
            return;
        }
        let next = index
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        if next != *index {
            *index = next;
            if self.tab != SyncTab::Conflicts {
                self.reselect(now);
            }
        }
    }

    fn step_conflict(&mut self, delta: isize, now: Instant) {
        let len = self.conflicts.ready().map_or(0, Vec::len);
        if len == 0 {
            return;
        }
        let next = self
            .conflict
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        if next != self.conflict {
            self.conflict = next;
            self.choice = 0;
            self.reselect(now);
        }
    }

    /// Opens the passphrase form `mode` (unlock, change, reset).
    pub(crate) fn open_form(&mut self, kind: PassKind) {
        let form = PassForm::new(kind, String::new());
        self.notice = None;
        self.mode = match kind {
            PassKind::Unlock => PanelMode::Unlock(form),
            PassKind::Change => PanelMode::Change(form),
            _ => PanelMode::Reset {
                form,
                confirm: None,
            },
        };
    }

    #[allow(clippy::too_many_lines)] // one arm per mode and tab key
    pub(crate) fn handle_key(
        &mut self,
        key: KeyEvent,
        data: &SyncData,
        now: Instant,
    ) -> SyncOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match &mut self.mode {
            PanelMode::Browse => {}
            PanelMode::Unlock(form) => {
                return match form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    PassOutcome::Submit => {
                        form.busy = true;
                        SyncOutcome::Unlock(form.take_old())
                    }
                    _ => SyncOutcome::None,
                };
            }
            PanelMode::Change(form) => {
                return match form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    PassOutcome::Submit => {
                        form.busy = true;
                        let old = form.take_old();
                        SyncOutcome::Change {
                            old,
                            new: form.take_new(),
                        }
                    }
                    PassOutcome::Generate => SyncOutcome::Generate,
                    PassOutcome::Copy => SyncOutcome::Copy,
                    PassOutcome::None => SyncOutcome::None,
                };
            }
            PanelMode::Reset { form, confirm } => {
                if let Some(opened) = *confirm {
                    if now.saturating_duration_since(opened) < ARM_DELAY {
                        return SyncOutcome::None;
                    }
                    return match key.code {
                        KeyCode::Char('y') => {
                            *confirm = None;
                            form.busy = true;
                            SyncOutcome::Reset(form.take_new())
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            *confirm = None;
                            SyncOutcome::None
                        }
                        _ => SyncOutcome::None,
                    };
                }
                return match form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    PassOutcome::Submit => {
                        *confirm = Some(now);
                        SyncOutcome::None
                    }
                    PassOutcome::Generate => SyncOutcome::Generate,
                    PassOutcome::Copy => SyncOutcome::Copy,
                    PassOutcome::None => SyncOutcome::None,
                };
            }
            PanelMode::Rename { name, problem } => {
                match key.code {
                    KeyCode::Esc => self.mode = PanelMode::Browse,
                    KeyCode::Char('c') if ctrl => self.mode = PanelMode::Browse,
                    KeyCode::Enter if !name.trim().is_empty() => {
                        return SyncOutcome::Rename(name.trim().to_owned());
                    }
                    KeyCode::Char('u') if ctrl => name.clear(),
                    KeyCode::Backspace => {
                        name.pop();
                        *problem = None;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        push_char(name, c, DEVICE_NAME_MAX);
                        *problem = None;
                    }
                    _ => {}
                }
                return SyncOutcome::None;
            }
            PanelMode::Disable { options, focus } => {
                match key.code {
                    KeyCode::Esc => self.mode = PanelMode::Browse,
                    KeyCode::Char('c') if ctrl => self.mode = PanelMode::Browse,
                    KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => *focus = (*focus + 1) % 2,
                    KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                        *focus = (*focus + 1) % 2;
                    }
                    KeyCode::Char(' ') => {
                        if *focus == 0 {
                            options.forget_key = !options.forget_key;
                        } else {
                            options.remove_my_file = !options.remove_my_file;
                        }
                    }
                    KeyCode::Enter => return SyncOutcome::Disable(*options),
                    _ => {}
                }
                return SyncOutcome::None;
            }
            PanelMode::Skip {
                key: record,
                opened_at,
                ..
            } => {
                if now.saturating_duration_since(*opened_at) < ARM_DELAY {
                    return SyncOutcome::None;
                }
                let record = record.clone();
                return match key.code {
                    KeyCode::Char('y') => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::Skip(record)
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    _ => SyncOutcome::None,
                };
            }
        }
        if self.busy.is_some() && !matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            return SyncOutcome::None;
        }
        // Browse: tabs, then the tab's own keys.
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return SyncOutcome::Close,
            KeyCode::Char('?') => return SyncOutcome::Help,
            KeyCode::Char('c') if ctrl => return SyncOutcome::Close,
            KeyCode::Tab | KeyCode::Right => {
                self.switch_tab(1, now);
                return SyncOutcome::None;
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.switch_tab(-1, now);
                return SyncOutcome::None;
            }
            KeyCode::Char(c @ '1'..='5') => {
                let index = usize::from(c as u8 - b'1');
                if let Some(tab) = TABS.get(index).copied().filter(|t| *t != self.tab) {
                    self.tab = tab;
                    self.notice = None;
                    self.reselect(now);
                }
                return SyncOutcome::None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.step(data, 1, now);
                return SyncOutcome::None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.step(data, -1, now);
                return SyncOutcome::None;
            }
            KeyCode::PageDown | KeyCode::Char('J') => {
                self.scroll = self.scroll.saturating_add(4).min(self.max_scroll);
                return SyncOutcome::None;
            }
            KeyCode::PageUp | KeyCode::Char('K') => {
                self.scroll = self.scroll.saturating_sub(4);
                return SyncOutcome::None;
            }
            _ => {}
        }
        self.notice = None;
        let status = data.status().cloned();
        match self.tab {
            SyncTab::Overview => self.overview_key(key, status.as_ref(), data),
            SyncTab::Conflicts => match key.code {
                KeyCode::Char(']' | 'n') => {
                    self.step_conflict(1, now);
                    SyncOutcome::None
                }
                KeyCode::Char('[' | 'p') => {
                    self.step_conflict(-1, now);
                    SyncOutcome::None
                }
                KeyCode::Enter => {
                    if !self.conflict_armed(now) {
                        return SyncOutcome::None;
                    }
                    let Some(conflict) = self.selected_conflict() else {
                        return SyncOutcome::None;
                    };
                    let list = choices(conflict);
                    let Some(choice) = list.get(self.choice.min(list.len().saturating_sub(1)))
                    else {
                        return SyncOutcome::None;
                    };
                    SyncOutcome::Resolve {
                        id: conflict.id.clone(),
                        resolution: choice.resolution(),
                        sides_hash: conflict.sides_hash.clone(),
                    }
                }
                _ => SyncOutcome::None,
            },
            SyncTab::Issues => match key.code {
                KeyCode::Char('x') => {
                    if let Some(issue) = self.selected_issue().filter(|i| i.skippable) {
                        let label = issue
                            .subject
                            .as_ref()
                            .map_or_else(|| "this record".to_owned(), subject_label);
                        self.mode = PanelMode::Skip {
                            key: issue.key.clone(),
                            label,
                            opened_at: now,
                        };
                    }
                    SyncOutcome::None
                }
                _ => SyncOutcome::None,
            },
            SyncTab::Received => {
                let Some(item) = self.selected_received(data) else {
                    return SyncOutcome::None;
                };
                let confirm = match key.code {
                    KeyCode::Char('y') => true,
                    KeyCode::Char('n') if matches!(item, ReceivedItem::Host(_)) => false,
                    _ => return SyncOutcome::None,
                };
                if !self.armed(data, now) || !self.read_to_end(data) {
                    return SyncOutcome::None;
                }
                match item {
                    ReceivedItem::Host(host) => {
                        let (alias, hash) = (host.alias.clone(), host.destination_hash.clone());
                        if confirm {
                            SyncOutcome::ConfirmHost { alias, hash }
                        } else {
                            SyncOutcome::RejectHost { alias, hash }
                        }
                    }
                    ReceivedItem::Snippet(snippet) => SyncOutcome::ConfirmSnippet {
                        id: snippet.id.clone(),
                        hash: snippet.hash.clone(),
                        name: snippet.snippet.name.clone(),
                    },
                }
            }
            SyncTab::Devices => SyncOutcome::None,
        }
    }

    fn overview_key(
        &mut self,
        key: KeyEvent,
        status: Option<&SyncStatus>,
        data: &SyncData,
    ) -> SyncOutcome {
        match status {
            None => SyncOutcome::None,
            Some(SyncStatus::Disabled) => match key.code {
                KeyCode::Enter => SyncOutcome::OpenWizard,
                _ => SyncOutcome::None,
            },
            Some(SyncStatus::Locked) => match key.code {
                KeyCode::Enter | KeyCode::Char('u') => {
                    self.open_form(PassKind::Unlock);
                    SyncOutcome::None
                }
                KeyCode::Char('D') => {
                    self.mode = PanelMode::Disable {
                        options: DisableOptions::default(),
                        focus: 0,
                    };
                    SyncOutcome::None
                }
                _ => SyncOutcome::None,
            },
            Some(status) => match key.code {
                KeyCode::Char('s') => SyncOutcome::SyncNow,
                KeyCode::Char('c') => {
                    self.open_form(PassKind::Change);
                    SyncOutcome::None
                }
                KeyCode::Char('R') => {
                    self.open_form(PassKind::Reset);
                    SyncOutcome::None
                }
                KeyCode::Char('r') => {
                    self.mode = PanelMode::Rename {
                        name: self.device_name.clone(),
                        problem: None,
                    };
                    SyncOutcome::None
                }
                KeyCode::Char('m') => SyncOutcome::SetRemember(
                    !data.overview.as_ref().is_some_and(|o| o.remember_key),
                ),
                KeyCode::Char('D') => {
                    self.mode = PanelMode::Disable {
                        options: DisableOptions::default(),
                        focus: 0,
                    };
                    SyncOutcome::None
                }
                KeyCode::Char('a')
                    if matches!(
                        status,
                        SyncStatus::Alert {
                            kind: AlertKind::PassphraseChanged,
                            ..
                        }
                    ) =>
                {
                    SyncOutcome::Acknowledge
                }
                _ => SyncOutcome::None,
            },
        }
    }

    /// Key hints for the status bar, most important first.
    pub(crate) fn hints(&self, data: &SyncData, now: Instant) -> Vec<(&'static str, &'static str)> {
        match &self.mode {
            PanelMode::Unlock(_) => return vec![("↵", "unlock"), ("Esc", "back")],
            PanelMode::Change(f)
            | PanelMode::Reset {
                form: f,
                confirm: None,
            } => {
                let mut hints = vec![("↵", "save"), ("Tab", "next field")];
                if f.generated.is_some() && f.focused() == Some(PassField::New) {
                    hints.push(("c", "copy"));
                    hints.push(("g", "another"));
                } else {
                    hints.push(("Ctrl-G", "generate"));
                }
                hints.push(("Esc", "back"));
                return hints;
            }
            PanelMode::Reset { .. } | PanelMode::Skip { .. } => {
                return vec![("y", "confirm"), ("n / Esc", "keep")];
            }
            PanelMode::Rename { .. } => return vec![("↵", "rename"), ("Esc", "back")],
            PanelMode::Disable { .. } => {
                return vec![("↵", "turn sync off"), ("Space", "toggle"), ("Esc", "back")];
            }
            PanelMode::Browse => {}
        }
        let mut hints: Vec<(&'static str, &'static str)> = Vec::new();
        match self.tab {
            SyncTab::Overview => match data.status() {
                Some(SyncStatus::Disabled) => hints.push(("↵", "set up sync")),
                Some(SyncStatus::Locked) => {
                    hints.push(("↵", "unlock"));
                    hints.push(("D", "turn sync off"));
                }
                Some(status) => {
                    if matches!(
                        status,
                        SyncStatus::Alert {
                            kind: AlertKind::PassphraseChanged,
                            ..
                        }
                    ) {
                        hints.push(("a", "acknowledge"));
                    }
                    hints.extend([
                        ("s", "sync now"),
                        ("c", "change passphrase"),
                        ("r", "rename"),
                        ("m", "remember key"),
                        ("D", "turn sync off"),
                        ("R", "reset passphrase"),
                    ]);
                }
                None => {}
            },
            SyncTab::Conflicts if self.selected_conflict().is_some() => {
                if self.conflict_armed(now) {
                    hints.push(("↵", "resolve"));
                }
                hints.push(("j k", "choose"));
                hints.push(("[ ]", "conflict"));
            }
            SyncTab::Issues if self.selected_issue().is_some_and(|i| i.skippable) => {
                hints.push(("x", "skip on this device"));
                hints.push(("j k", "move"));
            }
            SyncTab::Received => {
                if let Some(item) = self.selected_received(data) {
                    if !self.read_to_end(data) {
                        hints.push(("PgDn", "read to the end"));
                    } else if self.armed(data, now) {
                        hints.push(("y", "confirm"));
                        if matches!(item, ReceivedItem::Host(_)) {
                            hints.push(("n", "reject"));
                        }
                    }
                    hints.push(("j k", "move"));
                    if self.read_to_end(data) {
                        hints.push(("PgDn", "scroll"));
                    }
                }
            }
            _ => {}
        }
        hints.push(("Tab", "next tab"));
        hints.push(("Esc", "close"));
        hints
    }
}

#[cfg(test)]
mod tests;
