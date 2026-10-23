//! App glue for the phase 3 panels: keys (local, agent, generation,
//! copy-id), vault dialogs and snippets. Engine calls run in tasks and come
//! back as [`Action`]s; the panels' own state lives in `keys`, `snippets`
//! and `copy_job` (pure, tested there).

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alacritty_terminal::term::TermMode;
use crossterm::event::KeyEvent;
use portico_core::ErrorSummary;
use portico_core::engine::ConnectRequest;
use portico_core::events::SessionPhase;
use portico_core::ids::SessionId;
use portico_core::keys::LocalKey;
use portico_core::vault::VaultStatus;

use crate::action::Action;
use crate::app::{App, Focus, Overlay, SessionView};
use crate::copy_job::{CopyJob, Effect, Step};
use crate::dialog::{Dialog, SecretDialog, SecretOutcome, SecretPurpose};
use crate::keys::{
    CopyState, GenForm, HostPicker, KeysOutcome, KeysPanel, Load, Mode, Notice, file_name,
    host_hint, taken_names,
};
use crate::snippets::{SnipMode, SnipOutcome, SnippetsPanel, snippet_bytes};

/// Longest public key line shown or sent to the clipboard.
const MAX_PUBLIC_LINE: usize = 16 * 1024;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Spawns `fut` and sends its result back as an action.
fn spawn<F>(app: &App, fut: F)
where
    F: Future<Output = Action> + Send + 'static,
{
    let tx = app.tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(fut.await);
    });
}

/// Standard base64 (for OSC 52).
pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |i: u32| {
        let index = usize::try_from(i & 63).unwrap_or(0);
        char::from(ALPHABET.get(index).copied().unwrap_or(b'A'))
    };
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (b(0) << 16) | (b(1) << 8) | b(2);
        out.push(sym(n >> 18));
        out.push(sym(n >> 12));
        out.push(if chunk.len() > 1 { sym(n >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(n) } else { '=' });
    }
    out
}

/// This machine's name: `$HOSTNAME`, `/etc/hostname`, then `uname -n`.
/// Blocking (file read, subprocess): called from `spawn_blocking`.
fn machine_name() -> Option<String> {
    let clean = |s: &str| {
        let name: String = s
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .collect();
        // Short name: `laptop.local` → `laptop`.
        let short = name.split('.').next().unwrap_or_default().to_owned();
        (!short.is_empty()).then_some(short)
    };
    std::env::var("HOSTNAME")
        .ok()
        .and_then(|h| clean(&h))
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .and_then(|h| clean(&h))
        })
        .or_else(|| {
            std::process::Command::new("uname")
                .arg("-n")
                .output()
                .ok()
                .and_then(|o| clean(&String::from_utf8_lossy(&o.stdout)))
        })
}

impl App {
    // ── shared ─────────────────────────────────────────────────────────────

    /// Host the user is looking at: the active tab's, else the selected one.
    fn current_alias(&self) -> Option<String> {
        if matches!(self.focus, Focus::Terminal | Focus::Files)
            && let Some(slot) = self.focused_slot()
        {
            return Some(slot.alias().to_owned());
        }
        self.selected_host().map(|h| h.alias.clone())
    }

    fn tags_of(&self, alias: &str) -> Vec<String> {
        self.inventory
            .as_ref()
            .and_then(|i| i.hosts.iter().find(|h| h.alias == alias))
            .map(|h| h.meta.tags.clone())
            .unwrap_or_default()
    }

    pub(crate) fn spawn_machine_name(&self) {
        spawn(self, async {
            let name = tokio::task::spawn_blocking(machine_name)
                .await
                .ok()
                .flatten();
            Action::MachineName(name)
        });
    }

    pub(crate) fn refresh_vault(&self, startup: bool) {
        let engine = self.engine.clone();
        spawn(self, async move {
            let result = engine.vault_status().await.map_err(|e| e.summary());
            Action::VaultStatus { startup, result }
        });
    }

