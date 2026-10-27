//! The key rotation dialogs of the Sync panel (phase 11, S2c-1,
//! `docs/SYNC.md` §13.8 and §13.12 "Ce que S2c-1 (UIs) doit brancher"):
//! "Rotate keys…", "Forget <device>…" and, on a rotation conflict, "Keep
//! this device's keys".
//!
//! - **Steps**: what happens (the preview: devices that will need the new
//!   passphrase, devices on Portico < 0.7 that will be cut off (Q5); for a
//!   forget, first "Also remove <device>'s access to your cloud folder" (RR7) and the
//!   records it last wrote), then the current passphrase or "I no longer
//!   know it" (Q6), then the new passphrase (generator, meter, "I saved
//!   it…"), then a confirmation armed like reset (only `y`, as the GUI's
//!   danger button), worded at reset level without the current
//!   passphrase. A "Keep this device's keys" skips the authorization: the
//!   key this device holds authorizes it.
//! - **Secrets**: both passphrases live in the forms' `Zeroizing` buffers
//!   and are handed over once ([`PassForm::take_old`],
//!   [`PassForm::take_new`]); a refusal brings back the step that owns the
//!   field, empty.
//! - **Result**: "Keys rotated. Enter the new passphrase on: …", the
//!   forgotten devices, the server passwords to change (F2), or "Finishing
//!   the key rotation…" while the folder does not answer (forward only).

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::sync::api::{ForgetPreview, RotateAuth, RotationOutcome, RotationPreview};
use portico_core::sync::{NewPassphrase, SyncStatus, WaitReason};

use crate::dialog::ARM_DELAY;
use crate::keys::Load;
use crate::sync_panel::{PassField, PassForm, PassKind, PassOutcome};

/// Which rotation the dialog makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RotateKind {
    /// "Rotate keys…".
    Rotate,
    /// "Forget <device>…": always a rotation (Q2).
    Forget { id: String, label: String },
    /// A rotation conflict, "Keep this device's keys" (R17).
    KeepMine,
}

/// Where the dialog is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RotateStep {
    /// What happens (preview).
    About,
    /// The current passphrase, or "I no longer know it".
    Authorize,
    /// The new passphrase.
    New,
    /// The last question (armed, only `y`); reset-level without the
    /// current passphrase.
    Confirm(Instant),
    /// The engine works (Argon2, then the folder).
    Working,
    /// Done: what to do next.
    Done,
}

/// What the app should do after a key in the dialog.
#[derive(Debug)]
pub(crate) enum RotateOutcome {
    None,
    /// Closed (cancelled, or done).
    Close,
    Generate,
    Copy,
    /// Hand it to the engine.
    Submit(RotateRequest),
}

/// A rotation to run (the secrets move out of the forms).
#[derive(Debug)]
pub(crate) struct RotateRequest {
    pub(crate) kind: RotateKind,
    pub(crate) authorization: RotateAuth,
    pub(crate) new_passphrase: NewPassphrase,
    pub(crate) remember_key: bool,
}

/// What the engine answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rotated {
    pub(crate) status: SyncStatus,
    /// `None` for "Keep this device's keys" (the core returns a status).
    pub(crate) outcome: Option<RotationOutcome>,
}

impl Rotated {
    /// The folder did not take the new keys yet: they are written later.
    pub(crate) fn pending(&self) -> bool {
        matches!(
            self.status,
            SyncStatus::Waiting {
                reason: WaitReason::RotationPending,
                ..
            }
        )
    }
}

#[derive(Debug)]
pub(crate) struct RotateFlow {
    pub(crate) kind: RotateKind,
    pub(crate) step: RotateStep,
    /// Devices, outdated ones, format upgrade (every kind).
    pub(crate) preview: Load<RotationPreview>,
    /// What forgetting affects (forget only).
    pub(crate) forget: Option<Load<ForgetPreview>>,
    /// Current passphrase or "I no longer know it".
    pub(crate) auth: PassForm,
    /// The new passphrase, remember, "I saved it".
    pub(crate) new: PassForm,
    /// A refusal that belongs to no field (busy, folder, unknown device).
    pub(crate) problem: Option<ErrorSummary>,
    pub(crate) result: Option<Rotated>,
    /// First line shown of a long step (`PgDn`).
    pub(crate) scroll: u16,
}

