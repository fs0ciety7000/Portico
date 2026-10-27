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
use portico_core::sync::api::{NewerEpochFile, RevokedValueChoice, RevokedValueItem, SyncKeyEntry};
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
use crate::sync_rotate::{RotateFlow, RotateKind, RotateOutcome, RotateRequest};

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
    /// Local values written by a forgotten device (or of unknown origin
    /// after a re-join), held out of sync until a decision (S2a, R3, RR4).
    pub(crate) held: Vec<RevokedValueItem>,
    /// `SyncPendingChanged` came and the reload of the received hosts
    /// (`stale_hosts`) or snippets (`stale_snippets`) has not: what is on
    /// screen may be old, decisions wait.
    pub(crate) stale_hosts: bool,
    pub(crate) stale_snippets: bool,
    pub(crate) stale_held: bool,
    /// Last time the pending hosts or snippets changed (decisions are armed
    /// [`ARM_DELAY`] after it).
    pub(crate) changed_at: Option<Instant>,
}

impl SyncData {
    /// A reload is in flight: decisions wait for it.
    pub(crate) fn stale(&self) -> bool {
        self.stale_hosts || self.stale_snippets || self.stale_held
    }

    pub(crate) fn status(&self) -> Option<&SyncStatus> {
        self.overview.as_ref().map(|o| &o.status)
    }

    /// The pending change of `alias`, if any.
    pub(crate) fn pending_for(&self, alias: &str) -> Option<&PendingHostChange> {
        self.pending.iter().find(|p| p.alias == alias)
    }

    /// Received hosts and snippets, and held values, waiting for a
    /// decision here.
    pub(crate) fn to_review(&self) -> usize {
        self.pending
            .len()
            .saturating_add(self.snippets.len())
            .saturating_add(self.held.len())
    }

    /// New pending hosts / snippets: arms decisions again when the content
    /// changed.
    pub(crate) fn set_pending(&mut self, pending: Vec<PendingHostChange>, now: Instant) {
        if pending != self.pending {
            self.changed_at = Some(now);
        }
        self.pending = pending;
    }

