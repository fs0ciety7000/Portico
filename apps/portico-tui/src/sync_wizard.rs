//! "Set up sync": the wizard of the encrypted folder sync (phase 11, S1c,
//! `docs/SYNC.md` §6.1).
//!
//! Steps: what sync does (only encrypted files leave the machine; private
//! keys and `known_hosts` never do; no network of Portico's own) → the
//! folder (typed, `Tab` completes directories; `↵` asks the core what it
//! holds: usable, an existing sync, incomplete, missing, refused) → first
//! device (new passphrase with its meter, "Generate a passphrase", device
//! name, "Remember the key on this device") or join (passphrase, then the
//! review of what joining brings and how to settle conflicts; nothing is
//! written before the commit). The recovery warning sits **in** the step
//! that writes (Q1, S1c review): create gates "Create" on "I saved it. A
//! lost passphrase cannot be recovered unless a device remembers the key.",
//! typed or generated alike; join gates "Join" on the same box, on the
//! review. On success the wizard closes onto the Sync panel ("Sync is on").
//!
//! The join token is single use and expires (10 min, F14): when the commit
//! is refused for that, the wizard goes back to the passphrase. The
//! passphrase is wiped as soon as it is handed to the engine.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::sync::{FolderProbe, JoinBulkChoice, JoinPreview};

use crate::form::{MAX_LINE, paste_line, push_char};
use crate::sync_panel::{PassForm, PassKind, PassOutcome};

/// Bulk choices of the join review, in the order shown (the first is the
/// default: nothing settled without looking).
pub(crate) const BULK: [(JoinBulkChoice, &str, &str); 3] = [
    (
        JoinBulkChoice::ReviewEach,
        "Review each conflict",
        "Conflicts wait in the Sync panel; nothing is settled for you.",
    ),
    (
        JoinBulkChoice::PreferRemote,
        "Prefer the folder's values",
        "Conflicts take the values of the other devices.",
    ),
    (
        JoinBulkChoice::PreferLocal,
        "Prefer this device's values",
        "Conflicts keep what this device has.",
    ),
];

/// Why only "Review each conflict" is offered on a re-join (S2a, FAIBLE-5).
pub(crate) const REJOIN_NOTE: &str = "This device synced with this folder before: each difference is reviewed one by one, so nothing is settled in bulk.";

/// Under the bulk choices, always: neither confirms a received host (F1).
pub(crate) const BULK_NOTE: &str =
    "Received hosts still wait for your confirmation, one by one, whichever you choose.";

/// The recovery warning, word for word as in the GUI.
pub(crate) const RECOVERY: &str = "If you lose the passphrase and no device remembers the key, the content of the sync folder cannot be recovered: nobody can decrypt it, not even Portico. Your local hosts, snippets and settings are never deleted.";

/// The box that gates create and join, word for word as in the GUI.
pub(crate) const SAVED: &str =
    "I saved it. A lost passphrase cannot be recovered unless a device remembers the key.";

/// What a join token is good for (F14), in the review.
pub(crate) const JOIN_TTL_SECS: u64 = 600;

