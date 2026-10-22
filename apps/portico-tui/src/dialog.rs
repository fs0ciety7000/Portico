//! Modal dialogs: engine prompts (host key, secrets, keyboard-interactive),
//! the blocking "host key changed" alert, and Portico's own secret dialogs
//! (vault unlock / create / change, key passphrase for `ssh-agent`).
//!
//! Secrets are typed into `Zeroizing` buffers, never displayed (not even
//! their length) and handed to the engine as `SecretString`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::engine::PromptResponse;
use portico_core::events::{Prompt, PromptKind};
use portico_core::ids::PromptId;
use portico_core::vault::MIN_PASSPHRASE_CHARS;
use secrecy::SecretString;
use zeroize::Zeroizing;

/// Keys arriving this soon after a dialog opens were typed for something
/// else (the shell in the active tab): they are ignored, so a stray `y`
/// never trusts a host key and stray keystrokes never become a password.
pub(crate) const ARM_DELAY: Duration = Duration::from_millis(400);
/// Maximum length of a typed or pasted answer.
const MAX_INPUT: usize = 256;

/// One input line in a dialog. `Debug` never shows a masked value.
pub(crate) struct Field {
    pub(crate) label: String,
    pub(crate) masked: bool,
    pub(crate) value: Zeroizing<String>,
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value: &str = if self.masked {
            "[redacted]"
        } else {
            &self.value
        };
        f.debug_struct("Field")
            .field("label", &self.label)
            .field("masked", &self.masked)
            .field("value", &value)
            .finish()
    }
}

impl Field {
    pub(crate) fn new(label: impl Into<String>, masked: bool) -> Self {
        Self {
            label: label.into(),
            masked,
            // Pre-sized so typing never reallocates (no stray secret copies).
            value: Zeroizing::new(String::with_capacity(MAX_INPUT * 4)),
        }
    }
}

/// An engine prompt being answered.
#[derive(Debug)]
pub(crate) struct PromptDialog {
    pub(crate) id: PromptId,
    pub(crate) kind: PromptKind,
    /// Host alias of the session that asked (for the title).
    pub(crate) alias: Option<String>,
    pub(crate) fields: Vec<Field>,
    /// Index of the focused field; `fields.len()` is the "remember" toggle.
    pub(crate) focus: usize,
    pub(crate) remember: bool,
    /// Where "remember" stores the secret (system keyring or vault).
    pub(crate) remember_in: &'static str,
    /// Typed confirmation for risky host keys (`yes`).
    pub(crate) confirm: String,
    pub(crate) opened_at: Instant,
}

/// What a key press did to a dialog.
#[derive(Debug)]
pub(crate) enum Outcome {
    Pending,
    Answer(PromptResponse),
}

impl PromptDialog {
    pub(crate) fn new(prompt: Prompt, alias: Option<String>) -> Self {
        let fields = match &prompt.kind {
            PromptKind::Password { .. } => vec![Field::new("Password", true)],
            PromptKind::KeyPassphrase { .. } => vec![Field::new("Passphrase", true)],
            PromptKind::KeyboardInteractive { prompts, .. } => prompts
                .iter()
                .map(|p| Field::new(p.text.trim_end().trim_end_matches(':'), !p.echo))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            id: prompt.id,
            kind: prompt.kind,
            alias,
            fields,
            focus: 0,
            remember: false,
            remember_in: "the system keyring",
            confirm: String::new(),
            opened_at: Instant::now(),
        }
    }

    /// Secrets that can be remembered in the keyring.
    pub(crate) fn can_remember(&self) -> bool {
        matches!(
            self.kind,
            PromptKind::Password { .. } | PromptKind::KeyPassphrase { .. }
        )
    }

