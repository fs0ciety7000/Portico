//! Copy-id orchestration as a pure state machine.
//!
//! `Connecting → Installing → Verifying → Done | Failed`. The app feeds it
//! session phases and engine results, and runs the [`Effect`] it returns
//! (install the key, verify the login, close a session opened only for this
//! job). Results carry the job's `generation`: an answer for a job that was
//! retried or abandoned is ignored.

use std::path::PathBuf;

use portico_core::CopyIdOutcome;
use portico_core::ErrorSummary;
use portico_core::events::SessionPhase;
use portico_core::ids::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Connecting,
    Installing,
    Verifying(CopyIdOutcome),
    Done(CopyIdOutcome),
    Failed(ErrorSummary),
}

impl Step {
    /// Index of the visible stage (`Connecting · Installing · Verifying`).
    pub(crate) fn stage(&self) -> usize {
        match self {
            Self::Connecting => 0,
            Self::Installing => 1,
            Self::Verifying(_) => 2,
            Self::Done(_) | Self::Failed(_) => 3,
        }
    }
}

/// What the app must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Effect {
    None,
    Install(SessionId),
    Verify(SessionId),
    /// Close the session opened just for this job.
    Disconnect(SessionId),
}

#[derive(Debug, Clone)]
pub(crate) struct CopyJob {
    pub(crate) key_name: String,
    pub(crate) public_path: PathBuf,
    pub(crate) private_path: PathBuf,
    /// The private key is here: the login can be verified with it (a
    /// `.pub` alone is installed, not verified).
    pub(crate) has_private: bool,
    pub(crate) alias: String,
    pub(crate) session: SessionId,
    /// The session was opened by this job (closed at the end).
    pub(crate) opened: bool,
    pub(crate) step: Step,
    pub(crate) generation: u64,
    /// Stage at which it failed (for the step line).
    pub(crate) failed_at: usize,
}

impl CopyJob {
    /// Starts on `session`; `phase` is its current phase (a reused session
    /// that is already connected goes straight to installing).
    #[allow(clippy::too_many_arguments)] // plain data, named at the call site
    pub(crate) fn start(
        key_name: String,
        public_path: PathBuf,
        private_path: PathBuf,
        has_private: bool,
        alias: String,
        session: SessionId,
        opened: bool,
        phase: &SessionPhase,
        generation: u64,
    ) -> (Self, Effect) {
        let mut job = Self {
            key_name,
            public_path,
            private_path,
            has_private,
            alias,
            session,
            opened,
            step: Step::Connecting,
            generation,
            failed_at: 0,
        };
        let effect = job.on_phase(session, phase);
        (job, effect)
    }

    pub(crate) fn running(&self) -> bool {
        !matches!(self.step, Step::Done(_) | Step::Failed(_))
    }

    fn fail(&mut self, error: ErrorSummary) -> Effect {
        self.failed_at = self.step.stage().min(2);
        self.step = Step::Failed(error);
        if self.opened {
            Effect::Disconnect(self.session)
        } else {
            Effect::None
        }
    }

    /// A phase of `session` (only the job's own session counts: the
    /// verification probe reports through [`Self::on_verified`]).
    pub(crate) fn on_phase(&mut self, session: SessionId, phase: &SessionPhase) -> Effect {
        if session != self.session || self.step != Step::Connecting {
            return Effect::None;
        }
        match phase {
            SessionPhase::Connected => {
                self.step = Step::Installing;
                Effect::Install(self.session)
            }
            SessionPhase::Failed { error } => {
                self.failed_at = 0;
                self.step = Step::Failed(error.clone());
                Effect::None
            }
            SessionPhase::Closed { .. } => {
                self.failed_at = 0;
                self.step = Step::Failed(ErrorSummary {
                    code: "session_ended".into(),
                    message: format!("the session to {} closed", self.alias),
                    hint: Some("Press r to try again.".into()),
                });
                Effect::None
            }
            _ => Effect::None,
        }
    }

    pub(crate) fn on_installed(
        &mut self,
        generation: u64,
        result: Result<CopyIdOutcome, ErrorSummary>,
    ) -> Effect {
        if generation != self.generation || self.step != Step::Installing {
            return Effect::None;
        }
        match result {
            // Verifying logs in with the private key: without it, the job
            // ends here and says so.
            Ok(outcome) if !self.has_private => {
                self.step = Step::Done(outcome);
                if self.opened {
                    Effect::Disconnect(self.session)
                } else {
                    Effect::None
                }
            }
            Ok(outcome) => {
                self.step = Step::Verifying(outcome);
                Effect::Verify(self.session)
            }
            Err(error) => self.fail(error),
        }
    }

    /// Verification was skipped (no private key here).
    pub(crate) fn unverified(&self) -> bool {
        !self.has_private
    }

    pub(crate) fn on_verified(
        &mut self,
        generation: u64,
        result: Result<(), ErrorSummary>,
    ) -> Effect {
        let Step::Verifying(outcome) = self.step.clone() else {
            return Effect::None;
        };
        if generation != self.generation {
            return Effect::None;
        }
        match result {
            Ok(()) => {
                self.step = Step::Done(outcome);
                if self.opened {
                    Effect::Disconnect(self.session)
                } else {
                    Effect::None
                }
            }
            Err(error) => self.fail(error),
        }
    }