    pub(crate) fn set_held(&mut self, held: Vec<RevokedValueItem>, now: Instant) {
        if held != self.held {
            self.changed_at = Some(now);
        }
        self.held = held;
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
                (None, WaitReason::PartialSync) => "Waiting for files to arrive".into(),
                (None, WaitReason::Placeholder) => "Waiting for downloads".into(),
                (None, WaitReason::Deferred) => "Waiting for other devices".into(),
                (None, WaitReason::Busy) => "Waiting for the other Portico".into(),
                (_, WaitReason::RotationPending) => "Finishing key rotation…".into(),
                (Some(device), WaitReason::NeedsNewKeys) => format!("New keys needed on {device}"),
                (None, WaitReason::NeedsNewKeys) => "New keys needed".into(),
                (Some(device), _) => format!("Waiting for {device}"),
                (None, _) => "Waiting".into(),
            };
            (label, Tone::Busy)
        }
        SyncStatus::FolderUnavailable { .. } => ("Sync folder unavailable".into(), Tone::Warning),
        SyncStatus::Alert { kind, .. } => (
            match kind {
                AlertKind::KeysRotatedElsewhere => "Keys changed — enter new passphrase",
                AlertKind::RotationConflict => "Key conflict",
                AlertKind::DeviceForgotten => "Removed from sync",
                AlertKind::RevokedWrites => "Changes from a removed device",
                _ => "Sync alert",
            }
            .into(),
            Tone::Error,
        ),
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
            "Nothing is applied. If nobody changed the keys, restore the key file from this device; otherwise disable sync and create a new sync folder.",
        ),
        AlertKind::KeysRotatedElsewhere => (
            match device {
                Some(device) => format!("The file of {device} announces new keys."),
                None => "The keys were changed on another device.".into(),
            },
            "Enter the NEW passphrase chosen there. If you did not change the keys on another device, type nothing.",
        ),
        AlertKind::RotationConflict => (
            "Two devices changed the keys at the same time.".into(),
            "Nothing is merged until you choose: use the other device's keys (its passphrase), or keep this device's keys.",
        ),
        AlertKind::DeviceForgotten => (
            match device {
                Some(device) => format!("This device was removed from sync by {device}."),
                None => "This device was removed from sync.".into(),
            },
            "Its local data stays. Turn sync off, or join again as a new device.",
        ),
        AlertKind::RevokedWrites => (
            format!("{who} relays changes of a removed device."),
            "They are not applied. Type the new passphrase on that device. Values held here wait in Received.",
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
    /// The previous passphrase, optional (adoption on a device that does
    /// not remember its key, RR4).
    Previous,
    /// "I no longer know it" (rotation without the current passphrase,
    /// behind the reset-level confirmation, Q6).
    DontKnow,
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
    /// Rotation, first part: the current passphrase or "I no longer know
    /// it" (S2c-1).
    Authorize,
    /// Rotation, second part: the new passphrase, remember, "I saved it".
    Rotate,
    /// Adoption of another device's rotation: the new passphrase, after
    /// the optional previous one on a device that does not remember its
    /// key (RR4).
    Adopt {
        previous: bool,
    },
    /// Rotation conflict, "Use the other device's keys": its passphrase.
    UseOther,
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
            Self::Unlock | Self::Adopt { previous: false } | Self::UseOther => &[PassField::Old],
            Self::Change => &[PassField::Old, PassField::New, PassField::Confirm],
            Self::Reset => &[PassField::New, PassField::Confirm],
            Self::Authorize => &[PassField::Old, PassField::DontKnow],
            Self::Rotate if generated => &[PassField::New, PassField::Remember, PassField::Saved],
            Self::Rotate => &[
                PassField::New,
                PassField::Confirm,
                PassField::Remember,
                PassField::Saved,
            ],
            Self::Adopt { previous: true } => &[PassField::Previous, PassField::Old],
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
// Independent boxes of one form (saved, remember, I no longer know it,
// busy), not states of a machine.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct PassForm {
    pub(crate) kind: PassKind,
    pub(crate) focus: usize,
    pub(crate) old: zeroize::Zeroizing<String>,
    pub(crate) new: zeroize::Zeroizing<String>,
    pub(crate) confirm: zeroize::Zeroizing<String>,
    /// The previous passphrase (adoption, RR4): optional.
    pub(crate) previous: zeroize::Zeroizing<String>,
    /// "I no longer know it" (rotation authorization, Q6).
    pub(crate) dont_know: bool,
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
            .field("dont_know", &self.dont_know)
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
            previous: secret_buffer(),
            dont_know: false,
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
                PassField::Old if self.old.is_empty() => match self.kind {
                    PassKind::Authorize if self.dont_know => None,
                    PassKind::Authorize => {
                        Some("Type the current passphrase, or check “I no longer know it”")
                    }
                    PassKind::Change => Some("Type the current passphrase"),
                    PassKind::Adopt { .. } => Some("Type the new passphrase"),
                    PassKind::UseOther => Some("Type the other device's passphrase"),
                    _ => Some("Type the passphrase"),
                },
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
            "sync_passphrase_weak" | "sync_passphrase_reused"
                if self.fields().contains(&PassField::New) =>
            {
                PassField::New
            }
            "sync_passphrase_weak" => PassField::New,
            "sync_device_name_invalid" => PassField::Name,
            _ if self.fields().contains(&PassField::Old) => PassField::Old,
            _ => PassField::New,
        };
        self.refused_at(field, error);
    }

    /// The core refused the request because of `field`.
    pub(crate) fn refused_at(&mut self, field: PassField, error: ErrorSummary) {
        self.busy = false;
        self.problem = Some((field, error.message, error.hint));
        if let Some(i) = self.fields().iter().position(|f| *f == field) {
            self.focus = i;
        }
    }

    fn text_field(&mut self) -> Option<(&mut String, bool)> {
        match self.focused()? {
            PassField::Old => Some((&mut self.old, true)),
            PassField::Previous => Some((&mut self.previous, true)),
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
            KeyCode::Char(' ') if focused == Some(PassField::DontKnow) => {
                self.dont_know = !self.dont_know;
                if self.dont_know {
                    self.old.zeroize();
                }
                self.problem = None;
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
        // Typing the current passphrase means it is known after all.
        if field == Some(PassField::Old) && !self.old.is_empty() {
            self.dont_know = false;
        }
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

    /// The previous passphrase if one was typed (RR4), wiped from the form.
    pub(crate) fn take_previous(&mut self) -> Option<SecretString> {
        let secret =
            (!self.previous.is_empty()).then(|| SecretString::from(self.previous.as_str()));
        self.previous.zeroize();
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
    /// A value from a forgotten device (or of unknown origin), held.
    Held(&'a RevokedValueItem),
}

impl ReceivedItem<'_> {
    /// What a decision names: the destination or command hash, the held
    /// value's key.
    pub(crate) fn hash(&self) -> &str {
        match self {
            Self::Host(h) => &h.destination_hash,
            Self::Snippet(s) => &s.hash,
            Self::Held(h) => &h.key,
        }
    }
}

/// A question that only `y` answers, armed [`ARM_DELAY`] after it opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ask {
    /// "Restore key file": rewrite `bundle.toml` from this device's
    /// verified copy.
    Restore,
    /// Remove a device file that announces newer keys (RR5).
    Discard {
        device: String,
        label: String,
        file: String,
        fingerprint: String,
    },
    /// Remove an old sync key from the keychain.
    RemoveKey { account: String, label: String },
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
        /// 0: forget the key, 1: remove my file, 2: leave the device list.
        focus: usize,
        /// "Join again as a new device" (this device was forgotten): the
        /// wizard opens on the same folder once sync is off.
        join_again: bool,
    },
    /// "Skip on this device" for an issue's record (armed, `y`).
    Skip {
        key: String,
        label: String,
        opened_at: Instant,
    },
    /// Rotate keys, forget a device, keep this device's keys (S2c-1).
    Rotate(Box<RotateFlow>),
    /// "Enter the new passphrase" (adoption of another device's rotation).
    Adopt(PassForm),
    /// Rotation conflict: "Use the other device's keys".
    UseOther(PassForm),
    /// A question answered by `y` only (armed).
    Ask {
        ask: Ask,
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
    /// Turn sync off, then join the same folder again as a new device.
    DisableAndJoin(DisableOptions),
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
    /// Load the previews of a rotation dialog (and of a forget).
    LoadRotation {
        forget: Option<String>,
    },
    Rotate(RotateRequest),
    /// Adoption: the previous passphrase first when typed (RR4), then the
    /// new one.
    Adopt {
        previous: Option<SecretString>,
        new: SecretString,
    },
    UseOther(SecretString),
    Restore,
    DiscardNewer {
        device: String,
        fingerprint: String,
        file: String,
    },
    RemoveKey {
        account: String,
        label: String,
    },
    Held {
        key: String,
        label: String,
        choice: RevokedValueChoice,
    },
}

/// What the Overview offers, by status (keys and hints from one list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverviewAction {
    SetUp,
    Unlock,
    Adopt,
    UseOther,
    KeepMine,
    JoinAgain,
    Restore,
    Discard,
    Acknowledge,
    SyncNow,
    Rotate,
    Change,
    Rename,
    Remember,
    TurnOff,
    Reset,
}

impl OverviewAction {
    /// The key and the hint's words.
    pub(crate) fn hint(self) -> (&'static str, &'static str) {
        match self {
            Self::SetUp => ("↵", "set up sync"),
            Self::Unlock => ("↵", "unlock"),
            Self::Adopt => ("↵", "enter the new passphrase"),
            Self::UseOther => ("u", "use the other device's keys"),
            Self::KeepMine => ("o", "keep this device's keys"),
            Self::JoinAgain => ("n", "join again as a new device"),
            Self::Restore => ("b", "restore key file"),
            Self::Discard => ("x", "remove that file"),
            Self::Acknowledge => ("a", "acknowledge"),
            Self::SyncNow => ("s", "sync now"),
            Self::Rotate => ("o", "rotate keys"),
            Self::Change => ("c", "change passphrase"),
            Self::Rename => ("r", "rename"),
            Self::Remember => ("m", "remember key"),
            Self::TurnOff => ("D", "turn sync off"),
            Self::Reset => ("R", "reset passphrase"),
        }
    }

    fn matches(self, code: KeyCode) -> bool {
        match (self, code) {
            (Self::SetUp | Self::Adopt | Self::Unlock, KeyCode::Enter)
            | (Self::Unlock | Self::Adopt, KeyCode::Char('u')) => true,
            (_, KeyCode::Char(c)) => self.hint().0.starts_with(c) && self.hint().0.len() == 1,
            _ => false,
        }
    }
}

/// The Overview's actions for `status`, most important first. `newer`:
/// device files announce newer keys (RR5).
pub(crate) fn overview_actions(status: &SyncStatus, newer: bool) -> Vec<OverviewAction> {
    use OverviewAction as A;
    match status {
        SyncStatus::Disabled => vec![A::SetUp],
        SyncStatus::Locked => vec![A::Unlock, A::TurnOff],
        SyncStatus::Alert { kind, .. } => match kind {
            AlertKind::KeysRotatedElsewhere => {
                let mut actions = vec![A::Adopt];
                if newer {
                    actions.push(A::Discard);
                }
                actions.extend([A::SyncNow, A::Remember, A::TurnOff]);
                actions
            }
            AlertKind::RotationConflict => vec![A::UseOther, A::KeepMine, A::SyncNow, A::TurnOff],
            AlertKind::DeviceForgotten => vec![A::TurnOff, A::JoinAgain],
            AlertKind::BundleTampered => vec![A::Restore, A::SyncNow, A::TurnOff],
            AlertKind::PassphraseChanged => {
                let mut actions = vec![A::Acknowledge];
                actions.extend(everyday());
                actions
            }
            _ => everyday(),
        },
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            ..
        } => vec![A::SyncNow, A::Restore, A::Remember, A::TurnOff],
        _ => everyday(),
    }
}