    /// Unknown key while other key types are already known: unusual, so a
    /// typed `yes` is required instead of a single key.
    pub(crate) fn needs_typed_confirmation(&self) -> bool {
        matches!(&self.kind, PromptKind::UnknownHostKey { other_algorithms, .. } if !other_algorithms.is_empty())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.opened_at.elapsed() < ARM_DELAY {
            return Outcome::Pending;
        }
        if let PromptKind::UnknownHostKey { .. } = self.kind {
            return self.host_key_key(key);
        }
        let stops = self.fields.len() + usize::from(self.can_remember());
        match key.code {
            KeyCode::Esc => return Outcome::Answer(PromptResponse::Cancel),
            KeyCode::Enter => return Outcome::Answer(self.submit()),
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % stops.max(1),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + stops.max(1) - 1) % stops.max(1);
            }
            KeyCode::Char(' ') if self.focus == self.fields.len() => self.remember = !self.remember,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    f.value.clear();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.focus)
                    && f.value.chars().count() < MAX_INPUT
                {
                    f.value.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    f.value.pop();
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    /// Pasted text goes to the focused field (first line, no control
    /// characters). Never answers a host key question.
    pub(crate) fn paste(&mut self, text: &str) {
        if self.opened_at.elapsed() < ARM_DELAY {
            return;
        }
        let Some(field) = self.fields.get_mut(self.focus) else {
            return;
        };
        let line = text.lines().next().unwrap_or_default();
        for c in line.chars().filter(|c| !c.is_control()) {
            if field.value.chars().count() >= MAX_INPUT {
                break;
            }
            field.value.push(c);
        }
    }

    fn host_key_key(&mut self, key: KeyEvent) -> Outcome {
        if self.needs_typed_confirmation() {
            return match key.code {
                KeyCode::Enter if self.confirm == "yes" => {
                    Outcome::Answer(PromptResponse::AcceptHostKey)
                }
                KeyCode::Esc | KeyCode::Enter => Outcome::Answer(PromptResponse::Cancel),
                KeyCode::Backspace => {
                    self.confirm.pop();
                    Outcome::Pending
                }
                KeyCode::Char(c) if self.confirm.len() < 3 => {
                    self.confirm.push(c);
                    Outcome::Pending
                }
                _ => Outcome::Pending,
            };
        }
        match key.code {
            KeyCode::Char('y' | 'Y') => Outcome::Answer(PromptResponse::AcceptHostKey),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Outcome::Answer(PromptResponse::Cancel),
            _ => Outcome::Pending,
        }
    }

    fn submit(&mut self) -> PromptResponse {
        let take = |f: &mut Field| {
            // Copy into an exact-size secret, then wipe the input buffer.
            let secret = SecretString::from(f.value.as_str());
            zeroize::Zeroize::zeroize(&mut *f.value);
            secret
        };
        match self.kind {
            PromptKind::KeyboardInteractive { .. } => {
                PromptResponse::Answers(self.fields.iter_mut().map(take).collect())
            }
            _ => match self.fields.first_mut() {
                Some(f) => PromptResponse::Secret {
                    value: take(f),
                    remember: self.remember,
                },
                None => PromptResponse::Cancel,
            },
        }
    }
}

/// The server key differs from `known_hosts`: information only.
#[derive(Debug, Clone)]
pub(crate) struct ChangedKeyAlert {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) algorithm: String,
    pub(crate) fingerprint: String,
    pub(crate) expected: String,
    pub(crate) file: String,
    pub(crate) line: usize,
}

/// What a confirmation dialog confirms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmAction {
    ClearRecent,
}

/// A yes / no question: `y` confirms, `n` or `Esc` keeps things as they are.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmDialog {
    pub(crate) title: String,
    pub(crate) body: String,
    /// Label of the confirming action (`y`).
    pub(crate) confirm: &'static str,
    pub(crate) action: ConfirmAction,
}

#[derive(Debug)]
pub(crate) enum Dialog {
    Prompt(PromptDialog),
    ChangedKey(ChangedKeyAlert),
    Secret(SecretDialog),
    Confirm(ConfirmDialog),
}

/// Why Portico itself asks for a secret.
#[derive(Debug, Clone)]
pub(crate) enum SecretPurpose {
    /// Unlock the vault; `startup` when asked at launch (Esc continues
    /// without remembered secrets), else after "remember" was ticked.
    VaultUnlock {
        startup: bool,
    },
    /// Create the vault; `offered` after "remember" was ticked without one.
    VaultCreate {
        offered: bool,
    },
    VaultChange,
    /// Decrypt a private key to hand it to `ssh-agent`.
    AgentPassphrase {
        key_name: String,
        private_path: PathBuf,
        lifetime: Option<Duration>,
    },
}