    /// The success sentence.
    pub(crate) fn success_message(&self) -> Option<String> {
        match self.step {
            Step::Done(CopyIdOutcome::Added) if !self.has_private => {
                Some("Installed. Verification needs the private key.".to_owned())
            }
            Step::Done(CopyIdOutcome::AlreadyPresent) if !self.has_private => {
                Some("Already installed. Verification needs the private key.".to_owned())
            }
            Step::Done(CopyIdOutcome::Added) => Some(format!(
                "Installed and verified: you can now log in to {} with {}",
                self.alias, self.key_name
            )),
            Step::Done(CopyIdOutcome::AlreadyPresent) => Some(format!(
                "Already installed, and verified: you can log in to {} with {}",
                self.alias, self.key_name
            )),
            _ => None,
        }
    }

    /// Advice once installed (Portico never writes `~/.ssh/config`): where
    /// the line goes, then the line itself (shown as code).
    pub(crate) fn config_advice(&self) -> (String, String) {
        (
            format!(
                "To use it by default, add to Host {} in ~/.ssh/config:",
                self.alias
            ),
            format!("IdentityFile ~/.ssh/{}", self.key_name),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_job(opened: bool, phase: &SessionPhase) -> (CopyJob, Effect, SessionId) {
        job_with(true, opened, phase)
    }

    fn job_with(
        has_private: bool,
        opened: bool,
        phase: &SessionPhase,
    ) -> (CopyJob, Effect, SessionId) {
        let id = SessionId::next();
        let (job, effect) = CopyJob::start(
            "id_ed25519_web".into(),
            "/h/.ssh/id_ed25519_web.pub".into(),
            "/h/.ssh/id_ed25519_web".into(),
            has_private,
            "web".into(),
            id,
            opened,
            phase,
            7,
        );
        (job, effect, id)
    }

    fn error(code: &str) -> ErrorSummary {
        ErrorSummary {
            code: code.into(),
            message: "boom".into(),
            hint: None,
        }
    }

    #[test]
    fn full_run_on_a_session_opened_for_the_job() {
        let (mut job, effect, id) = new_job(true, &SessionPhase::Resolving);
        assert_eq!(effect, Effect::None);
        assert_eq!(job.step, Step::Connecting);
        // Another session's phase is not ours.
        assert_eq!(
            job.on_phase(SessionId::next(), &SessionPhase::Connected),
            Effect::None
        );
        assert_eq!(
            job.on_phase(id, &SessionPhase::Connected),
            Effect::Install(id)
        );
        // A stale answer (older generation) is ignored.
        assert_eq!(job.on_installed(6, Ok(CopyIdOutcome::Added)), Effect::None);
        assert_eq!(
            job.on_installed(7, Ok(CopyIdOutcome::Added)),
            Effect::Verify(id)
        );
        assert_eq!(job.on_verified(7, Ok(())), Effect::Disconnect(id));
        assert!(
            job.success_message()
                .is_some_and(|m| m.starts_with("Installed and verified"))
        );
        assert!(!job.running());
    }

    #[test]
    fn reused_connected_session_installs_at_once_and_stays_open() {
        let (mut job, effect, id) = new_job(false, &SessionPhase::Connected);
        assert_eq!(effect, Effect::Install(id));
        assert_eq!(
            job.on_installed(7, Ok(CopyIdOutcome::AlreadyPresent)),
            Effect::Verify(id)
        );
        assert_eq!(job.on_verified(7, Ok(())), Effect::None);
        assert!(
            job.success_message()
                .is_some_and(|m| m.starts_with("Already installed"))
        );
    }

    #[test]
    fn failures_keep_their_stage_and_close_what_was_opened() {
        let (mut job, _, id) = new_job(true, &SessionPhase::Resolving);
        job.on_phase(id, &SessionPhase::Connected);
        job.on_installed(7, Ok(CopyIdOutcome::Added));
        assert_eq!(
            job.on_verified(7, Err(error("copy_id_not_accepted"))),
            Effect::Disconnect(id)
        );
        assert_eq!(job.failed_at, 2);
        assert!(matches!(&job.step, Step::Failed(e) if e.code == "copy_id_not_accepted"));

        let (mut job, _, id) = new_job(true, &SessionPhase::Resolving);
        let failed = SessionPhase::Failed {
            error: error("auth_failed"),
        };
        // The session failed by itself: nothing to close.
        assert_eq!(job.on_phase(id, &failed), Effect::None);
        assert_eq!(job.failed_at, 0);
    }

    #[test]
    fn a_public_key_alone_is_installed_without_verification() {
        let (mut job, _, id) = job_with(false, true, &SessionPhase::Resolving);
        assert_eq!(
            job.on_phase(id, &SessionPhase::Connected),
            Effect::Install(id)
        );
        // No Verify step: done, and the session opened for it is closed.
        assert_eq!(
            job.on_installed(7, Ok(CopyIdOutcome::Added)),
            Effect::Disconnect(id)
        );
        assert_eq!(job.step, Step::Done(CopyIdOutcome::Added));
        assert!(job.unverified());
        assert_eq!(
            job.success_message().as_deref(),
            Some("Installed. Verification needs the private key.")
        );
        // A late verification answer changes nothing.
        assert_eq!(job.on_verified(7, Ok(())), Effect::None);
    }
}
