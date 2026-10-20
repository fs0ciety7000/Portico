//! Modal dialogs: engine prompts (host key, secrets, keyboard-interactive)
//! and the blocking "host key changed" alert.
//!
//! Secrets are typed into `Zeroizing` buffers, never displayed (not even
//! their length) and handed to the engine as `SecretString`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::engine::PromptResponse;
use portico_core::events::{Prompt, PromptKind};
use portico_core::ids::PromptId;
use secrecy::SecretString;
use zeroize::Zeroizing;

/// Keys arriving this soon after a dialog opens were typed for something
/// else (the shell in the active tab): they are ignored, so a stray `y`
/// never trusts a host key and stray keystrokes never become a password.
const ARM_DELAY: Duration = Duration::from_millis(400);
/// Maximum length of a typed or pasted answer.
const MAX_INPUT: usize = 256;

/// One input line in a dialog.
#[derive(Debug)]
pub(crate) struct Field {
    pub(crate) label: String,
    pub(crate) masked: bool,
    pub(crate) value: Zeroizing<String>,
}

impl Field {
    fn new(label: impl Into<String>, masked: bool) -> Self {
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

#[derive(Debug)]
pub(crate) enum Dialog {
    Prompt(PromptDialog),
    ChangedKey(ChangedKeyAlert),
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