#[derive(Debug)]
pub(crate) enum Step {
    Intro,
    Folder,
    Create,
    Join,
    /// What joining brings, the bulk choice and "I saved it".
    Review,
    /// Waiting for the engine: its words.
    Working(&'static str),
}

/// What the app should do after a key in the wizard.
#[derive(Debug)]
pub(crate) enum WizardOutcome {
    None,
    Close,
    Complete(String),
    Probe(String),
    Generate,
    Copy,
    /// Create the sync (the form holds the passphrase).
    Create,
    /// Join preview (the form holds the passphrase).
    Preview,
    Commit {
        token: String,
        bulk: JoinBulkChoice,
    },
}

/// The box of the join review is not checked.
pub(crate) const SAVED_PROBLEM: &str = "Save the passphrase, then check this box";

#[derive(Debug)]
pub(crate) struct SyncWizard {
    pub(crate) step: Step,
    pub(crate) folder: String,
    /// The probe of `probed` (the folder as it was typed when probed).
    pub(crate) probe: Option<(String, Result<FolderProbe, ErrorSummary>)>,
    pub(crate) probing: bool,
    /// Directory candidates of the last `Tab`.
    pub(crate) candidates: Vec<String>,
    /// First device, or join: the form.
    pub(crate) form: PassForm,
    pub(crate) preview: Option<(JoinPreview, Instant)>,
    pub(crate) bulk: usize,
    /// "I saved it" on the join review.
    pub(crate) saved: bool,
    /// Join pressed without the box: say so under it.
    pub(crate) saved_problem: bool,
    /// The last refusal outside a form field.
    pub(crate) error: Option<ErrorSummary>,
    /// A line above the folder step ("Sync is off. Join the folder again
    /// as a new device.", S2c-1).
    pub(crate) note: Option<String>,
    pub(crate) device_name: String,
}

impl SyncWizard {
    pub(crate) fn new(device_name: String) -> Self {
        Self {
            step: Step::Intro,
            folder: String::new(),
            probe: None,
            probing: false,
            candidates: Vec::new(),
            form: PassForm::new(PassKind::Create, device_name.clone()),
            preview: None,
            bulk: 0,
            saved: false,
            saved_problem: false,
            error: None,
            note: None,
            device_name,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.probing || matches!(self.step, Step::Working(_)) || self.form.busy
    }

    /// Joining (the folder holds a sync) rather than creating.
    pub(crate) fn joining(&self) -> bool {
        self.form.kind == PassKind::Join
    }

    /// The probe of the folder as typed now.
    pub(crate) fn current_probe(&self) -> Option<&Result<FolderProbe, ErrorSummary>> {
        self.probe
            .as_ref()
            .filter(|(folder, _)| *folder == self.folder)
            .map(|(_, probe)| probe)
    }

    /// The folder as the engine gets it: `~` expanded.
    pub(crate) fn folder_path(&self) -> std::path::PathBuf {
        expand_home(self.folder.trim())
    }

    pub(crate) fn probed(&mut self, folder: String, result: Result<FolderProbe, ErrorSummary>) {
        self.probing = false;
        self.probe = Some((folder, result));
    }

    pub(crate) fn completed(&mut self, input: &str, candidates: &[String]) {
        if input != self.folder {
            return;
        }
        match candidates {
            [] => self.candidates.clear(),
            [one] => {
                self.folder = format!("{one}{}", std::path::MAIN_SEPARATOR);
                self.candidates.clear();
            }
            many => {
                let common = common_prefix(many);
                if common.chars().count() > self.folder.chars().count() {
                    self.folder = common;
                }
                self.candidates = many.to_vec();
            }
        }
    }

    /// The engine answered a create, preview or commit.
    pub(crate) fn failed(&mut self, error: ErrorSummary) {
        self.form.busy = false;
        match error.code.as_str() {
            // The preview expired or was used: the passphrase again.
            "sync_join_expired" => {
                self.preview = None;
                self.step = Step::Join;
                self.form = PassForm::new(PassKind::Join, self.device_name.clone());
                self.error = Some(error);
            }
            "sync_passphrase_weak" | "sync_device_name_invalid" | "sync_wrong_passphrase" => {
                self.step = if self.joining() {
                    Step::Join
                } else {
                    Step::Create
                };
                self.form.refused(error);
            }
            _ => {
                self.step = if self.preview.is_some() {
                    Step::Review
                } else if self.joining() {
                    Step::Join
                } else {
                    Step::Create
                };
                self.error = Some(error);
            }
        }
    }

    /// Straight to the folder step with `folder` filled in ("Join again
    /// as a new device").
    pub(crate) fn at_folder(&mut self, folder: &str) {
        self.folder = folder.chars().take(MAX_LINE).collect();
        self.step = Step::Folder;
    }

    /// This device verified this bundle before (S2a, `JoinPreview::rejoin`):
    /// only "Review each conflict" is accepted.
    pub(crate) fn rejoin(&self) -> bool {
        self.preview.as_ref().is_some_and(|(p, _)| p.rejoin)
    }

    pub(crate) fn previewed(&mut self, preview: JoinPreview, now: Instant) {
        self.form.busy = false;
        self.preview = Some((preview, now));
        self.bulk = 0;
        self.saved = false;
        self.saved_problem = false;
        self.error = None;
        self.step = Step::Review;
    }

    #[allow(clippy::too_many_lines)] // one arm per step
    pub(crate) fn handle_key(&mut self, key: KeyEvent, now: Instant) -> WizardOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') && !self.busy() {
            return WizardOutcome::Close;
        }
        match self.step {
            Step::Intro => match key.code {
                KeyCode::Enter => {
                    self.step = Step::Folder;
                    WizardOutcome::None
                }
                KeyCode::Esc | KeyCode::Char('q') => WizardOutcome::Close,
                _ => WizardOutcome::None,
            },
            Step::Folder => {
                if self.probing {
                    return WizardOutcome::None;
                }
                match key.code {
                    KeyCode::Esc => {
                        self.step = Step::Intro;
                        WizardOutcome::None
                    }
                    KeyCode::Tab => WizardOutcome::Complete(self.folder.clone()),
                    KeyCode::Enter => self.folder_enter(),
                    KeyCode::Backspace => {
                        self.folder.pop();
                        self.candidates.clear();
                        WizardOutcome::None
                    }
                    KeyCode::Char('u') if ctrl => {
                        self.folder.clear();
                        self.candidates.clear();
                        WizardOutcome::None
                    }
                    KeyCode::Char(c) if !ctrl => {
                        push_char(&mut self.folder, c, MAX_LINE * 4);
                        self.candidates.clear();
                        WizardOutcome::None
                    }
                    _ => WizardOutcome::None,
                }
            }
            Step::Create | Step::Join => {
                self.error = None;
                match self.form.handle_key(key) {
                    PassOutcome::Cancel => {
                        self.form = PassForm::new(self.form.kind, self.device_name.clone());
                        self.step = Step::Folder;
                        WizardOutcome::None
                    }
                    PassOutcome::Submit if self.joining() => {
                        self.form.busy = true;
                        self.device_name = self.form.name.trim().to_owned();
                        self.step = Step::Working("Opening the sync folder…");
                        WizardOutcome::Preview
                    }
                    // "I saved it" is checked (a field of the form).
                    PassOutcome::Submit => {
                        self.device_name = self.form.name.trim().to_owned();
                        self.form.busy = true;
                        self.step = Step::Working("Creating the sync folder…");
                        WizardOutcome::Create
                    }
                    PassOutcome::Generate => WizardOutcome::Generate,
                    PassOutcome::Copy => WizardOutcome::Copy,
                    PassOutcome::None => WizardOutcome::None,
                }
            }
            Step::Review => match key.code {
                KeyCode::Esc => {
                    // The token stays unused; it expires on its own.
                    self.preview = None;
                    self.form = PassForm::new(PassKind::Join, self.device_name.clone());
                    self.step = Step::Join;
                    WizardOutcome::None
                }
                // A re-join: each difference is reviewed, nothing else.
                KeyCode::Down
                | KeyCode::Up
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Char('j' | 'k')
                    if self.rejoin() =>
                {
                    WizardOutcome::None
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    self.bulk = (self.bulk + 1).min(BULK.len() - 1);
                    WizardOutcome::None
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    self.bulk = self.bulk.saturating_sub(1);
                    WizardOutcome::None
                }
                KeyCode::Char(' ') => {
                    self.saved = !self.saved;
                    self.saved_problem = false;
                    WizardOutcome::None
                }
                KeyCode::Enter if !self.saved => {
                    self.saved_problem = true;
                    WizardOutcome::None
                }
                KeyCode::Enter => {
                    if let Some((preview, at)) = &self.preview {
                        let token = preview.token.clone();
                        let expired = now.saturating_duration_since(*at).as_secs() >= JOIN_TTL_SECS;
                        if expired {
                            self.failed(ErrorSummary {
                                code: "sync_join_expired".into(),
                                message: "the review expired".into(),
                                hint: Some(
                                    "Type the passphrase again to see what joining brings.".into(),
                                ),
                            });
                            return WizardOutcome::None;
                        }
                        let bulk = if self.rejoin() {
                            JoinBulkChoice::ReviewEach
                        } else {
                            BULK.get(self.bulk)
                                .map_or(JoinBulkChoice::ReviewEach, |b| b.0)
                        };
                        self.step = Step::Working("Joining…");
                        WizardOutcome::Commit { token, bulk }
                    } else {
                        WizardOutcome::None
                    }
                }
                _ => WizardOutcome::None,
            },
            Step::Working(_) => WizardOutcome::None,
        }
    }