impl RotateFlow {
    pub(crate) fn new(kind: RotateKind, remember: bool) -> Self {
        let mut new = PassForm::new(PassKind::Rotate, String::new());
        new.remember = remember;
        Self {
            forget: matches!(kind, RotateKind::Forget { .. }).then_some(Load::Loading),
            kind,
            step: RotateStep::About,
            preview: Load::Loading,
            auth: PassForm::new(PassKind::Authorize, String::new()),
            new,
            problem: None,
            result: None,
            scroll: 0,
        }
    }

    /// The dialog's title.
    pub(crate) fn title(&self) -> String {
        match &self.kind {
            RotateKind::Rotate => "Rotate keys".to_owned(),
            RotateKind::Forget { label, .. } => {
                format!("Forget {}", crate::form::display_safe(label))
            }
            RotateKind::KeepMine => "Keep this device's keys".to_owned(),
        }
    }

    /// The previews needed before the first step can go on.
    pub(crate) fn previews_ready(&self) -> bool {
        let forget = self.forget.as_ref().is_none_or(|f| f.ready().is_some());
        self.preview.ready().is_some() && forget
    }

    /// The open form, if the step has one.
    pub(crate) fn form_mut(&mut self) -> Option<&mut PassForm> {
        match self.step {
            RotateStep::Authorize => Some(&mut self.auth),
            RotateStep::New | RotateStep::Confirm(_) => Some(&mut self.new),
            _ => None,
        }
    }