    /// Actions of the phase 3 panels (the rest is in `App::update`).
    // Flat dispatch table, one arm per action.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn update_panels(&mut self, action: Action) {
        match action {
            Action::OpenKeys => self.open_keys(),
            Action::KeysKey(key) => self.keys_key(key),
            Action::MachineName(name) => self.machine = name,
            Action::LocalKeys(result) => {
                if let Some(panel) = &mut self.keys_panel {
                    panel.set_local(result);
                }
            }
            Action::AgentKeys(result) => {
                if let Some(panel) = &mut self.keys_panel {
                    panel.set_agent(result);
                }
            }
            Action::KeyGenerated(result) => self.key_generated(result),
            Action::AgentAdded {
                key,
                lifetime,
                result,
            } => match result {
                Ok(_) => self.agent_changed(Notice::ok(added_message(&key, lifetime))),
                Err(e) if e.code == "key_needs_passphrase" => {
                    self.dialogs.push_back(Dialog::Secret(SecretDialog::new(
                        SecretPurpose::AgentPassphrase {
                            key_name: file_name(&key.private_path),
                            private_path: key.private_path,
                            lifetime,
                        },
                    )));
                }
                Err(e) => self.keys_notice(Notice::error(e)),
            },
            Action::AgentRemoved { label, result } => match result {
                Ok(()) => self.agent_changed(Notice::ok(format!("Removed {label} from the agent"))),
                Err(e) => self.keys_notice(Notice::error(e)),
            },
            Action::PublicKeyRead(result) => match result {
                Ok(line) => self.copy_public(line),
                Err(e) => self.keys_notice(Notice::error(e)),
            },
            Action::CopyInstalled { generation, result } => {
                let effect = self
                    .copy_job
                    .as_mut()
                    .map_or(Effect::None, |j| j.on_installed(generation, result));
                self.run_copy_effect(effect);
                self.copy_finished_elsewhere();
            }
            Action::CopyVerified { generation, result } => {
                let effect = self
                    .copy_job
                    .as_mut()
                    .map_or(Effect::None, |j| j.on_verified(generation, result));
                self.run_copy_effect(effect);
                self.copy_finished_elsewhere();
            }
            Action::VaultStatus {
                startup,
                result: Ok(status),
            } => {
                self.vault = Some(status);
                let asking = self.dialogs.iter().any(|d| {
                    matches!(d, Dialog::Secret(s) if matches!(s.purpose, SecretPurpose::VaultUnlock { .. }))
                });
                if startup && status == VaultStatus::Locked && !asking {
                    self.dialogs.push_back(Dialog::Secret(SecretDialog::new(
                        SecretPurpose::VaultUnlock { startup: true },
                    )));
                }
            }
            Action::SecretDone(result) => self.secret_done(result),
            Action::VaultLocked(result) => {
                match result {
                    Ok(()) => self.notify("Vault locked: remembered secrets are unreadable".into()),
                    Err(e) => self.notify_error(e),
                }
                self.refresh_vault(false);
            }
            Action::OpenSnippets => self.open_snippets(),
            Action::SnippetsKey(key) => self.snippets_key(key),
            Action::SnippetsLoaded(result) => {
                if let Some(panel) = &mut self.snippets_panel {
                    panel.set(result);
                }
            }
            Action::SnippetSaved(result) => {
                let Some(panel) = &mut self.snippets_panel else {
                    return;
                };
                if let Some(saved) = panel.saved(result) {
                    if let Load::Ready(list) = &mut panel.all {
                        match list.iter_mut().find(|s| s.id == saved.id) {
                            Some(existing) => *existing = saved.clone(),
                            None => list.push(saved.clone()),
                        }
                    }
                    panel.select_id(&saved.id);
                }
            }
            Action::SnippetDeleted { name, result } => {
                let Some(panel) = &mut self.snippets_panel else {
                    return;
                };
                match result {
                    Ok(()) => {
                        panel.notice = Some(Notice::ok(format!("Deleted {name}")));
                        self.reload_snippets();
                    }
                    Err(e) => panel.notice = Some(Notice::error(e)),
                }
            }
            // Phase 4 actions; an unknown vault status leaves the keys
            // panel saying "checking…".
            other => self.update_meta(other),
        }
    }

    // ── keys panel ─────────────────────────────────────────────────────────

    fn open_keys(&mut self) {
        self.prefix = false;
        self.tunnels_panel = None;
        self.snippets_panel = None;
        let mut panel = KeysPanel::new(self.current_alias(), unix_now());
        // A copy-id job in flight (or just finished) is shown again.
        if self.copy_job.is_some() {
            panel.mode = Mode::Copy;
        }
        self.keys_panel = Some(panel);
        self.overlay = Some(Overlay::Keys);
        self.refresh_keys();
        self.refresh_vault(false);
    }

    fn refresh_keys(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::LocalKeys(engine.local_keys().await.map_err(|e| e.summary()))
        });
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::AgentKeys(engine.agent_keys().await.map_err(|e| e.summary()))
        });
    }

    fn refresh_agent(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::AgentKeys(engine.agent_keys().await.map_err(|e| e.summary()))
        });
    }

    fn keys_notice(&mut self, notice: Notice) {
        match &mut self.keys_panel {
            Some(panel) if self.overlay == Some(Overlay::Keys) => panel.notice = Some(notice),
            _ if notice.ok => self.notify(notice.message),
            _ => self.notify_error(ErrorSummary {
                code: "keys".into(),
                message: notice.message,
                hint: notice.hint,
            }),
        }
    }

    fn agent_changed(&mut self, notice: Notice) {
        self.keys_notice(notice);
        self.refresh_agent();
    }

    pub(crate) fn copy_state(&self) -> CopyState {
        match self.copy_job.as_ref().map(|j| &j.step) {
            None => CopyState::None,
            Some(Step::Done(_)) => CopyState::Succeeded,
            Some(Step::Failed(_)) => CopyState::Failed,
            Some(_) => CopyState::Running,
        }
    }

    // One arm per panel outcome.
    #[allow(clippy::too_many_lines)]
    fn keys_key(&mut self, key: KeyEvent) {
        let (vault, copy) = (self.vault, self.copy_state());
        let Some(panel) = &mut self.keys_panel else {
            return;
        };
        match panel.handle_key(key, vault, copy) {
            KeysOutcome::None => {}
            KeysOutcome::Close => {
                self.overlay = None;
                self.keys_panel = None;
            }
            KeysOutcome::Refresh => {
                panel.local = Load::Loading;
                panel.agent = Load::Loading;
                panel.now = unix_now();
                self.refresh_keys();
                self.refresh_vault(false);
            }
            KeysOutcome::NewKey => {
                let taken = taken_names(panel.locals());
                let user = self.engine.config().local_user.clone();
                let machine = self.machine.clone().unwrap_or_else(|| "portico".into());
                let host = host_hint(panel.host.as_deref());
                panel.mode = Mode::Generate(GenForm::new(host, format!("{user}@{machine}"), taken));
            }
            KeysOutcome::Generate(request) => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    Action::KeyGenerated(
                        engine.generate_key(request).await.map_err(|e| e.summary()),
                    )
                });
            }
            KeysOutcome::AgentAdd { key, lifetime } => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    let result = engine
                        .agent_add(key.private_path.clone(), None, lifetime)
                        .await
                        .map_err(|e| e.summary());
                    Action::AgentAdded {
                        key,
                        lifetime,
                        result,
                    }
                });
            }
            KeysOutcome::AgentRemove { fingerprint, label } => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    let result = engine
                        .agent_remove(&fingerprint)
                        .await
                        .map_err(|e| e.summary());
                    Action::AgentRemoved { label, result }
                });
            }
            KeysOutcome::PickHost(key) => {
                let hosts = self
                    .inventory
                    .as_ref()
                    .map(|i| i.hosts.clone())
                    .unwrap_or_default();
                if let Some(panel) = &mut self.keys_panel {
                    panel.mode = Mode::PickHost(HostPicker::new(key, hosts));
                }
            }
            KeysOutcome::CopyId { key, alias } => self.start_copy(&key, alias),
            KeysOutcome::CopyPublic(key) => {
                let path = key.public_path;
                spawn(self, async move {
                    let result = tokio::task::spawn_blocking(move || read_public_line(&path))
                        .await
                        .unwrap_or_else(|_| {
                            Err(ErrorSummary {
                                code: "engine_closed".into(),
                                message: "could not read the public key".into(),
                                hint: None,
                            })
                        });
                    Action::PublicKeyRead(result)
                });
            }
            KeysOutcome::VaultToggle => match vault {
                Some(VaultStatus::Locked) => self.dialogs.push_back(Dialog::Secret(
                    SecretDialog::new(SecretPurpose::VaultUnlock { startup: false }),
                )),
                Some(VaultStatus::Unlocked) => {
                    let engine = self.engine.clone();
                    spawn(self, async move {
                        Action::VaultLocked(engine.vault_lock().await.map_err(|e| e.summary()))
                    });
                }
                _ => {}
            },
            KeysOutcome::VaultPassphrase => {
                let purpose = match vault {
                    Some(VaultStatus::Absent) => SecretPurpose::VaultCreate { offered: false },
                    Some(VaultStatus::Unlocked) => SecretPurpose::VaultChange,
                    _ => return,
                };
                self.dialogs
                    .push_back(Dialog::Secret(SecretDialog::new(purpose)));
            }
            KeysOutcome::CopyRetry => {
                if let Some(job) = self.copy_job.clone() {
                    let key = LocalKey {
                        private_path: job.private_path,
                        public_path: job.public_path,
                        algorithm: String::new(),
                        fingerprint: String::new(),
                        comment: String::new(),
                        // A `.pub` alone is still not verified on retry.
                        has_private: job.has_private,
                        certificate: None,
                    };
                    self.start_copy(&key, job.alias);
                }
            }
            KeysOutcome::CopyLeave => {
                if !self.copy_job.as_ref().is_some_and(CopyJob::running) {
                    self.copy_job = None;
                }
            }
        }
    }

    fn key_generated(&mut self, result: Result<LocalKey, ErrorSummary>) {
        let Some(panel) = &mut self.keys_panel else {
            return;
        };
        match result {
            Ok(key) => {
                let name = file_name(&key.private_path);
                if let Load::Ready(list) = &mut panel.local {
                    list.push(key.clone());
                    list.sort_by(|a, b| a.public_path.cmp(&b.public_path));
                }
                panel.mode = Mode::Browse;
                panel.select_local(&key.private_path);
                panel.notice = Some(Notice {
                    ok: true,
                    message: format!("Generated ~/.ssh/{name}"),
                    hint: Some("a adds it to the agent · c installs it on a host".into()),
                });
            }
            Err(error) => {
                if let Mode::Generate(form) = &mut panel.mode {
                    form.refused(error);
                } else {
                    panel.notice = Some(Notice::error(error));
                }
            }
        }
    }

    /// `y`: OSC 52 to the local terminal, and the line in the panel (the
    /// terminal may ignore OSC 52).
    fn copy_public(&mut self, line: String) {
        if self.clipboard {
            let sequence = format!("\x1b]52;c;{}\x07", base64(line.as_bytes()));
            let mut out = std::io::stdout();
            let _ = out.write_all(sequence.as_bytes());
            let _ = out.flush();
        }
        if let Some(panel) = &mut self.keys_panel {
            panel.notice = Some(Notice {
                ok: true,
                message: "Public key copied to the clipboard (OSC 52)".into(),
                hint: Some(
                    "If your terminal does not support it, select the line below. Esc hides it."
                        .into(),
                ),
            });
            panel.public_line = Some(line);
        }
    }

    // ── copy-id ────────────────────────────────────────────────────────────

    fn start_copy(&mut self, key: &LocalKey, alias: String) {
        self.copy_generation = self.copy_generation.wrapping_add(1);
        let (session, opened, phase) = match self.session_for(&alias).filter(|s| s.usable()) {
            Some(s) => (s.id, false, s.phase.clone()),
            None => match self.engine.connect(ConnectRequest::new(alias.clone())) {
                Ok(id) => {
                    self.sessions.insert(
                        alias.clone(),
                        SessionView {
                            id,
                            phase: SessionPhase::Resolving,
                            latency: None,
                        },
                    );
                    (id, true, SessionPhase::Resolving)
                }
                Err(e) => {
                    self.keys_notice(Notice::error(e.summary()));
                    if let Some(panel) = &mut self.keys_panel {
                        panel.mode = Mode::Browse;
                    }
                    return;
                }
            },
        };
        let (job, effect) = CopyJob::start(
            file_name(&key.private_path),
            key.public_path.clone(),
            key.private_path.clone(),
            key.has_private,
            alias,
            session,
            opened,
            &phase,
            self.copy_generation,
        );
        self.copy_job = Some(job);
        if let Some(panel) = &mut self.keys_panel {
            panel.mode = Mode::Copy;
        }
        self.run_copy_effect(effect);
    }

    pub(crate) fn copy_job_phase(&mut self, session: SessionId, phase: &SessionPhase) {
        let effect = self
            .copy_job
            .as_mut()
            .map_or(Effect::None, |j| j.on_phase(session, phase));
        self.run_copy_effect(effect);
        self.copy_finished_elsewhere();
    }

    /// The copy-id view is on screen and follows this session.
    pub(crate) fn copy_view_shows(&self, session: SessionId) -> bool {
        self.overlay == Some(Overlay::Keys)
            && self
                .keys_panel
                .as_ref()
                .is_some_and(|p| matches!(p.mode, Mode::Copy))
            && self.copy_job.as_ref().is_some_and(|j| j.session == session)
    }

    fn run_copy_effect(&mut self, effect: Effect) {
        let Some(job) = &self.copy_job else { return };
        let generation = job.generation;
        let engine = self.engine.clone();
        match effect {
            Effect::None => {}
            Effect::Install(session) => {
                let public = job.public_path.clone();
                spawn(self, async move {
                    let result = engine
                        .copy_id(session, public)
                        .await
                        .map_err(|e| e.summary());
                    Action::CopyInstalled { generation, result }
                });
            }
            Effect::Verify(session) => {
                let private = job.private_path.clone();
                spawn(self, async move {
                    let result = engine
                        .verify_key_login(session, private)
                        .await
                        .map_err(|e| e.summary());
                    Action::CopyVerified { generation, result }
                });
            }
            Effect::Disconnect(session) => {
                tokio::spawn(async move {
                    let _ = engine.disconnect(session).await;
                });
            }
        }
    }

    /// A job that ended while its view was hidden reports by toast.
    fn copy_finished_elsewhere(&mut self) {
        let Some(job) = &self.copy_job else { return };
        if job.running() || self.copy_view_shows(job.session) {
            return;
        }
        let job = job.clone();
        self.copy_job = None;
        match &job.step {
            Step::Failed(error) => self.notify_error(error.clone()),
            _ => {
                if let Some(message) = job.success_message() {
                    self.notify(message);
                }
            }
        }
    }

    // ── vault and secret dialogs ───────────────────────────────────────────

    /// "Remember" was ticked: without a keyring, that needs an unlocked vault.
    pub(crate) fn offer_vault(&mut self) {
        let purpose = match self.vault {
            Some(VaultStatus::Absent) => SecretPurpose::VaultCreate { offered: true },
            Some(VaultStatus::Locked) => SecretPurpose::VaultUnlock { startup: false },
            _ => return,
        };
        self.dialogs
            .push_back(Dialog::Secret(SecretDialog::new(purpose)));
    }

    pub(crate) fn secret_key(&mut self, key: KeyEvent) {
        let Some(Dialog::Secret(dialog)) = self.dialogs.front_mut() else {
            return;
        };
        match dialog.handle_key(key) {
            SecretOutcome::Pending => {}
            SecretOutcome::Cancel => {
                let purpose = dialog.purpose.clone();
                let host_save = dialog.host_save;
                self.dialogs.pop_front();
                if host_save {
                    self.host_save_vault_done(false);
                    return;
                }
                if let SecretPurpose::VaultUnlock { startup: true } = purpose {
                    self.notify_error(ErrorSummary {
                        code: "vault_locked".into(),
                        message: "Continuing without the vault".into(),
                        hint: Some(
                            "Passwords and passphrases will not be remembered. Unlock it later from the keys panel (i, then v)."
                                .into(),
                        ),
                    });
                }
            }
            SecretOutcome::Submit(answers) => {
                let purpose = dialog.purpose.clone();
                let engine = self.engine.clone();
                let mut answers = answers.into_iter();
                let first = answers.next();
                let second = answers.next();
                spawn(self, async move {
                    let Some(first) = first else {
                        return Action::SecretDone(Ok(()));
                    };
                    let result = match purpose {
                        SecretPurpose::VaultUnlock { .. } => engine.vault_unlock(first).await,
                        SecretPurpose::VaultCreate { .. } => engine.vault_create(first).await,
                        SecretPurpose::VaultChange => match second {
                            Some(new) => engine.vault_change_passphrase(first, new).await,
                            None => Ok(()),
                        },
                        SecretPurpose::AgentPassphrase {
                            private_path,
                            lifetime,
                            ..
                        } => engine
                            .agent_add(private_path, Some(first), lifetime)
                            .await
                            .map(|_| ()),
                    };
                    Action::SecretDone(result.map_err(|e| e.summary()))
                });
            }
        }
    }

    fn secret_done(&mut self, result: Result<(), ErrorSummary>) {
        let Some(Dialog::Secret(dialog)) = self.dialogs.front_mut() else {
            return;
        };
        if let Err(error) = result {
            dialog.fail(error);
            return;
        }
        let purpose = dialog.purpose.clone();
        let host_save = dialog.host_save;
        self.dialogs.pop_front();
        if host_save {
            // The vault is ready: the host is saved again (its own toast).
            self.refresh_vault(false);
            self.host_save_vault_done(true);
            return;
        }
        match purpose {
            SecretPurpose::VaultUnlock { .. } => {
                self.notify("Vault unlocked".into());
                self.refresh_vault(false);
            }
            SecretPurpose::VaultCreate { offered } => {
                self.notify(if offered {
                    "Vault created: secrets you choose to remember from now on are encrypted on this machine".into()
                } else {
                    "Vault created and unlocked".into()
                });
                self.refresh_vault(false);
            }
            SecretPurpose::VaultChange => {
                self.notify("Vault passphrase changed".into());
                self.refresh_vault(false);
            }
            SecretPurpose::AgentPassphrase {
                key_name, lifetime, ..
            } => {
                let label = match lifetime {
                    Some(d) => format!("Added {key_name} to the agent for {}", lifetime_words(d)),
                    None => format!("Added {key_name} to the agent"),
                };
                self.agent_changed(Notice::ok(label));
            }
        }
    }

    // ── snippets ───────────────────────────────────────────────────────────

    fn open_snippets(&mut self) {
        self.prefix = false;
        self.tunnels_panel = None;
        self.keys_panel = None;
        // The tab to type into: the active one, else a live tab of the
        // selected host.
        let live = |t: &&crate::terminal::TerminalView| !t.closed && t.ended.is_none();
        let target = self
            .active_terminal()
            .filter(live)
            .or_else(|| {
                let alias = self.selected_host()?.alias.clone();
                self.views().filter(live).find(|t| t.alias == alias)
            })
            .map(|t| (t.id, t.alias.clone()));
        let (alias, cannot_run) = if let Some((_, alias)) = &target {
            (Some(alias.clone()), None)
        } else {
            let alias = self.current_alias();
            let why = match &alias {
                Some(a) => format!("No open terminal on {a}"),
                None => "No open terminal".to_owned(),
            };
            (alias, Some(why))
        };
        self.snippet_target = target.map(|(id, _)| id);
        let tags = alias
            .as_deref()
            .map(|a| self.tags_of(a))
            .unwrap_or_default();
        self.snippets_panel = Some(SnippetsPanel::new(alias, tags, cannot_run));
        self.overlay = Some(Overlay::Snippets);
        self.reload_snippets();
    }

    fn reload_snippets(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SnippetsLoaded(engine.snippets().await.map_err(|e| e.summary()))
        });
    }

    fn snippets_key(&mut self, key: KeyEvent) {
        let Some(panel) = &mut self.snippets_panel else {
            return;
        };
        match panel.handle_key(key, &self.snippet_values) {
            SnipOutcome::None => {}
            SnipOutcome::Close => {
                self.overlay = None;
                self.snippets_panel = None;
            }
            SnipOutcome::Reload => {
                panel.all = Load::Loading;
                self.reload_snippets();
            }
            SnipOutcome::Send {
                rendered,
                run,
                values,
            } => {
                self.snippet_values.extend(values);
                self.send_snippet(&rendered, run);
            }
            SnipOutcome::Save(snippet) => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    Action::SnippetSaved(
                        engine.save_snippet(snippet).await.map_err(|e| e.summary()),
                    )
                });
            }
            SnipOutcome::Delete { id, name } => {
                let engine = self.engine.clone();
                spawn(self, async move {
                    let result = engine.delete_snippet(&id).await.map_err(|e| e.summary());
                    Action::SnippetDeleted { name, result }
                });
            }
        }
    }

    /// Types a rendered snippet into the target tab, then shows that tab.
    fn send_snippet(&mut self, rendered: &str, run: bool) {
        let live = self.snippet_target.and_then(|id| {
            self.views()
                .find(|t| t.id == id && !t.closed && t.ended.is_none())
        });
        let target = live.and_then(|view| self.pane_of_terminal(view.id));
        let Some(view) = live else {
            if let Some(panel) = &mut self.snippets_panel {
                panel.notice = Some(Notice {
                    ok: false,
                    message: "The terminal tab closed".into(),
                    hint: Some("Open a terminal on the host, then try again.".into()),
                });
            }
            return;
        };
        let bracketed = view.mode().contains(TermMode::BRACKETED_PASTE);
        match snippet_bytes(rendered, run, bracketed) {
            Ok(bytes) => {
                view.send(bytes);
                let id = view.id;
                // Typing into the pane returns it to the live view.
                if let Some(view) = self.views_mut().find(|v| v.id == id) {
                    view.close_search();
                }
                self.overlay = None;
                self.snippets_panel = None;
                // The pane typed into is shown and focused.
                if let Some((tab, pane)) = target {
                    self.show_pane(tab, pane);
                }
            }
            Err(error) => {
                if let Some(panel) = &mut self.snippets_panel {
                    if error.code == "snippet_needs_paste_mode" {
                        panel.mode = SnipMode::NeedsRun {
                            rendered: rendered.to_owned(),
                            error,
                            // Armed later: a held or repeated key that sent
                            // the snippet must not also run it.
                            opened_at: std::time::Instant::now(),
                        };
                    } else {
                        panel.notice = Some(Notice::error(error));
                    }
                }
            }
        }
    }
}