/// A Portico secret dialog. Same rules as engine prompts: fixed mask,
/// buffers wiped once handed over, 400 ms arming, pastes land here.
#[derive(Debug)]
pub(crate) struct SecretDialog {
    pub(crate) purpose: SecretPurpose,
    pub(crate) fields: Vec<Field>,
    pub(crate) focus: usize,
    /// Last refusal (wrong passphrase, too short…), shown inline.
    pub(crate) error: Option<ErrorSummary>,
    /// Wrong answers so far (shown as a counter).
    pub(crate) failures: u32,
    /// Waiting for the engine (Argon2 takes a moment).
    pub(crate) busy: bool,
    pub(crate) opened_at: Instant,
}

pub(crate) enum SecretOutcome {
    Pending,
    Cancel,
    /// The answers, in field order (create: the passphrase; change: the
    /// current one, then the new one).
    Submit(Vec<SecretString>),
}

impl SecretDialog {
    pub(crate) fn new(purpose: SecretPurpose) -> Self {
        let labels: &[&str] = match purpose {
            SecretPurpose::VaultUnlock { .. } | SecretPurpose::AgentPassphrase { .. } => {
                &["Passphrase"]
            }
            SecretPurpose::VaultCreate { .. } => &["Passphrase", "Confirm"],
            SecretPurpose::VaultChange => &["Current", "New", "Confirm"],
        };
        Self {
            purpose,
            fields: labels.iter().map(|l| Field::new(*l, true)).collect(),
            focus: 0,
            error: None,
            failures: 0,
            busy: false,
            opened_at: Instant::now(),
        }
    }

    /// Characters of the new passphrase (create, change), capped at the
    /// minimum: the counter says whether it is long enough, never more.
    /// `(field index, characters)`.
    pub(crate) fn new_passphrase_progress(&self) -> Option<(usize, usize)> {
        let index = match self.purpose {
            SecretPurpose::VaultCreate { .. } => 0,
            SecretPurpose::VaultChange => 1,
            _ => return None,
        };
        let typed = self.fields.get(index)?.value.chars().count();
        Some((index, typed.min(MIN_PASSPHRASE_CHARS)))
    }

    /// The new passphrase and its confirmation must agree and be long
    /// enough (create, change); a single passphrase must not be empty.
    fn local_problem(&self) -> Option<ErrorSummary> {
        let problem = |message: &str, hint: Option<&str>| ErrorSummary {
            code: "vault_form".into(),
            message: message.to_owned(),
            hint: hint.map(str::to_owned),
        };
        let values: Vec<&str> = self.fields.iter().map(|f| f.value.as_str()).collect();
        let (new, confirm) = match (&self.purpose, values.as_slice()) {
            (SecretPurpose::VaultCreate { .. }, [new, confirm])
            | (SecretPurpose::VaultChange, [_, new, confirm]) => (*new, *confirm),
            (_, ["", ..]) => {
                return Some(problem("type the passphrase", None));
            }
            _ => return None,
        };
        if new.chars().count() < MIN_PASSPHRASE_CHARS {
            return Some(problem(
                &format!("the vault passphrase needs at least {MIN_PASSPHRASE_CHARS} characters"),
                Some("Several words are long and easy to remember."),
            ));
        }
        (new != confirm).then(|| problem("the passphrases do not match", None))
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> SecretOutcome {
        if self.busy || self.opened_at.elapsed() < ARM_DELAY {
            return SecretOutcome::Pending;
        }
        let stops = self.fields.len().max(1);
        match key.code {
            KeyCode::Esc => return SecretOutcome::Cancel,
            KeyCode::Enter => {
                if let Some(problem) = self.local_problem() {
                    self.error = Some(problem);
                    return SecretOutcome::Pending;
                }
                return SecretOutcome::Submit(self.take());
            }
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % stops,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + stops - 1) % stops,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    zeroize::Zeroize::zeroize(&mut *f.value);
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(f) = self.fields.get_mut(self.focus)
                    && f.value.chars().count() < MAX_INPUT
                {
                    f.value.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(f) = self.fields.get_mut(self.focus) {
                    f.value.pop();
                }
            }
            _ => {}
        }
        SecretOutcome::Pending
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.busy || self.opened_at.elapsed() < ARM_DELAY {
            return;
        }
        if let Some(field) = self.fields.get_mut(self.focus) {
            let line = text.lines().next().unwrap_or_default();
            for c in line.chars().filter(|c| !c.is_control()) {
                if field.value.chars().count() >= MAX_INPUT {
                    break;
                }
                field.value.push(c);
            }
        }
    }