    pub(crate) fn form(&self) -> Option<&PassForm> {
        match self.step {
            RotateStep::Authorize => Some(&self.auth),
            RotateStep::New | RotateStep::Confirm(_) => Some(&self.new),
            _ => None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.step == RotateStep::Working
    }

    fn go(&mut self, step: RotateStep) {
        self.step = step;
        self.problem = None;
        self.scroll = 0;
    }

    /// The step before the new passphrase.
    fn before_new(&self) -> RotateStep {
        if self.kind == RotateKind::KeepMine {
            RotateStep::About
        } else {
            RotateStep::Authorize
        }
    }

    fn submit(&mut self) -> RotateOutcome {
        let authorization = if self.kind == RotateKind::KeepMine || self.auth.dont_know {
            RotateAuth::ResetConfirmed
        } else {
            RotateAuth::CurrentPassphrase(self.auth.take_old())
        };
        let remember_key = self.new.remember;
        let new_passphrase = self.new.take_new();
        self.go(RotateStep::Working);
        RotateOutcome::Submit(RotateRequest {
            kind: self.kind.clone(),
            authorization,
            new_passphrase,
            remember_key,
        })
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent, now: Instant) -> RotateOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.step {
            RotateStep::About => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => RotateOutcome::Close,
                KeyCode::Char('c') if ctrl => RotateOutcome::Close,
                KeyCode::PageDown | KeyCode::Char('J' | 'j') | KeyCode::Down => {
                    self.scroll = self.scroll.saturating_add(4);
                    RotateOutcome::None
                }
                KeyCode::PageUp | KeyCode::Char('K' | 'k') | KeyCode::Up => {
                    self.scroll = self.scroll.saturating_sub(4);
                    RotateOutcome::None
                }
                KeyCode::Enter if self.previews_ready() => {
                    let next = if self.kind == RotateKind::KeepMine {
                        RotateStep::New
                    } else {
                        RotateStep::Authorize
                    };
                    self.go(next);
                    RotateOutcome::None
                }
                _ => RotateOutcome::None,
            },
            RotateStep::Authorize => match self.auth.handle_key(key) {
                PassOutcome::Cancel => {
                    self.auth = PassForm::new(PassKind::Authorize, String::new());
                    self.go(RotateStep::About);
                    RotateOutcome::None
                }
                PassOutcome::Submit => {
                    self.go(RotateStep::New);
                    RotateOutcome::None
                }
                _ => RotateOutcome::None,
            },
            RotateStep::New => match self.new.handle_key(key) {
                PassOutcome::Cancel => {
                    let remember = self.new.remember;
                    self.new = PassForm::new(PassKind::Rotate, String::new());
                    self.new.remember = remember;
                    self.go(self.before_new());
                    RotateOutcome::None
                }
                PassOutcome::Submit => {
                    self.step = RotateStep::Confirm(now);
                    RotateOutcome::None
                }
                PassOutcome::Generate => RotateOutcome::Generate,
                PassOutcome::Copy => RotateOutcome::Copy,
                PassOutcome::None => RotateOutcome::None,
            },
            RotateStep::Confirm(opened) => {
                if now.saturating_duration_since(opened) < ARM_DELAY {
                    return RotateOutcome::None;
                }
                match key.code {
                    KeyCode::Char('y') => self.submit(),
                    KeyCode::Char('n') | KeyCode::Esc => {
                        self.step = RotateStep::New;
                        RotateOutcome::None
                    }
                    _ => RotateOutcome::None,
                }
            }
            RotateStep::Working => RotateOutcome::None,
            RotateStep::Done => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => RotateOutcome::Close,
                KeyCode::Char('c') if ctrl => RotateOutcome::Close,
                _ => RotateOutcome::None,
            },
        }
    }

    /// The engine answered.
    pub(crate) fn finished(&mut self, result: Result<Rotated, ErrorSummary>) {
        match result {
            Ok(rotated) => {
                self.result = Some(rotated);
                self.go(RotateStep::Done);
            }
            Err(e) => self.refused(e),
        }
    }

    /// A refusal goes back to the step that owns it, the field empty.
    pub(crate) fn refused(&mut self, error: ErrorSummary) {
        match error.code.as_str() {
            // The current passphrase did not open the keys.
            "sync_wrong_passphrase" if self.kind != RotateKind::KeepMine => {
                self.go(RotateStep::Authorize);
                self.auth.refused_at(PassField::Old, error);
            }
            "sync_passphrase_weak" | "sync_passphrase_reused" => {
                self.go(RotateStep::New);
                self.new.refused_at(PassField::New, error);
            }
            _ => {
                self.go(RotateStep::New);
                self.new.busy = false;
                self.problem = Some(error);
            }
        }
    }

    /// Without the current passphrase (reset-level question).
    pub(crate) fn without_passphrase(&self) -> bool {
        self.kind != RotateKind::KeepMine && self.auth.dont_know
    }

    fn confirm_words(&self) -> &'static str {
        match self.kind {
            RotateKind::Forget { .. } => "forget and rotate keys",
            RotateKind::KeepMine => "keep my keys",
            RotateKind::Rotate if self.without_passphrase() => "rotate without it",
            RotateKind::Rotate => "rotate keys",
        }
    }

    /// Key hints, most important first.
    pub(crate) fn hints(&self, now: Instant) -> Vec<(&'static str, &'static str)> {
        match self.step {
            RotateStep::About if self.previews_ready() => {
                vec![("↵", "continue"), ("PgDn", "scroll"), ("Esc", "cancel")]
            }
            RotateStep::About => vec![("Esc", "cancel")],
            RotateStep::Authorize => {
                let mut hints = vec![("↵", "continue"), ("Tab", "next field")];
                if self.auth.focused() == Some(PassField::DontKnow) {
                    hints.push(("Space", "I no longer know it"));
                }
                hints.push(("Esc", "back"));
                hints
            }
            RotateStep::New => {
                let primary = match self.kind {
                    RotateKind::Rotate => "rotate keys",
                    RotateKind::Forget { .. } => "forget and rotate keys",
                    RotateKind::KeepMine => "keep my keys",
                };
                let mut hints = vec![("↵", primary), ("Tab", "next field")];
                match self.new.focused() {
                    Some(PassField::New) if self.new.generated.is_some() => {
                        hints.extend([("c", "copy"), ("g", "another")]);
                    }
                    Some(PassField::Saved) => hints.push(("Space", "I saved it")),
                    _ => hints.push(("Ctrl-G", "generate")),
                }
                hints.push(("Esc", "back"));
                hints
            }
            RotateStep::Confirm(opened) if now.saturating_duration_since(opened) >= ARM_DELAY => {
                vec![("y", self.confirm_words()), ("n / Esc", "back")]
            }
            RotateStep::Confirm(_) | RotateStep::Working => Vec::new(),
            RotateStep::Done => vec![("↵", "close")],
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use portico_core::sync::api::RotationPeer;
    use secrecy::ExposeSecret;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(flow: &mut RotateFlow, text: &str, now: Instant) {
        for c in text.chars() {
            flow.handle_key(key(KeyCode::Char(c)), now);
        }
    }

    pub(crate) fn preview() -> RotationPreview {
        RotationPreview {
            devices: vec![RotationPeer {
                id: "77a1b2c3".repeat(4),
                label: "desk (77a1b2c3)".into(),
                this_device: false,
                app_version: Some("0.7.0".into()),
                outdated: false,
                waiting: false,
            }],
            outdated: Vec::new(),
            format_upgrade: false,
            exposed_accounts: Vec::new(),
        }
    }

    fn ready(kind: RotateKind) -> RotateFlow {
        let mut flow = RotateFlow::new(kind, true);
        flow.preview = Load::Ready(preview());
        flow
    }

    #[test]
    fn about_waits_for_the_preview() {
        let now = Instant::now();
        let mut flow = RotateFlow::new(RotateKind::Rotate, true);
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::About);
        flow.preview = Load::Ready(preview());
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::Authorize);
    }

    #[test]
    fn a_forget_also_waits_for_its_own_preview() {
        let now = Instant::now();
        let mut flow = ready(RotateKind::Forget {
            id: "77".into(),
            label: "desk (77a1b2c3)".into(),
        });
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::About);
    }

    #[test]
    fn current_passphrase_then_new_passphrase_then_saved() {
        let now = Instant::now();
        let mut flow = ready(RotateKind::Rotate);
        flow.handle_key(key(KeyCode::Enter), now);
        // Nothing typed, nothing checked: refused locally.
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::Authorize);
        assert!(flow.auth.problem.is_some());
        typed(&mut flow, "old words", now);
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::New);
        typed(&mut flow, "new long words", now);
        flow.handle_key(key(KeyCode::Tab), now);
        typed(&mut flow, "new long words", now);
        // "I saved it" not checked.
        assert!(matches!(
            flow.handle_key(key(KeyCode::Enter), now),
            RotateOutcome::None
        ));
        assert_eq!(flow.new.focused(), Some(PassField::Saved));
        flow.handle_key(key(KeyCode::Char(' ')), now);
        assert!(matches!(
            flow.handle_key(key(KeyCode::Enter), now),
            RotateOutcome::None
        ));
        assert!(matches!(flow.step, RotateStep::Confirm(_)));
        let later = now + ARM_DELAY + Duration::from_millis(1);
        let RotateOutcome::Submit(request) = flow.handle_key(key(KeyCode::Char('y')), later) else {
            panic!("submitted");
        };
        assert_eq!(flow.step, RotateStep::Working);
        let RotateAuth::CurrentPassphrase(old) = request.authorization else {
            panic!("current passphrase");
        };
        assert_eq!(old.expose_secret(), "old words");
        assert!(request.remember_key);
        // Wiped from the forms.
        assert!(flow.auth.old.is_empty());
        assert!(flow.new.new.is_empty() && flow.new.confirm.is_empty());
    }

    #[test]
    fn without_the_current_passphrase_only_y_rotates() {
        let start = Instant::now();
        let mut flow = ready(RotateKind::Rotate);
        flow.handle_key(key(KeyCode::Enter), start);
        flow.handle_key(key(KeyCode::Tab), start);
        flow.handle_key(key(KeyCode::Char(' ')), start);
        assert!(flow.auth.dont_know);
        flow.handle_key(key(KeyCode::Enter), start);
        assert_eq!(flow.step, RotateStep::New);
        typed(&mut flow, "new long words", start);
        flow.handle_key(key(KeyCode::Tab), start);
        typed(&mut flow, "new long words", start);
        flow.handle_key(key(KeyCode::Tab), start);
        flow.handle_key(key(KeyCode::Tab), start);
        flow.handle_key(key(KeyCode::Char(' ')), start);
        assert!(matches!(
            flow.handle_key(key(KeyCode::Enter), start),
            RotateOutcome::None
        ));
        assert!(matches!(flow.step, RotateStep::Confirm(_)));
        // Not armed yet, then `n` goes back, then armed `y` rotates.
        assert!(matches!(
            flow.handle_key(key(KeyCode::Char('y')), start),
            RotateOutcome::None
        ));
        let later = start + ARM_DELAY + Duration::from_millis(1);
        flow.handle_key(key(KeyCode::Char('x')), later);
        assert!(matches!(flow.step, RotateStep::Confirm(_)));
        let RotateOutcome::Submit(request) = flow.handle_key(key(KeyCode::Char('y')), later) else {
            panic!("submitted");
        };
        assert!(matches!(request.authorization, RotateAuth::ResetConfirmed));
    }

    #[test]
    fn typing_the_current_passphrase_unchecks_dont_know() {
        let now = Instant::now();
        let mut flow = ready(RotateKind::Rotate);
        flow.handle_key(key(KeyCode::Enter), now);
        flow.handle_key(key(KeyCode::Tab), now);
        flow.handle_key(key(KeyCode::Char(' ')), now);
        flow.handle_key(key(KeyCode::BackTab), now);
        typed(&mut flow, "x", now);
        assert!(!flow.auth.dont_know);
    }

    #[test]
    fn keep_mine_skips_the_authorization() {
        let now = Instant::now();
        let mut flow = ready(RotateKind::KeepMine);
        flow.handle_key(key(KeyCode::Enter), now);
        assert_eq!(flow.step, RotateStep::New);
        flow.handle_key(key(KeyCode::Esc), now);
        assert_eq!(flow.step, RotateStep::About);
    }

    #[test]
    fn refusals_go_back_to_their_field() {
        let mut flow = ready(RotateKind::Rotate);
        flow.step = RotateStep::Working;
        flow.finished(Err(ErrorSummary {
            code: "sync_wrong_passphrase".into(),
            message: "wrong sync passphrase".into(),
            hint: None,
        }));
        assert_eq!(flow.step, RotateStep::Authorize);
        assert_eq!(flow.auth.focused(), Some(PassField::Old));
        flow.step = RotateStep::Working;
        flow.finished(Err(ErrorSummary {
            code: "sync_passphrase_reused".into(),
            message: "this sync passphrase opens older sync keys".into(),
            hint: Some("choose another one".into()),
        }));
        assert_eq!(flow.step, RotateStep::New);
        assert!(
            flow.new
                .problem
                .as_ref()
                .is_some_and(|(f, ..)| *f == PassField::New)
        );
        flow.step = RotateStep::Working;
        flow.finished(Err(ErrorSummary {
            code: "sync_busy".into(),
            message: "sync is busy".into(),
            hint: None,
        }));
        assert_eq!(flow.step, RotateStep::New);
        assert!(flow.problem.is_some());
    }
}