    fn folder_enter(&mut self) -> WizardOutcome {
        if self.folder.trim().is_empty() {
            return WizardOutcome::None;
        }
        match self.current_probe() {
            None | Some(Err(_)) => {
                self.probing = true;
                self.candidates.clear();
                WizardOutcome::Probe(self.folder.clone())
            }
            Some(Ok(FolderProbe::Usable)) => {
                self.form = PassForm::new(PassKind::Create, self.device_name.clone());
                self.preview = None;
                self.step = Step::Create;
                WizardOutcome::None
            }
            Some(Ok(FolderProbe::ExistingBundle { .. })) => {
                self.form = PassForm::new(PassKind::Join, self.device_name.clone());
                self.preview = None;
                self.step = Step::Join;
                WizardOutcome::None
            }
            // Incomplete, missing, refused: probe again (it may be ready now).
            Some(Ok(_)) => {
                self.probing = true;
                WizardOutcome::Probe(self.folder.clone())
            }
        }
    }

    pub(crate) fn paste(&mut self, text: &str) {
        match self.step {
            Step::Folder if !self.probing => {
                paste_line(&mut self.folder, text, MAX_LINE * 4);
                self.candidates.clear();
            }
            Step::Create | Step::Join => self.form.paste(text),
            _ => {}
        }
    }
}

