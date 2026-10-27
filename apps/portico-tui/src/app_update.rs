//! App glue for the opt-in update check (0.3.0): the one-time question,
//! the status-bar badge and the update view.
//!
//! - The engine makes the request, and only when `[updates] check = true`
//!   ([`portico_core::update`]); the TUI only shows its status, from
//!   `start_update_checks` at launch, `EngineEvent::Update` and
//!   `update_status` after a lagged bus.
//! - The question is a dialog (`y` check daily, `n` not now: never asked
//!   again, `Esc` later: asked at the next launch). A banner would have
//!   needed `y` and `n`, which the host list already uses (copy the
//!   `Include` line, add a host).
//! - `u` opens the update view (the release, or the setting and its
//!   state), `U` the setting itself (even when a release is known); both
//!   from the host list. Turning the check on always takes `y` in that
//!   view: no single key reaches the network.
//! - Portico never downloads nor runs anything: the view shows the release
//!   page and the command for this install, copied with OSC 52.

use portico_core::ErrorSummary;
use portico_core::update::{Frontend, UpdateInstruction, UpdateStatus};
use time::OffsetDateTime;

use crate::action::Action;
use crate::app::App;
use crate::dialog::{Answers, ConfirmAction, ConfirmDialog, Dialog};

/// The question, word for word as in the GUI.
pub(crate) const QUESTION: &str =
    "Check for new versions once a day? Portico would contact github.com and send nothing else.";

/// Under the question, muted.
pub(crate) const NOTE: &str = "Change it later: U on the host list.";

/// The closing line of the update view, word for word as in the GUI.
pub(crate) const NEVER_INSTALLS: &str = "Portico never downloads or installs anything itself.";

/// What the TUI knows of the update check.
#[derive(Debug, Clone, Default)]
pub(crate) struct UpdateState {
    /// `None` until the engine answered (or when `settings.toml` is
    /// unreadable: then nothing is asked nor shown).
    pub(crate) status: Option<UpdateStatus>,
    /// How to update this install, for the release it was computed for.
    pub(crate) instruction: Option<(String, UpdateInstruction)>,
    /// The question was put during this run.
    pub(crate) asked: bool,
}

impl UpdateState {
    /// `0.3.1 available`, when a newer release is known.
    pub(crate) fn badge(&self) -> Option<String> {
        let available = self.status.as_ref()?.available.as_ref()?;
        Some(format!("{} available", available.version))
    }
}

/// Spawns `fut` and sends its result back as an action. Outside a runtime
/// (pure state tests) nothing is spawned.
fn spawn<F>(app: &App, fut: F)
where
    F: Future<Output = Action> + Send + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let tx = app.tx.clone();
    runtime.spawn(async move {
        let _ = tx.send(fut.await);
    });
}