    /// Hands the answers over and wipes every buffer.
    fn take(&mut self) -> Vec<SecretString> {
        let count = match self.purpose {
            SecretPurpose::VaultChange => 2,
            _ => 1,
        };
        let answers = self
            .fields
            .iter()
            .take(count)
            .map(|f| SecretString::from(f.value.as_str()))
            .collect();
        for f in &mut self.fields {
            zeroize::Zeroize::zeroize(&mut *f.value);
        }
        self.busy = true;
        self.error = None;
        answers
    }

    /// The engine refused the answer: say why and ask again (re-armed, so
    /// a held `Enter` does not resubmit an empty field).
    pub(crate) fn fail(&mut self, error: ErrorSummary) {
        self.busy = false;
        self.failures = self.failures.saturating_add(1);
        self.error = Some(error);
        self.focus = 0;
        self.opened_at = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use portico_core::events::InteractivePrompt;
    use secrecy::ExposeSecret;

    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// A dialog that has been open long enough to accept keys.
    fn dialog(kind: PromptKind) -> PromptDialog {
        let mut d = PromptDialog::new(
            Prompt {
                id: PromptId::next(),
                session: None,
                kind,
            },
            None,
        );
        d.opened_at = Instant::now()
            .checked_sub(ARM_DELAY * 2)
            .unwrap_or_else(Instant::now);
        d
    }

    #[test]
    fn keys_right_after_opening_are_ignored() {
        let mut d = PromptDialog::new(
            Prompt {
                id: PromptId::next(),
                session: None,
                kind: PromptKind::UnknownHostKey {
                    host: "h".into(),
                    port: 22,
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: "SHA256:x".into(),
                    other_algorithms: vec![],
                },
            },
            None,
        );
        assert!(matches!(
            d.handle_key(key(KeyCode::Char('y'))),
            Outcome::Pending
        ));
        d.opened_at = Instant::now()
            .checked_sub(ARM_DELAY * 2)
            .unwrap_or_else(Instant::now);
        assert!(matches!(
            d.handle_key(key(KeyCode::Char('y'))),
            Outcome::Answer(PromptResponse::AcceptHostKey)
        ));
    }

    #[test]
    fn paste_fills_the_field_but_never_answers_a_host_key() {
        let mut d = dialog(PromptKind::Password {
            user: "u".into(),
            host: "h".into(),
            attempt: 1,
            attempts: 3,
        });
        d.paste("hunter2\nsecond line");
        let Outcome::Answer(PromptResponse::Secret { value, .. }) =
            d.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected a secret");
        };
        assert_eq!(value.expose_secret(), "hunter2");

        let mut hk = dialog(PromptKind::UnknownHostKey {
            host: "h".into(),
            port: 22,
            algorithm: "a".into(),
            fingerprint: "f".into(),
            other_algorithms: vec!["b".into()],
        });
        hk.paste("yes");
        assert!(hk.confirm.is_empty());
    }