/// `Added id_x to the agent (for 1 hour)`.
fn added_message(key: &LocalKey, lifetime: Option<Duration>) -> String {
    let name = file_name(&key.private_path);
    match lifetime {
        Some(d) => format!("Added {name} to the agent for {}", lifetime_words(d)),
        None => format!("Added {name} to the agent"),
    }
}

fn lifetime_words(d: Duration) -> String {
    crate::keys::duration_words(d.as_secs())
}

/// The public key as one display-safe line.
fn read_public_line(path: &PathBuf) -> Result<String, ErrorSummary> {
    let text = std::fs::read_to_string(path).map_err(|e| ErrorSummary {
        code: "key_public".into(),
        message: format!("cannot read {}: {e}", path.display()),
        hint: Some("Refresh the list (Ctrl-R).".into()),
    })?;
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let line = crate::form::display_safe(line);
    if line.is_empty() || line.len() > MAX_PUBLIC_LINE {
        return Err(ErrorSummary {
            code: "key_public".into(),
            message: format!("{} is not a usable public key", path.display()),
            hint: Some("Choose the .pub file of a key pair.".into()),
        });
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"ssh-ed25519 AAAA"), "c3NoLWVkMjU1MTkgQUFBQQ==");
    }
}

/// Panels rendered at 80×24 (and 110×28), and the event plumbing around
/// copy-id probes.
#[cfg(test)]
mod render_tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use portico_core::events::{Prompt, PromptKind, SessionEvent};
    use portico_core::ids::PromptId;
    use portico_core::keys::CertificateInfo;
    use portico_core::snippets::{Snippet, SnippetScope};
    use portico_core::{AgentKey, CopyIdOutcome, EngineEvent};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::test_app;
    use crate::keys::GenForm;

    fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|f| crate::ui::render(app, f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                    .collect()
            })
            .collect()
    }

    fn has(lines: &[String], needle: &str) -> bool {
        lines.iter().any(|l| l.contains(needle))
    }

    fn check(lines: &[String], needles: &[&str]) {
        let text = lines.join("\n");
        for n in needles {
            assert!(has(lines, n), "`{n}` missing:\n{text}");
        }
    }

    const NOW: u64 = 1_800_000_000;

    fn local(name: &str, fp: &str) -> LocalKey {
        LocalKey {
            private_path: PathBuf::from(format!("/home/me/.ssh/{name}")),
            public_path: PathBuf::from(format!("/home/me/.ssh/{name}.pub")),
            algorithm: "ssh-ed25519".into(),
            fingerprint: format!("SHA256:{fp}AbCdEfGhIjKlMnOpQrStUvWxYz0123456789ab"),
            comment: "me@laptop".into(),
            has_private: true,
            certificate: None,
        }
    }

    fn keys_app() -> App {
        let mut app = test_app();
        let mut work = local("id_ed25519_work", "w");
        work.certificate = Some(CertificateInfo {
            path: "/home/me/.ssh/id_ed25519_work-cert.pub".into(),
            key_id: "me".into(),
            principals: vec!["me".into()],
            valid_after: 0,
            valid_before: NOW + 3 * 86_400 + 60,
            ca_fingerprint: "SHA256:ca".into(),
        });
        let mut old = local("id_rsa_old", "o");
        old.algorithm = "ssh-rsa".into();
        old.has_private = false;
        let mut panel = KeysPanel::new(Some("web".into()), NOW);
        panel.set_local(Ok(vec![local("id_ed25519", "a"), work.clone(), old]));
        panel.set_agent(Ok(vec![AgentKey {
            algorithm: "ssh-ed25519".into(),
            fingerprint: work.fingerprint.clone(),
            comment: String::new(),
            certificate: None,
        }]));
        app.keys_panel = Some(panel);
        app.overlay = Some(Overlay::Keys);
        app.vault = Some(VaultStatus::Locked);
        app
    }

    #[test]
    fn keys_panel_shows_both_sections_and_badges() {
        let mut app = keys_app();
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "LOCAL KEYS",
                "SSH AGENT",
                "id_ed25519_work",
                "in agent",
                "cert · expires in 3 days",
                "public only",
                "RSA",
                "SHA256:",
                "me@laptop",
                "Vault",
                "locked",
                "a  add to agent",
            ],
        );
        // 110×28 too, without panicking or losing the vault line.
        let lines = screen(&mut app, 110, 28);
        check(&lines, &["SSH AGENT", "unlock vault"]);
    }

    #[test]
    fn keys_panel_empty_and_error_states() {
        let mut app = test_app();
        let mut panel = KeysPanel::new(None, NOW);
        panel.set_local(Ok(Vec::new()));
        panel.set_agent(Err(ErrorSummary {
            code: "agent_unavailable".into(),
            message: "no ssh-agent is reachable".into(),
            hint: Some("Start ssh-agent (eval $(ssh-agent)).".into()),
        }));
        app.keys_panel = Some(panel);
        app.overlay = Some(Overlay::Keys);
        app.vault = Some(VaultStatus::NotInUse);
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "No key pairs in ~/.ssh yet.",
                "No ssh-agent is reachable",
                "Start ssh-agent",
                "system keyring",
            ],
        );
        assert!(
            !has(&lines, "unlock vault"),
            "no vault keys without a vault"
        );
    }

    #[test]
    fn generation_form_fits_80x24() {
        let mut app = keys_app();
        if let Some(panel) = &mut app.keys_panel {
            panel.mode = Mode::Generate(GenForm::new(
                "web".into(),
                "me@laptop".into(),
                vec!["id_ed25519_web".into()],
            ));
        }
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "new key pair",
                "id_ed25519_web_2",
                "‹ Ed25519 ›",
                "recommended",
                "Passphrase",
                "Confirm",
                "No passphrase",
                "↵  generate",
            ],
        );
        assert!(!has(&lines, "SSH AGENT"), "the form replaces the list");
    }

    fn job(step: Step) -> CopyJob {
        CopyJob {
            key_name: "id_ed25519_web".into(),
            public_path: "/home/me/.ssh/id_ed25519_web.pub".into(),
            private_path: "/home/me/.ssh/id_ed25519_web".into(),
            has_private: true,
            alias: "web".into(),
            session: SessionId::next(),
            opened: true,
            step,
            generation: 1,
            failed_at: 2,
        }
    }

    #[test]
    fn copy_progress_success_and_failure() {
        let mut app = keys_app();
        if let Some(panel) = &mut app.keys_panel {
            panel.mode = Mode::Copy;
        }
        app.copy_job = Some(job(Step::Verifying(CopyIdOutcome::Added)));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &["✓ Connecting", "✓ Installing", "Verifying", "only…"],
        );

        app.copy_job = Some(job(Step::Done(CopyIdOutcome::Added)));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Installed and verified: you can now log in to web with",
                "IdentityFile ~/.ssh/id_ed25519_web",
                "never edits ~/.ssh/config",
            ],
        );

        app.copy_job = Some(job(Step::Failed(ErrorSummary {
            code: "copy_id_not_accepted".into(),
            message: "the key was installed on web but logging in with it failed".into(),
            hint: Some("The server may ignore ~/.ssh/authorized_keys.".into()),
        })));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "✕ Verifying",
                "logging in with it failed",
                "may ignore",
                "↵ / r  retry",
            ],
        );
    }

    #[tokio::test]
    async fn copy_retry_restarts_the_job() {
        let mut app = keys_app();
        if let Some(panel) = &mut app.keys_panel {
            panel.mode = Mode::Copy;
        }
        app.copy_generation = 1;
        app.copy_job = Some(job(Step::Failed(ErrorSummary {
            code: "x".into(),
            message: "boom".into(),
            hint: None,
        })));
        app.update(Action::KeysKey(KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::NONE,
        )));
        // No host `web` in the (empty) test engine: either a new attempt is
        // under way or the refusal is reported; never the old failure.
        let restarted = app
            .copy_job
            .as_ref()
            .is_some_and(|j| j.generation > 1 && j.step == Step::Connecting);
        let reported = app.keys_panel.as_ref().is_some_and(|p| p.notice.is_some());
        assert!(restarted || reported);
    }

    #[test]
    fn probe_prompts_carry_the_alias_and_changed_keys_always_alert() {
        let mut app = test_app();
        let probe = SessionId::next();
        app.update(Action::Engine(EngineEvent::Session {
            id: probe,
            event: SessionEvent::Started {
                alias: "web".into(),
                probe: true,
            },
        }));
        assert!(app.sessions.is_empty(), "a probe is not the host's session");
        app.update(Action::Engine(EngineEvent::Prompt(Prompt {
            id: PromptId::next(),
            session: Some(probe),
            kind: PromptKind::KeyPassphrase {
                key_path: "/home/me/.ssh/id_ed25519_web".into(),
                attempt: 1,
                attempts: 3,
            },
        })));
        let Some(Dialog::Prompt(p)) = app.dialogs.front() else {
            panic!("the probe's prompt shows");
        };
        assert_eq!(p.alias.as_deref(), Some("web"));
        app.update(Action::Engine(EngineEvent::Session {
            id: probe,
            event: SessionEvent::HostKeyChanged {
                host: "web".into(),
                port: 22,
                algorithm: "ssh-ed25519".into(),
                fingerprint: "SHA256:new".into(),
                expected_fingerprint: "SHA256:old".into(),
                known_hosts_file: "/home/me/.ssh/known_hosts".into(),
                known_hosts_line: 3,
            },
        }));
        assert!(
            app.dialogs
                .iter()
                .any(|d| matches!(d, Dialog::ChangedKey(_)))
        );
    }

    #[test]
    fn vault_unlock_at_startup_and_offer_after_remember() {
        let mut app = test_app();
        app.update(Action::VaultStatus {
            startup: true,
            result: Ok(VaultStatus::Locked),
        });
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Unlock the vault",
                "Esc continues without it",
                "continue without",
            ],
        );
        // The status bar says the same, not "cancel".
        assert!(lines[23].contains("continue without"), "{}", lines[23]);
        // Typing is masked, never shown.
        if let Some(Dialog::Secret(d)) = app.dialogs.front_mut() {
            d.opened_at = std::time::Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now);
        }
        for c in "hunter22".chars() {
            app.update(Action::DialogKey(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        let lines = screen(&mut app, 80, 24);
        assert!(!has(&lines, "hunter22"));
        check(&lines, &["••••••••"]);
        // Esc: continue, and say what that means.
        app.update(Action::DialogKey(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(app.dialogs.is_empty());
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|t| t.message.contains("without the vault"))
        );

        // Remember ticked with no vault: offer to create one.
        let mut app = test_app();
        app.vault = Some(VaultStatus::Absent);
        app.offer_vault();
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Create the vault", "no keyring", "not now"]);
    }

    fn snippets_app(cannot_run: Option<String>) -> App {
        let mut app = test_app();
        let mut panel = SnippetsPanel::new(Some("web".into()), vec!["prod".into()], cannot_run);
        panel.set(Ok(vec![
            Snippet {
                id: "disk".into(),
                name: "Disk usage".into(),
                command: "df -h".into(),
                description: None,
                scope: SnippetScope::Global,
                run: true,
            },
            Snippet {
                id: "logs".into(),
                name: "Tail logs".into(),
                command: "journalctl -u {{unit}}\n  -n 50".into(),
                description: None,
                scope: SnippetScope::Tags(vec!["prod".into()]),
                run: false,
            },
        ]));
        app.snippets_panel = Some(panel);
        app.overlay = Some(Overlay::Snippets);
        app
    }

    #[test]
    fn snippets_list_scope_and_no_terminal() {
        let mut app = snippets_app(Some("No open terminal on web".into()));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Snippets",
                "· web",
                "2 of 2",
                "Disk usage",
                "global",
                "paste only · #prod",
                "+1 line",
                "No open terminal on web",
                "Esc, then ↵ to connect web.",
                "Ctrl-N  new",
            ],
        );
        assert!(!has(&lines, "↵  run"), "run is disabled without a terminal");
    }

    #[test]
    fn snippet_placeholder_form_and_editor() {
        let mut app = snippets_app(None);
        let key = |code| Action::SnippetsKey(KeyEvent::new(code, KeyModifiers::NONE));
        app.update(key(KeyCode::Down));
        app.update(key(KeyCode::Enter));
        for c in "nginx".chars() {
            app.update(key(KeyCode::Char(c)));
        }
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Run Tail logs on web",
                "unit",
                "Will type",
                "journalctl -u nginx",
            ],
        );

        app.update(key(KeyCode::Esc));
        app.update(Action::SnippetsKey(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
        )));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "New snippet",
                "Name",
                "Command",
                "Applies to",
                "‹ Global ›",
                "Run",
                "Ctrl-S  save",
            ],
        );
    }

    /// `n` local keys, none in the agent.
    fn many_keys_app(n: usize) -> App {
        let mut app = keys_app();
        if let Some(panel) = &mut app.keys_panel {
            panel.set_local(Ok((0..n)
                .map(|i| local(&format!("id_key_{i:02}"), &format!("k{i}")))
                .collect()));
        }
        app
    }

    fn keys_press(app: &mut App, code: KeyCode) {
        app.update(Action::KeysKey(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn row_of(lines: &[String], needle: &str) -> Option<usize> {
        lines.iter().position(|l| l.contains(needle))
    }

    #[test]
    fn keys_list_scrolls_keeping_headers_and_counting_hidden_keys() {
        let mut app = many_keys_app(12);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["LOCAL KEYS", "id_key_00", "more"]);
        assert!(has(&lines, "↓ "), "hidden keys are counted");
        // The last key: scrolled, the rest counted above.
        keys_press(&mut app, KeyCode::End);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["id_key_11", "↑ "]);
        assert!(!has(&lines, "id_key_00"));
        // Back to the first: its section header is in view again.
        keys_press(&mut app, KeyCode::Home);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["LOCAL KEYS", "id_key_00"]);
        // The agent section's header shows with its first row.
        keys_press(&mut app, KeyCode::Tab);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["SSH AGENT"]);
        // Everything fits again: back to the top, nothing counted.
        if let Some(panel) = &mut app.keys_panel {
            panel.set_local(Ok(vec![local("id_a", "a")]));
        }
        let lines = screen(&mut app, 110, 28);
        check(&lines, &["LOCAL KEYS", "id_a", "SSH AGENT"]);
        assert!(!has(&lines, "more"));
        assert_eq!(app.keys_panel.as_ref().map(|p| p.list.offset()), Some(0));
    }

    #[test]
    fn inline_questions_keep_the_selected_background() {
        let mut app = keys_app();
        // id_ed25519_work is in the agent: `x` asks inline.
        keys_press(&mut app, KeyCode::Down);
        keys_press(&mut app, KeyCode::Char('x'));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Remove id_ed25519_work from the agent?"]);
        let y = lines
            .iter()
            .position(|l| l.contains("▍") && l.contains("id_ed25519_work"))
            .unwrap();
        let x = lines[y].chars().position(|c| c == '▍').unwrap();
        let s = app.styles();
        let bar = buffer
            .cell((u16::try_from(x).unwrap(), u16::try_from(y).unwrap()))
            .unwrap();
        assert_eq!(Some(bar.bg), s.selected().bg, "row keeps its background");
        assert_eq!(Some(bar.fg), s.muted().fg, "the bar goes muted");
    }

    #[test]
    fn a_notice_never_squeezes_the_list_and_toasts_sit_under_the_panel() {
        let mut app = many_keys_app(12);
        let before = screen(&mut app, 80, 24)
            .iter()
            .filter(|l| l.contains("id_key_"))
            .count();
        if let Some(panel) = &mut app.keys_panel {
            panel.notice = Some(Notice {
                ok: false,
                message: "the agent refused the key".into(),
                hint: Some("Check that ssh-agent is still running.".into()),
            });
        }
        let lines = screen(&mut app, 80, 24);
        let notice = row_of(&lines, "The agent refused the key").unwrap();
        let border = lines.iter().rposition(|l| l.contains('╰')).unwrap();
        // The toast box itself ends in ╰ too: the panel's border is above it.
        let panel_bottom = lines
            .iter()
            .take(notice)
            .rposition(|l| l.contains('╰'))
            .unwrap();
        assert!(panel_bottom < notice && notice < border);
        assert!(notice < 23, "above the status bar");
        let after = lines.iter().filter(|l| l.contains("id_key_")).count();
        assert!(after + 3 >= before, "the list keeps most of its room");

        // A toast over the panel: between it and the status bar.
        let mut app = many_keys_app(12);
        app.notify("Vault locked".into());
        let lines = screen(&mut app, 80, 24);
        let toast = row_of(&lines, "Vault locked").unwrap();
        let panel_bottom = lines
            .iter()
            .take(toast)
            .rposition(|l| l.contains('╰'))
            .unwrap();
        assert!(panel_bottom < toast && toast < 23);
        check(&lines, &["a  add to agent"]);
    }

    #[test]
    fn panel_hints_hide_behind_a_dialog_and_esc_close_only_when_browsing() {
        let mut app = keys_app();
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Esc close", "a  add to agent"]);
        app.dialogs.push_back(Dialog::Secret(SecretDialog::new(
            SecretPurpose::VaultUnlock { startup: false },
        )));
        let lines = screen(&mut app, 80, 24);
        assert!(!has(&lines, "a  add to agent"));
        app.dialogs.clear();
        keys_press(&mut app, KeyCode::Char('n'));
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &["new key pair", "At least 8 characters", "0 / 8 min"],
        );
        assert!(!has(&lines, "Esc close"));
    }

    #[test]
    fn copy_of_a_public_key_alone_skips_verification() {
        let mut app = keys_app();
        if let Some(panel) = &mut app.keys_panel {
            panel.mode = Mode::Copy;
        }
        let mut j = job(Step::Done(CopyIdOutcome::Added));
        j.has_private = false;
        app.copy_job = Some(j);
        let lines = screen(&mut app, 80, 24);
        check(
            &lines,
            &[
                "Copy id_ed25519_web to web",
                "– Verifying",
                "Installed. Verification needs the private key.",
            ],
        );
        assert!(!has(&lines, "IdentityFile"));
        assert!(!has(&lines, "`"), "no literal backticks");
    }

    #[test]
    fn help_lists_the_new_panels() {
        let mut app = test_app();
        app.overlay = Some(Overlay::Help);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Keys · snippets"]);
        // The panels' own keys are on the second page at 80×24.
        app.update(Action::HelpPage);
        let lines = screen(&mut app, 80, 24);
        check(&lines, &["Copy pub · vault", "Run/paste · new"]);
    }
}