fn everyday() -> Vec<OverviewAction> {
    use OverviewAction as A;
    vec![
        A::SyncNow,
        A::Rotate,
        A::Change,
        A::Rename,
        A::Remember,
        A::TurnOff,
        A::Reset,
    ]
}

/// A row of the Devices tab: a device, then an old key of the keychain.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DeviceRow<'a> {
    Device(&'a SyncDevice),
    Key(&'a SyncKeyEntry),
}

/// `epoch 3 · this sync` / `epoch 1 · another sync (9f0e…)`.
pub(crate) fn key_entry_label(entry: &SyncKeyEntry) -> String {
    if entry.this_bundle {
        format!("Keys {} · this sync", entry.epoch)
    } else {
        let short: String = entry.bundle.chars().take(8).collect();
        format!("Keys {} · another sync ({short})", entry.epoch)
    }
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
    /// Old sync keys in this keychain (Devices tab, S2c-1).
    pub(crate) keys: Load<Vec<SyncKeyEntry>>,
    /// Device files that announce newer keys (RR5, Overview).
    pub(crate) newer: Load<Vec<NewerEpochFile>>,
    /// Selected row of the Devices tab (devices, then old keys).
    pub(crate) device: usize,
    /// Selected newer file (Overview).
    pub(crate) newer_index: usize,
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
            keys: Load::Loading,
            newer: Load::Loading,
            device: 0,
            newer_index: 0,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.busy.is_some()
            || match &self.mode {
                PanelMode::Rotate(flow) => flow.busy(),
                _ => self.form().is_some_and(|f| f.busy),
            }
    }

    /// The open passphrase form, if any.
    pub(crate) fn form_mut(&mut self) -> Option<&mut PassForm> {
        match &mut self.mode {
            PanelMode::Unlock(f)
            | PanelMode::Change(f)
            | PanelMode::Adopt(f)
            | PanelMode::UseOther(f)
            | PanelMode::Reset { form: f, .. } => Some(f),
            PanelMode::Rotate(flow) => flow.form_mut(),
            _ => None,
        }
    }

    pub(crate) fn form(&self) -> Option<&PassForm> {
        match &self.mode {
            PanelMode::Unlock(f)
            | PanelMode::Change(f)
            | PanelMode::Adopt(f)
            | PanelMode::UseOther(f)
            | PanelMode::Reset { form: f, .. } => Some(f),
            PanelMode::Rotate(flow) => flow.form(),
            _ => None,
        }
    }

    /// The rows of the Devices tab: devices, then old keys.
    pub(crate) fn device_rows(&self) -> Vec<DeviceRow<'_>> {
        let mut rows: Vec<DeviceRow<'_>> = self
            .devices
            .ready()
            .map(|d| d.iter().map(DeviceRow::Device).collect())
            .unwrap_or_default();
        if let Some(keys) = self.keys.ready() {
            rows.extend(keys.iter().map(DeviceRow::Key));
        }
        rows
    }

    pub(crate) fn selected_device_row(&self) -> Option<DeviceRow<'_>> {
        let rows = self.device_rows();
        let index = self.device.min(rows.len().saturating_sub(1));
        rows.get(index).copied()
    }

    pub(crate) fn newer_files(&self) -> &[NewerEpochFile] {
        self.newer.ready().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn selected_newer(&self) -> Option<&NewerEpochFile> {
        let list = self.newer_files();
        list.get(self.newer_index.min(list.len().saturating_sub(1)))
    }

    /// Opens a rotation dialog (the app loads its previews).
    pub(crate) fn open_rotation(&mut self, kind: RotateKind, remember: bool) -> SyncOutcome {
        let forget = match &kind {
            RotateKind::Forget { id, .. } => Some(id.clone()),
            _ => None,
        };
        self.notice = None;
        self.mode = PanelMode::Rotate(Box::new(RotateFlow::new(kind, remember)));
        SyncOutcome::LoadRotation { forget }
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

    /// The selected received item (clamped): hosts, snippets, then held
    /// values.
    pub(crate) fn selected_received<'a>(&self, data: &'a SyncData) -> Option<ReceivedItem<'a>> {
        let total = data.to_review();
        let index = self.received.min(total.saturating_sub(1));
        if let Some(host) = data.pending.get(index) {
            return Some(ReceivedItem::Host(host));
        }
        let index = index.saturating_sub(data.pending.len());
        if let Some(snippet) = data.snippets.get(index) {
            return Some(ReceivedItem::Snippet(snippet));
        }
        data.held
            .get(index.saturating_sub(data.snippets.len()))
            .map(ReceivedItem::Held)
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
            SyncTab::Overview => {
                let len = self.newer_files().len();
                (&mut self.newer_index, len)
            }
            SyncTab::Devices => {
                let len = self.device_rows().len();
                (&mut self.device, len)
            }
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
            if !matches!(self.tab, SyncTab::Conflicts | SyncTab::Devices) {
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
            PanelMode::Disable {
                options,
                focus,
                join_again,
            } => {
                match key.code {
                    KeyCode::Esc => self.mode = PanelMode::Browse,
                    KeyCode::Char('c') if ctrl => self.mode = PanelMode::Browse,
                    // Joining again: already off the list, nothing to leave.
                    KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                        let n = if *join_again { 2 } else { 3 };
                        *focus = (*focus + 1) % n;
                    }
                    KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                        let n = if *join_again { 2 } else { 3 };
                        *focus = (*focus + n - 1) % n;
                    }
                    KeyCode::Char(' ') => match *focus {
                        0 => options.forget_key = !options.forget_key,
                        // As in the GUI: removing this device's file also
                        // ticks "Leave the device list" (it can be unticked).
                        1 if !*join_again => {
                            options.remove_my_file = !options.remove_my_file;
                            options.leave_roster = options.remove_my_file;
                        }
                        1 => options.remove_my_file = !options.remove_my_file,
                        _ => options.leave_roster = !options.leave_roster,
                    },
                    KeyCode::Enter if *join_again => {
                        return SyncOutcome::DisableAndJoin(*options);
                    }
                    KeyCode::Enter => return SyncOutcome::Disable(*options),
                    _ => {}
                }
                return SyncOutcome::None;
            }
            PanelMode::Rotate(flow) => {
                return match flow.handle_key(key, now) {
                    RotateOutcome::None => SyncOutcome::None,
                    RotateOutcome::Close => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    RotateOutcome::Generate => SyncOutcome::Generate,
                    RotateOutcome::Copy => SyncOutcome::Copy,
                    RotateOutcome::Submit(request) => SyncOutcome::Rotate(request),
                };
            }
            PanelMode::Adopt(form) => {
                return match form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    PassOutcome::Submit => {
                        form.busy = true;
                        let previous = form.take_previous();
                        SyncOutcome::Adopt {
                            previous,
                            new: form.take_old(),
                        }
                    }
                    _ => SyncOutcome::None,
                };
            }
            PanelMode::UseOther(form) => {
                return match form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    PassOutcome::Submit => {
                        form.busy = true;
                        SyncOutcome::UseOther(form.take_old())
                    }
                    _ => SyncOutcome::None,
                };
            }
            PanelMode::Ask { ask, opened_at } => {
                if now.saturating_duration_since(*opened_at) < ARM_DELAY {
                    return SyncOutcome::None;
                }
                let ask = ask.clone();
                return match key.code {
                    KeyCode::Char('y') => {
                        self.mode = PanelMode::Browse;
                        match ask {
                            Ask::Restore => SyncOutcome::Restore,
                            Ask::Discard {
                                device,
                                file,
                                fingerprint,
                                ..
                            } => SyncOutcome::DiscardNewer {
                                device,
                                fingerprint,
                                file,
                            },
                            Ask::RemoveKey { account, label } => {
                                SyncOutcome::RemoveKey { account, label }
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.mode = PanelMode::Browse;
                        SyncOutcome::None
                    }
                    _ => SyncOutcome::None,
                };
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
            SyncTab::Overview => self.overview_key(key, status.as_ref(), data, now),
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
                if let ReceivedItem::Held(held) = item {
                    let choice = match key.code {
                        KeyCode::Char('m') => RevokedValueChoice::KeepAsMine,
                        KeyCode::Char('t') => RevokedValueChoice::TakeSynced,
                        _ => return SyncOutcome::None,
                    };
                    if !self.armed(data, now) || !self.read_to_end(data) {
                        return SyncOutcome::None;
                    }
                    return SyncOutcome::Held {
                        key: held.key.clone(),
                        label: subject_label(&held.subject),
                        choice,
                    };
                }
                let confirm = match key.code {
                    KeyCode::Char('y') => true,
                    KeyCode::Char('n') if matches!(item, ReceivedItem::Host(_)) => false,
                    _ => return SyncOutcome::None,
                };
                if !self.armed(data, now) || !self.read_to_end(data) {
                    return SyncOutcome::None;
                }
                match item {
                    ReceivedItem::Held(_) => SyncOutcome::None,
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
            SyncTab::Devices => self.devices_key(key, data, now),
        }
    }

    /// Rotation is offered while sync runs normally (not off, locked, nor
    /// waiting for a key decision).
    fn can_rotate(data: &SyncData) -> bool {
        data.status()
            .is_some_and(|status| overview_actions(status, false).contains(&OverviewAction::Rotate))
    }

    fn remember(data: &SyncData) -> bool {
        data.overview.as_ref().is_none_or(|o| o.remember_key)
    }

    fn devices_key(&mut self, key: KeyEvent, data: &SyncData, now: Instant) -> SyncOutcome {
        match key.code {
            KeyCode::Char('o') if Self::can_rotate(data) => {
                self.open_rotation(RotateKind::Rotate, Self::remember(data))
            }
            KeyCode::Char('f') => {
                let target = match self.selected_device_row() {
                    Some(DeviceRow::Device(d)) if d.this_device => Err(()),
                    Some(DeviceRow::Device(d)) if !d.revoked => Ok((d.id.clone(), d.label.clone())),
                    _ => return SyncOutcome::None,
                };
                match target {
                    Err(()) => {
                        self.notice = Some(Notice {
                            ok: false,
                            message: "this device cannot forget itself".into(),
                            hint: Some(
                                "To leave, use Turn sync off (Overview, D) with “Leave the device list”."
                                    .into(),
                            ),
                        });
                        SyncOutcome::None
                    }
                    Ok(_) if !Self::can_rotate(data) => {
                        self.notice = Some(Notice {
                            ok: false,
                            message: "a device can be forgotten only while sync runs here".into(),
                            hint: Some("Settle what the Overview shows first.".into()),
                        });
                        SyncOutcome::None
                    }
                    Ok((id, label)) => {
                        self.open_rotation(RotateKind::Forget { id, label }, Self::remember(data))
                    }
                }
            }
            KeyCode::Char('x') => {
                if let Some(DeviceRow::Key(entry)) = self.selected_device_row() {
                    self.mode = PanelMode::Ask {
                        ask: Ask::RemoveKey {
                            account: entry.account.clone(),
                            label: key_entry_label(entry),
                        },
                        opened_at: now,
                    };
                }
                SyncOutcome::None
            }
            _ => SyncOutcome::None,
        }
    }

    fn overview_key(
        &mut self,
        key: KeyEvent,
        status: Option<&SyncStatus>,
        data: &SyncData,
        now: Instant,
    ) -> SyncOutcome {
        let Some(status) = status else {
            return SyncOutcome::None;
        };
        let newer = !self.newer_files().is_empty();
        let Some(action) = overview_actions(status, newer)
            .into_iter()
            .find(|a| a.matches(key.code))
        else {
            return SyncOutcome::None;
        };
        let remember = Self::remember(data);
        match action {
            OverviewAction::SetUp => SyncOutcome::OpenWizard,
            OverviewAction::Unlock => {
                self.open_form(PassKind::Unlock);
                SyncOutcome::None
            }
            OverviewAction::Adopt => {
                self.notice = None;
                self.mode = PanelMode::Adopt(PassForm::new(
                    PassKind::Adopt {
                        previous: !remember,
                    },
                    String::new(),
                ));
                SyncOutcome::None
            }
            OverviewAction::UseOther => {
                self.notice = None;
                self.mode = PanelMode::UseOther(PassForm::new(PassKind::UseOther, String::new()));
                SyncOutcome::None
            }
            OverviewAction::KeepMine => self.open_rotation(RotateKind::KeepMine, remember),
            OverviewAction::Rotate => self.open_rotation(RotateKind::Rotate, remember),
            OverviewAction::JoinAgain => {
                self.mode = PanelMode::Disable {
                    options: DisableOptions::default(),
                    focus: 0,
                    join_again: true,
                };
                SyncOutcome::None
            }
            OverviewAction::TurnOff => {
                self.mode = PanelMode::Disable {
                    options: DisableOptions::default(),
                    focus: 0,
                    join_again: false,
                };
                SyncOutcome::None
            }
            OverviewAction::Restore => {
                self.mode = PanelMode::Ask {
                    ask: Ask::Restore,
                    opened_at: now,
                };
                SyncOutcome::None
            }
            OverviewAction::Discard => {
                if let Some(file) = self.selected_newer() {
                    self.mode = PanelMode::Ask {
                        ask: Ask::Discard {
                            device: file.device.clone(),
                            label: file.label.clone(),
                            file: file.file.clone(),
                            fingerprint: file.fingerprint.clone(),
                        },
                        opened_at: now,
                    };
                }
                SyncOutcome::None
            }
            OverviewAction::Acknowledge => SyncOutcome::Acknowledge,
            OverviewAction::SyncNow => SyncOutcome::SyncNow,
            OverviewAction::Change => {
                self.open_form(PassKind::Change);
                SyncOutcome::None
            }
            OverviewAction::Reset => {
                self.open_form(PassKind::Reset);
                SyncOutcome::None
            }
            OverviewAction::Rename => {
                self.mode = PanelMode::Rename {
                    name: self.device_name.clone(),
                    problem: None,
                };
                SyncOutcome::None
            }
            OverviewAction::Remember => SyncOutcome::SetRemember(!remember),
        }
    }

    /// Key hints for the status bar, most important first.
    #[allow(clippy::too_many_lines)] // one arm per mode, then per tab
    pub(crate) fn hints(&self, data: &SyncData, now: Instant) -> Vec<(&'static str, &'static str)> {
        let armed = |at: Instant| now.saturating_duration_since(at) >= ARM_DELAY;
        match &self.mode {
            PanelMode::Unlock(_) => return vec![("↵", "unlock"), ("Esc", "back")],
            PanelMode::Adopt(_) => {
                return vec![
                    ("↵", "use the new keys"),
                    ("Tab", "next field"),
                    ("Esc", "back"),
                ];
            }
            PanelMode::UseOther(_) => return vec![("↵", "use their keys"), ("Esc", "back")],
            PanelMode::Rotate(flow) => return flow.hints(now),
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
            PanelMode::Ask { opened_at, .. } if !armed(*opened_at) => return Vec::new(),
            PanelMode::Reset { .. } | PanelMode::Skip { .. } | PanelMode::Ask { .. } => {
                return vec![("y", "confirm"), ("n / Esc", "keep")];
            }
            PanelMode::Rename { .. } => return vec![("↵", "rename"), ("Esc", "back")],
            PanelMode::Disable { join_again, .. } => {
                let enter = if *join_again {
                    "turn off, then join again"
                } else {
                    "turn sync off"
                };
                return vec![("↵", enter), ("Space", "toggle"), ("Esc", "back")];
            }
            PanelMode::Browse => {}
        }
        let mut hints: Vec<(&'static str, &'static str)> = Vec::new();
        match self.tab {
            SyncTab::Overview => {
                if let Some(status) = data.status() {
                    let newer = !self.newer_files().is_empty();
                    hints.extend(
                        overview_actions(status, newer)
                            .into_iter()
                            .map(OverviewAction::hint),
                    );
                    if self.newer_files().len() > 1 {
                        hints.push(("j k", "file"));
                    }
                }
            }
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
                        match item {
                            ReceivedItem::Held(_) => {
                                hints.push(("m", "keep as mine"));
                                hints.push(("t", "take the synced value"));
                            }
                            ReceivedItem::Host(_) => {
                                hints.push(("y", "confirm"));
                                hints.push(("n", "reject"));
                            }
                            ReceivedItem::Snippet(_) => hints.push(("y", "confirm")),
                        }
                    }
                    hints.push(("j k", "move"));
                    if self.read_to_end(data) {
                        hints.push(("PgDn", "scroll"));
                    }
                }
            }
            SyncTab::Devices => {
                match self.selected_device_row() {
                    Some(DeviceRow::Device(d)) if !d.this_device && !d.revoked => {
                        if Self::can_rotate(data) {
                            hints.push(("f", "forget…"));
                        }
                    }
                    Some(DeviceRow::Key(_)) => hints.push(("x", "remove")),
                    _ => {}
                }
                if Self::can_rotate(data) {
                    hints.push(("o", "rotate keys"));
                }
                if self.device_rows().len() > 1 {
                    hints.push(("j k", "move"));
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