    #[test]
    fn password_with_remember() {
        let mut d = dialog(PromptKind::Password {
            user: "u".into(),
            host: "h".into(),
            attempt: 1,
            attempts: 3,
        });
        for c in "s3cr3t".chars() {
            d.handle_key(key(KeyCode::Char(c)));
        }
        d.handle_key(key(KeyCode::Tab));
        d.handle_key(key(KeyCode::Char(' ')));
        let Outcome::Answer(PromptResponse::Secret { value, remember }) =
            d.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected a secret");
        };
        assert_eq!(value.expose_secret(), "s3cr3t");
        assert!(remember);
        assert!(d.fields[0].value.is_empty(), "buffer handed over, not kept");
    }

    #[test]
    fn host_key_needs_explicit_yes() {
        let mut d = dialog(PromptKind::UnknownHostKey {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            other_algorithms: vec![],
        });
        assert!(matches!(
            d.handle_key(key(KeyCode::Enter)),
            Outcome::Pending
        ));
        assert!(matches!(
            d.handle_key(key(KeyCode::Char('y'))),
            Outcome::Answer(PromptResponse::AcceptHostKey)
        ));

        let mut risky = dialog(PromptKind::UnknownHostKey {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            other_algorithms: vec!["ecdsa-sha2-nistp256".into()],
        });
        assert!(matches!(
            risky.handle_key(key(KeyCode::Char('y'))),
            Outcome::Pending
        ));
        assert!(matches!(
            risky.handle_key(key(KeyCode::Enter)),
            Outcome::Answer(PromptResponse::Cancel)
        ));
    }

    fn armed(purpose: SecretPurpose) -> SecretDialog {
        let mut d = SecretDialog::new(purpose);
        d.opened_at = Instant::now()
            .checked_sub(ARM_DELAY * 2)
            .unwrap_or_else(Instant::now);
        d
    }

    fn typed(d: &mut SecretDialog, text: &str) {
        for c in text.chars() {
            d.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn vault_create_checks_length_and_confirmation() {
        let mut d = armed(SecretPurpose::VaultCreate { offered: false });
        typed(&mut d, "short");
        d.handle_key(key(KeyCode::Tab));
        typed(&mut d, "short");
        assert!(matches!(
            d.handle_key(key(KeyCode::Enter)),
            SecretOutcome::Pending
        ));
        assert!(
            d.error
                .as_ref()
                .is_some_and(|e| e.message.contains("at least 8"))
        );
        d.handle_key(key(KeyCode::Tab));
        typed(&mut d, "correct horse");
        d.handle_key(key(KeyCode::Tab));
        typed(&mut d, " battery");
        assert!(matches!(
            d.handle_key(key(KeyCode::Enter)),
            SecretOutcome::Pending
        ));
        assert!(
            d.error
                .as_ref()
                .is_some_and(|e| e.message.contains("do not match"))
        );
    }

    #[test]
    fn vault_change_hands_over_old_and_new_then_wipes() {
        let mut d = armed(SecretPurpose::VaultChange);
        typed(&mut d, "old passphrase");
        d.handle_key(key(KeyCode::Tab));
        typed(&mut d, "new passphrase");
        d.handle_key(key(KeyCode::Tab));
        typed(&mut d, "new passphrase");
        let SecretOutcome::Submit(answers) = d.handle_key(key(KeyCode::Enter)) else {
            panic!("submit");
        };
        let got: Vec<&str> = answers.iter().map(ExposeSecret::expose_secret).collect();
        assert_eq!(got, ["old passphrase", "new passphrase"]);
        assert!(d.fields.iter().all(|f| f.value.is_empty()));
        assert!(d.busy);
        // Busy: keys are ignored until the engine answers.
        assert!(matches!(
            d.handle_key(key(KeyCode::Esc)),
            SecretOutcome::Pending
        ));
        d.fail(ErrorSummary {
            code: "vault_wrong_passphrase".into(),
            message: "wrong vault passphrase".into(),
            hint: None,
        });
        assert_eq!(d.failures, 1);
        // Re-armed: an immediate key is ignored.
        assert!(matches!(
            d.handle_key(key(KeyCode::Esc)),
            SecretOutcome::Pending
        ));
    }

    #[test]
    fn secret_fields_debug_is_redacted() {
        let mut d = armed(SecretPurpose::VaultCreate { offered: false });
        typed(&mut d, "hunter2hunter2");
        let shown = format!("{d:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("[redacted]"));
        // An echoed (not masked) field shows its value.
        let mut code = Field::new("Code", false);
        code.value.push_str("123456");
        assert!(format!("{code:?}").contains("123456"));

        let mut form = crate::keys::GenForm::new("web".into(), "me@laptop".into(), Vec::new());
        form.passphrase.push_str("correct horse");
        form.confirm.push_str("correct horse");
        let shown = format!("{form:?}");
        assert!(!shown.contains("correct horse"), "{shown}");
        assert!(shown.contains("[redacted]"));
        assert!(shown.contains("me@laptop"), "the rest stays readable");
    }

    #[test]
    fn keyboard_interactive_answers_every_field() {
        let mut d = dialog(PromptKind::KeyboardInteractive {
            name: String::new(),
            instructions: String::new(),
            prompts: vec![
                InteractivePrompt {
                    text: "Password: ".into(),
                    echo: false,
                },
                InteractivePrompt {
                    text: "Code: ".into(),
                    echo: true,
                },
            ],
        });
        d.handle_key(key(KeyCode::Char('a')));
        d.handle_key(key(KeyCode::Tab));
        d.handle_key(key(KeyCode::Char('1')));
        let Outcome::Answer(PromptResponse::Answers(values)) = d.handle_key(key(KeyCode::Enter))
        else {
            panic!("expected answers");
        };
        let got: Vec<&str> = values.iter().map(ExposeSecret::expose_secret).collect();
        assert_eq!(got, ["a", "1"]);
    }
}