/// `3 h ago`, `2 days ago`… (`just now` under a minute).
pub(crate) fn ago(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let minutes = (now - at).whole_minutes().max(0);
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=2879 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// The one-time question.
pub(crate) fn question_dialog() -> ConfirmDialog {
    ConfirmDialog {
        title: "Updates".into(),
        subject: None,
        body: QUESTION.into(),
        confirm: "check daily",
        action: ConfirmAction::UpdateQuestion,
        // Keys typed just before it showed (for the host list) never answer it.
        armed: Some(std::time::Instant::now()),
        answers: Answers::YesNoLater { no: "not now" },
        note: Some(NOTE.into()),
    }
}

/// The setting view (`U`, or `u` without a newer release): the state of
/// the daily check, `y` turns it on or off.
pub(crate) fn settings_dialog(status: &UpdateStatus, now: OffsetDateTime) -> ConfirmDialog {
    let on = status.check == Some(true);
    let state_line = if on {
        match status.checked_at {
            Some(at) => format!("Daily update check: on (last checked {}).", ago(at, now)),
            None => "Daily update check: on.".into(),
        }
    } else {
        "Daily update check: off. When on, Portico contacts github.com once a day and sends nothing else.".into()
    };
    ConfirmDialog {
        title: "Updates".into(),
        subject: None,
        body: format!("You have Portico {}.\n{state_line}", status.current),
        confirm: if on { "turn off" } else { "turn on" },
        action: ConfirmAction::SetUpdateCheck(!on),
        armed: None,
        answers: Answers::Close,
        note: None,
    }
}

/// The update view: the release and how to update, or the setting.
pub(crate) fn update_dialog(state: &UpdateState, now: OffsetDateTime) -> Option<ConfirmDialog> {
    let status = state.status.as_ref()?;
    let dialog = if let Some(available) = &status.available {
        let instruction = state
            .instruction
            .as_ref()
            .filter(|(version, _)| *version == available.version)
            .map_or_else(
                || {
                    portico_core::update::instruction(
                        portico_core::update::InstallMethod::Other,
                        Frontend::Tui,
                        &available.version,
                    )
                },
                |(_, i)| i.clone(),
            );
        let mut body = format!(
            "You have {}.\n\nRelease notes: {}\n\n{}",
            status.current, available.url, instruction.text
        );
        if let Some(command) = &instruction.command {
            body.push('\n');
            body.push_str(command);
        }
        body.push_str("\n\n");
        body.push_str(NEVER_INSTALLS);
        let (confirm, copied) = match instruction.command {
            Some(command) => ("copy command", command),
            None => ("copy link", available.url.clone()),
        };
        ConfirmDialog {
            title: format!("Portico {} is available", available.version),
            subject: None,
            body,
            confirm,
            action: ConfirmAction::CopyUpdate(copied),
            armed: None,
            answers: Answers::Close,
            note: None,
        }
    } else {
        settings_dialog(status, now)
    };
    Some(dialog)
}

impl App {
    /// Update-check actions (after `update_hosts` in the `update` chain).
    pub(crate) fn update_updates(&mut self, action: Action) {
        match action {
            Action::UpdateLoaded(Ok(status)) => self.update_status_changed(status),
            Action::UpdateInstruction {
                version,
                instruction,
            } => self.update.instruction = Some((version, instruction)),
            Action::ShowUpdate => {
                if let Some(dialog) = update_dialog(&self.update, OffsetDateTime::now_utc()) {
                    self.dialogs.push_back(Dialog::Confirm(dialog));
                }
            }
            Action::ShowUpdateSettings => {
                if let Some(status) = &self.update.status {
                    let dialog = settings_dialog(status, OffsetDateTime::now_utc());
                    self.dialogs.push_back(Dialog::Confirm(dialog));
                }
            }
            Action::UpdateCheckSaved(result) => self.update_check_saved(result),
            // `UpdateLoaded(Err)`: settings.toml is unreadable, its own toast
            // says so; nothing is asked (the answer could not be saved).
            Action::UpdateLoaded(Err(_)) => {}
            other => self.update_sync(other),
        }
    }

    /// At launch: the engine starts its checks if they are on, and says
    /// whether to ask.
    pub(crate) fn start_update_checks(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::UpdateLoaded(engine.start_update_checks().await.map_err(|e| e.summary()))
        });
    }

    /// After a lagged bus: the status again (an `Update` event may be lost).
    pub(crate) fn resync_update(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::UpdateLoaded(engine.update_status().await.map_err(|e| e.summary()))
        });
    }

    /// A new status (launch, event, saved setting).
    pub(crate) fn update_status_changed(&mut self, status: UpdateStatus) {
        if let Some(available) = &status.available
            && self
                .update
                .instruction
                .as_ref()
                .is_none_or(|(v, _)| *v != available.version)
        {
            let version = available.version.clone();
            spawn(self, async move {
                let target = version.clone();
                let instruction = tokio::task::spawn_blocking(move || {
                    portico_core::update::install_instruction(Frontend::Tui, &target)
                })
                .await
                .unwrap_or_else(|_| {
                    portico_core::update::instruction(
                        portico_core::update::InstallMethod::Other,
                        Frontend::Tui,
                        &version,
                    )
                });
                Action::UpdateInstruction {
                    version,
                    instruction,
                }
            });
        }
        let ask = status.check.is_none() && !self.update.asked;
        self.update.status = Some(status);
        if ask {
            self.update.asked = true;
            self.dialogs.push_back(Dialog::Confirm(question_dialog()));
        }
    }

    /// Writes `[updates] check` (the engine starts or stops its checks).
    pub(crate) fn save_update_check(&mut self, on: bool) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::UpdateCheckSaved(engine.set_update_check(on).await.map_err(|e| e.summary()))
        });
    }

    fn update_check_saved(&mut self, result: Result<UpdateStatus, ErrorSummary>) {
        match result {
            Ok(status) => {
                self.notify(if status.check == Some(true) {
                    "Daily update check on · Portico contacts github.com once a day".into()
                } else {
                    "Update check off · Portico never contacts github.com".into()
                });
                self.update_status_changed(status);
            }
            Err(e) => self.notify_error(e),
        }
    }

    /// `y` in the update view: the command (or the release page) to the
    /// clipboard.
    pub(crate) fn copy_update(&mut self, text: &str) {
        self.osc52(text);
        self.notify(if text.starts_with("https://") {
            "Release page link copied".into()
        } else {
            "Update command copied".into()
        });
    }
}

#[cfg(test)]
mod tests;