/// `~/x` → `$HOME/x`.
pub(crate) fn expand_home(text: &str) -> std::path::PathBuf {
    let home = crate::ui::fmt::home_dir();
    match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home))
            if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') =>
        {
            std::path::PathBuf::from(format!("{home}{rest}"))
        }
        _ => std::path::PathBuf::from(text),
    }
}

/// The directories whose path starts with `input` (`~` kept as typed),
/// sorted, hidden ones only when asked for. Blocking: call it off the UI
/// thread.
pub(crate) fn complete_dirs(input: &str) -> Vec<String> {
    const MAX: usize = 32;
    let sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let (dir_text, stem) = match input.rfind(sep) {
        Some(i) => (
            input.get(..=i).unwrap_or(""),
            input.get(i + 1..).unwrap_or(""),
        ),
        None => ("", input),
    };
    let dir = if dir_text.is_empty() {
        std::path::PathBuf::from(".")
    } else {
        expand_home(dir_text)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with(stem) && (stem.starts_with('.') || !name.starts_with('.')))
        .map(|name| format!("{dir_text}{name}"))
        .collect();
    out.sort();
    out.truncate(MAX);
    out
}

fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.chars().collect();
    for item in items.iter().skip(1) {
        let same = prefix
            .iter()
            .zip(item.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(same);
    }
    prefix.into_iter().collect()
}
