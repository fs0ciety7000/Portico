//! App glue for the encrypted folder sync (phase 11, S1c): the Sync panel,
//! the wizard, the status bar item, the received hosts and snippets.
//!
//! What `docs/SYNC.md` §10 "Ce que S1c attend de S1b" asks of the UIs:
//!
//! - `Engine::start_sync` at launch, `sync_status` for the bar, then the
//!   events: `Sync`, `SyncConflicts`, `SyncPendingChanged` (reload the
//!   received hosts and snippets; until they are back, decisions wait, so
//!   nothing is confirmed from an old screen), `SnippetsChanged`,
//!   `SettingsChanged` (theme and keymaps live, through the same checks as
//!   `settings.toml` at launch, F18). After a lagged bus, everything is
//!   read again.
//! - An operation refused with `sync_folder_timeout` may still finish on
//!   the sync thread: the UI assumes neither outcome, reads everything
//!   again and says so.
//! - `sync_not_pending`: what was confirmed changed since it was shown; the
//!   lists are reloaded and the panel says to review it again.
//! - A snippet run refused with `sync_snippet_unconfirmed` opens its review
//!   (the whole command) instead of a bare error.

use std::time::Instant;

use portico_core::sync::{SyncCreate, SyncJoin, SyncOptions, SyncStatus};
use portico_core::{EngineEvent, ErrorSummary};
use secrecy::ExposeSecret;

use crate::action::Action;
use crate::app::{App, Overlay};
use crate::keys::{Load, Notice};
use crate::sync_panel::{PanelMode, SyncOutcome, SyncPanel, SyncTab};
use crate::sync_wizard::{SyncWizard, WizardOutcome};

/// A panel operation, to word its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SyncOp {
    SyncNow,
    Unlock,
    Change,
    Reset,
    Rename(String),
    Remember(bool),
    Disable,
    Acknowledge,
    Resolve,
    Skip,
    ConfirmHost(String),
    RejectHost(String),
    ConfirmSnippet(String),
}

/// The status bar's words while an operation runs.
fn busy_words(op: &SyncOp) -> &'static str {
    match op {
        SyncOp::SyncNow => "Syncing…",
        SyncOp::Unlock => "Unlocking…",
        SyncOp::Change | SyncOp::Reset => "Deriving the new key…",
        SyncOp::Disable => "Disabling sync…",
        _ => "Saving…",
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

/// How long a copied passphrase stays in the clipboard (A4, S1c review).
pub(crate) const CLIPBOARD_CLEAR: std::time::Duration = std::time::Duration::from_secs(30);

/// This machine's name for a new device (editable in the wizard); without
/// one, `portico-` and four random hex digits, so two such devices never
/// read the same (B15, S1c review).
fn default_device_name(machine: Option<&str>) -> String {
    machine
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map_or_else(
            || {
                use std::hash::{BuildHasher, Hasher};
                // Randomly keyed by the standard library: no extra crate.
                let random = std::collections::hash_map::RandomState::new()
                    .build_hasher()
                    .finish();
                format!("portico-{:04x}", random & 0xffff)
            },
            |m| m.chars().take(64).collect(),
        )
}

impl App {
    /// Sync actions (the last link of the `update` chain).
    #[allow(clippy::too_many_lines)] // flat dispatch table, one arm per action
    pub(crate) fn update_sync(&mut self, action: Action) {
        match action {
            Action::OpenSync => self.open_sync(SyncTab::Overview),
            Action::SyncKey(key) => self.sync_key(key),
            Action::SyncWizardKey(key) => self.sync_wizard_key(key),
            Action::SyncOverview(Ok(overview)) => {
                if let SyncStatus::Idle { conflicts, .. } = &overview.status {
                    self.sync.conflicts = *conflicts;
                }
                self.sync.overview = Some(overview);
            }
            // Unreadable sync state: the bar says nothing, the panel says why.
            Action::SyncOverview(Err(e)) => {
                if let Some(panel) = &mut self.sync_panel {
                    panel.notice = Some(Notice::error(e));
                }
            }
            Action::SyncDevices(result) => {
                if let Some(panel) = &mut self.sync_panel {
                    if let Ok(devices) = &result
                        && let Some(me) = devices.iter().find(|d| d.this_device)
                    {
                        panel.device_name.clone_from(&me.name);
                    }
                    panel.devices = match result {
                        Ok(d) => Load::Ready(d),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            Action::SyncConflicts(result) => {
                if let Ok(list) = &result {
                    self.sync.conflicts = list.len();
                }
                if let Some(panel) = &mut self.sync_panel {
                    panel.set_conflicts(result, Instant::now());
                }
            }
            Action::SyncIssues(result) => {
                if let Some(panel) = &mut self.sync_panel {
                    panel.set_issues(result);
                }
            }
            Action::SyncPending(result) => {
                self.sync.stale_hosts = false;
                // Sync off or unreadable: nothing waits (the panel shows the
                // status, and its own error).
                self.sync
                    .set_pending(result.unwrap_or_default(), Instant::now());
            }
            Action::SyncSnippets(result) => {
                self.sync.stale_snippets = false;
                match result {
                    Ok(list) => self.sync.set_snippets(list, Instant::now()),
                    Err(e) => {
                        if let Some(panel) = &mut self.sync_panel {
                            panel.notice = Some(Notice::error(e));
                        }
                    }
                }
            }
            Action::SyncDone { op, result } => self.sync_done(&op, result),
            Action::SyncClearClipboard(generation) => self.clear_clipboard(generation),
            Action::SyncProbed { folder, result } => {
                if let Some(wizard) = &mut self.sync_wizard {
                    wizard.probed(folder, result);
                }
            }
            Action::SyncCompleted { input, candidates } => {
                if let Some(wizard) = &mut self.sync_wizard {
                    wizard.completed(&input, &candidates);
                }
            }
            Action::SyncGenerated(result) => self.sync_generated(result),
            Action::SyncPreviewed(result) => {
                let Some(wizard) = &mut self.sync_wizard else {
                    return;
                };
                match result {
                    Ok(preview) => wizard.previewed(preview, Instant::now()),
                    Err(e) => {
                        let timeout = e.code == "sync_folder_timeout";
                        wizard.failed(e);
                        if timeout {
                            self.refresh_sync();
                        }
                    }
                }
            }
            Action::SyncSetUp(result) => {
                match result {
                    // The wizard closes onto the panel: "Sync is on".
                    Ok(status) => {
                        if self.sync_wizard.take().is_some() {
                            if let Some(overview) = &mut self.sync.overview {
                                overview.status = status;
                            }
                            self.open_sync(SyncTab::Overview);
                            if let Some(panel) = &mut self.sync_panel {
                                panel.notice = Some(Notice::ok("Sync is on".to_owned()));
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(wizard) = &mut self.sync_wizard {
                            wizard.failed(e);
                        }
                    }
                }
                // Done or not (a timeout may still finish): read it all again.
                self.refresh_sync();
            }
            _ => {}
        }
    }

    // ── startup, events, resync ────────────────────────────────────────────

    /// At launch: the background sync starts (it does nothing while sync
    /// is off), then the status, received hosts and snippets are read.
    pub(crate) fn start_sync(&self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            self.engine.start_sync();
        }
        self.refresh_sync();
    }

    /// Reads everything again: launch, lagged bus, timeout, after an
    /// operation. The panel's own lists too, when it is open.
    pub(crate) fn refresh_sync(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncOverview(engine.sync_status().await.map_err(|e| e.summary()))
        });
        self.reload_received();
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncConflicts(engine.sync_conflicts().await.map_err(|e| e.summary()))
        });
        if self.sync_panel.is_some() {
            self.reload_sync_panel();
        }
    }

    /// Received hosts and snippets waiting for this device.
    fn reload_received(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncPending(engine.sync_pending_hosts().await.map_err(|e| e.summary()))
        });
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncSnippets(
                engine
                    .synced_snippets_unconfirmed()
                    .await
                    .map_err(|e| e.summary()),
            )
        });
    }

    /// Devices and issues (conflicts come with `refresh_sync`).
    fn reload_sync_panel(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncDevices(engine.sync_devices().await.map_err(|e| e.summary()))
        });
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncIssues(engine.sync_issues().await.map_err(|e| e.summary()))
        });
    }

    /// Sync events (from `on_engine_event`).
    pub(crate) fn on_sync_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Sync { status } => {
                if let SyncStatus::Idle { conflicts, .. } = &status {
                    self.sync.conflicts = *conflicts;
                }
                let Some(overview) = &mut self.sync.overview else {
                    self.refresh_sync();
                    return;
                };
                // Set up or turned off elsewhere: folder and device too.
                let was_enabled = !matches!(overview.status, SyncStatus::Disabled);
                let enabled = !matches!(status, SyncStatus::Disabled);
                overview.status = status;
                if was_enabled != enabled {
                    self.refresh_sync();
                    return;
                }
                if self.sync_panel.is_some() {
                    self.reload_sync_panel();
                }
            }
            EngineEvent::SyncConflicts { count } => {
                self.sync.conflicts = count;
                if self.sync_panel.is_some() {
                    let engine = self.engine.clone();
                    spawn(self, async move {
                        Action::SyncConflicts(
                            engine.sync_conflicts().await.map_err(|e| e.summary()),
                        )
                    });
                }
            }
            EngineEvent::SyncPendingChanged => {
                // What is on screen may be old: decisions wait for the reload.
                self.sync.stale_hosts = true;
                self.sync.stale_snippets = true;
                self.reload_received();
            }
            EngineEvent::SnippetsChanged => {
                if self.snippets_panel.is_some() {
                    self.reload_snippets();
                }
                let engine = self.engine.clone();
                spawn(self, async move {
                    Action::SyncSnippets(
                        engine
                            .synced_snippets_unconfirmed()
                            .await
                            .map_err(|e| e.summary()),
                    )
                });
            }
            EngineEvent::SettingsChanged { settings } => {
                self.update(Action::SettingsLoaded(Ok(settings)));
            }
            _ => {}
        }
    }

    // ── panel ──────────────────────────────────────────────────────────────

    fn device_name(&self) -> String {
        default_device_name(self.machine.as_deref())
    }

    /// "Sync": the panel on `tab` (its Overview offers the wizard when
    /// sync is off).
    pub(crate) fn open_sync(&mut self, tab: SyncTab) {
        self.prefix = false;
        self.close_panels();
        self.sync_wizard = None;
        self.sync_panel = Some(Box::new(SyncPanel::new(
            tab,
            self.device_name(),
            Instant::now(),
        )));
        self.overlay = Some(Overlay::Sync);
        self.refresh_sync();
    }

    /// A run of snippet `id` was refused (not confirmed here, F8): its
    /// review, over the snippets panel, which comes back after.
    pub(crate) fn review_snippet(&mut self, id: &str) {
        let mut panel = SyncPanel::new(SyncTab::Received, self.device_name(), Instant::now());
        panel.select_snippet(&self.sync, id);
        panel.from_snippets = self.snippets_panel.is_some();
        self.sync_panel = Some(Box::new(panel));
        self.overlay = Some(Overlay::Sync);
        self.refresh_sync();
    }

    fn close_sync_panel(&mut self) {
        let back = self.sync_panel.as_ref().is_some_and(|p| p.from_snippets)
            && self.snippets_panel.is_some();
        self.sync_panel = None;
        if back {
            self.overlay = Some(Overlay::Snippets);
            self.reload_snippets();
        } else {
            self.overlay = None;
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per outcome
    fn sync_key(&mut self, key: crossterm::event::KeyEvent) {
        let now = Instant::now();
        let Some(panel) = &mut self.sync_panel else {
            return;
        };
        let outcome = panel.handle_key(key, &self.sync, now);
        let engine = self.engine.clone();
        match outcome {
            SyncOutcome::None => {}
            SyncOutcome::Close => self.close_sync_panel(),
            SyncOutcome::Help => self.open_help(),
            SyncOutcome::OpenWizard => self.open_wizard(),
            SyncOutcome::Generate => self.generate_passphrase(),
            SyncOutcome::Copy => {
                let text = panel
                    .form()
                    .and_then(|f| f.generated_text())
                    .map(|s| zeroize::Zeroizing::new(s.expose_secret().to_owned()));
                if let Some(text) = text {
                    self.copy_passphrase(&text);
                }
            }
            SyncOutcome::SyncNow => {
                self.run_op(
                    SyncOp::SyncNow,
                    async move { engine.sync_now().await.map(Some) },
                );
            }
            SyncOutcome::Unlock(passphrase) => self.run_op(SyncOp::Unlock, async move {
                engine.sync_unlock(passphrase).await.map(Some)
            }),
            SyncOutcome::Change { old, new } => self.run_op(SyncOp::Change, async move {
                engine.sync_change_passphrase(old, new).await.map(Some)
            }),
            SyncOutcome::Reset(new) => self.run_op(SyncOp::Reset, async move {
                engine.sync_reset_passphrase(new).await.map(Some)
            }),
            SyncOutcome::Rename(name) => {
                let op = SyncOp::Rename(name.clone());
                self.run_op(op, async move {
                    engine.sync_rename_device(name).await.map(Some)
                });
            }
            SyncOutcome::SetRemember(on) => self.run_op(SyncOp::Remember(on), async move {
                engine
                    .sync_set_options(SyncOptions { remember_key: on })
                    .await
                    .map(Some)
            }),
            SyncOutcome::Disable(options) => self.run_op(SyncOp::Disable, async move {
                engine.sync_disable(options).await.map(Some)
            }),
            SyncOutcome::Acknowledge => self.run_op(SyncOp::Acknowledge, async move {
                engine.sync_acknowledge_alerts().await.map(Some)
            }),
            SyncOutcome::Resolve {
                id,
                resolution,
                sides_hash,
            } => self.run_op(SyncOp::Resolve, async move {
                engine
                    .sync_resolve(id, resolution, sides_hash)
                    .await
                    .map(Some)
            }),
            SyncOutcome::Skip(record) => self.run_op(SyncOp::Skip, async move {
                engine.sync_skip_on_this_device(record).await.map(Some)
            }),
            SyncOutcome::ConfirmHost { alias, hash } => {
                let op = SyncOp::ConfirmHost(alias.clone());
                self.run_op(op, async move {
                    engine
                        .sync_confirm_hosts(vec![(alias, hash)])
                        .await
                        .map(Some)
                });
            }
            SyncOutcome::RejectHost { alias, hash } => {
                let op = SyncOp::RejectHost(alias.clone());
                self.run_op(op, async move {
                    engine
                        .sync_reject_hosts(vec![(alias, hash)])
                        .await
                        .map(Some)
                });
            }
            SyncOutcome::ConfirmSnippet { id, hash, name } => {
                self.run_op(SyncOp::ConfirmSnippet(name), async move {
                    engine.confirm_synced_snippet(id, hash).await.map(|()| None)
                });
            }
        }
    }

    /// Runs a panel operation; its result comes back as `SyncDone`.
    fn run_op<F>(&mut self, op: SyncOp, fut: F)
    where
        F: Future<Output = portico_core::Result<Option<SyncStatus>>> + Send + 'static,
    {
        if let Some(panel) = &mut self.sync_panel {
            panel.busy = Some(busy_words(&op));
            panel.notice = None;
        }
        spawn(self, async move {
            let result = fut.await.map_err(|e| e.summary());
            Action::SyncDone { op, result }
        });
    }

    #[allow(clippy::too_many_lines)] // one message per operation
    fn sync_done(&mut self, op: &SyncOp, result: Result<Option<SyncStatus>, ErrorSummary>) {
        // Whatever happened, read it all again (a timeout may still finish).
        self.refresh_sync();
        if let (Ok(Some(status)), Some(overview)) = (&result, &mut self.sync.overview) {
            overview.status = status.clone();
        }
        let back_to_snippets = matches!(op, SyncOp::ConfirmSnippet(_))
            && result.is_ok()
            && self.sync_panel.as_ref().is_some_and(|p| p.from_snippets);
        let Some(panel) = &mut self.sync_panel else {
            if let Err(e) = result {
                self.notify_error(e);
            }
            return;
        };
        panel.busy = None;
        let error = match result {
            Ok(_) => {
                let message = match op {
                    SyncOp::SyncNow => "Sync done".to_owned(),
                    SyncOp::Unlock => "Sync unlocked".to_owned(),
                    SyncOp::Change => "Passphrase changed. Use it to set up sync on another device.".to_owned(),
                    SyncOp::Reset => {
                        "Passphrase reset. Your other devices ask to acknowledge it.".to_owned()
                    }
                    SyncOp::Rename(name) => {
                        panel.device_name.clone_from(name);
                        format!("This device is now {name}")
                    }
                    SyncOp::Remember(true) => "The key is remembered on this device".to_owned(),
                    SyncOp::Remember(false) => {
                        "The key is no longer remembered: unlock with the passphrase after a restart"
                            .to_owned()
                    }
                    SyncOp::Disable => {
                        "Sync is off on this device. Local hosts, snippets and settings stay."
                            .to_owned()
                    }
                    SyncOp::Acknowledge => "Alert acknowledged".to_owned(),
                    SyncOp::Resolve => "Conflict resolved".to_owned(),
                    SyncOp::Skip => "Skipped on this device".to_owned(),
                    SyncOp::ConfirmHost(alias) => {
                        format!("{alias} confirmed: written at the next sync")
                    }
                    SyncOp::RejectHost(alias) => {
                        format!("{alias} rejected on this device")
                    }
                    SyncOp::ConfirmSnippet(name) => format!("{name} confirmed: it can run now"),
                };
                panel.mode = PanelMode::Browse;
                panel.notice = Some(Notice::ok(message));
                None
            }
            Err(e) => Some(e),
        };
        if back_to_snippets {
            let notice = panel.notice.take();
            self.close_sync_panel();
            if let (Some(snippets), Some(notice)) = (&mut self.snippets_panel, notice) {
                snippets.notice = Some(notice);
            }
            return;
        }
        let Some(e) = error else {
            return;
        };
        match e.code.as_str() {
            "sync_folder_timeout" => {
                panel.notice = Some(Notice {
                    ok: false,
                    message: e.message,
                    hint: Some(
                        "It may still finish in the background: the status is read again. Try again once it settles."
                            .into(),
                    ),
                });
                if let Some(form) = panel.form_mut() {
                    form.busy = false;
                }
            }
            "sync_not_pending" => {
                panel.notice = Some(Notice {
                    ok: false,
                    message: "it changed since it was shown".into(),
                    hint: Some("Review what is on screen now, then decide again.".into()),
                });
            }
            _ => match &mut panel.mode {
                PanelMode::Unlock(form) | PanelMode::Change(form) => form.refused(e),
                PanelMode::Reset { form, confirm } => {
                    *confirm = None;
                    form.refused(e);
                }
                PanelMode::Rename { problem, .. } => *problem = Some(e),
                _ => panel.notice = Some(Notice::error(e)),
            },
        }
    }

    fn generate_passphrase(&self) {
        let engine = self.engine.clone();
        spawn(self, async move {
            Action::SyncGenerated(
                engine
                    .sync_generate_passphrase()
                    .await
                    .map_err(|e| e.summary()),
            )
        });
    }

    /// A generated passphrase for the form on screen (wizard or panel).
    fn sync_generated(
        &mut self,
        result: Result<portico_core::sync::GeneratedPassphrase, ErrorSummary>,
    ) {
        let form = match self.overlay {
            Some(Overlay::SyncWizard) => self.sync_wizard.as_mut().map(|w| &mut w.form),
            Some(Overlay::Sync) => self.sync_panel.as_mut().and_then(|p| p.form_mut()),
            _ => None,
        };
        // The form is gone: the passphrase is dropped (zeroized) here.
        let Some(form) = form else {
            return;
        };
        match result {
            Ok(generated) => form.set_generated(generated),
            Err(e) => form.refused(e),
        }
    }

    /// The generated passphrase to the clipboard (OSC 52), once asked; the
    /// clipboard is emptied [`CLIPBOARD_CLEAR`] later unless Portico copied
    /// something else meanwhile (A4: a terminal cannot be asked what its
    /// clipboard holds, so the copy is tracked by a generation).
    fn copy_passphrase(&mut self, text: &str) {
        // `osc52_secret` moved the generation on: this copy's own.
        self.osc52_secret(text);
        let generation = self.clipboard_secret.get();
        let tx = self.tx.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                tokio::time::sleep(CLIPBOARD_CLEAR).await;
                let _ = tx.send(Action::SyncClearClipboard(generation));
            });
        }
        self.notify_warning(
            "Copied — cleared from the clipboard in 30 s".into(),
            Some("Paste it into your password manager now.".into()),
        );
    }

    /// The copied passphrase's time is up: empty the clipboard, unless
    /// something else was copied since.
    fn clear_clipboard(&mut self, generation: u64) {
        if generation == self.clipboard_secret.get() {
            self.osc52_secret("");
        }
    }

    // ── wizard ─────────────────────────────────────────────────────────────

    fn open_wizard(&mut self) {
        self.sync_panel = None;
        self.sync_wizard = Some(Box::new(SyncWizard::new(self.device_name())));
        self.overlay = Some(Overlay::SyncWizard);
    }

    fn sync_wizard_key(&mut self, key: crossterm::event::KeyEvent) {
        let Some(wizard) = &mut self.sync_wizard else {
            return;
        };
        let engine = self.engine.clone();
        match wizard.handle_key(key, Instant::now()) {
            WizardOutcome::None => {}
            WizardOutcome::Close => {
                self.sync_wizard = None;
                self.overlay = None;
            }
            WizardOutcome::Complete(input) => spawn(self, async move {
                let query = input.clone();
                let candidates =
                    tokio::task::spawn_blocking(move || crate::sync_wizard::complete_dirs(&query))
                        .await
                        .unwrap_or_default();
                Action::SyncCompleted { input, candidates }
            }),
            WizardOutcome::Probe(folder) => {
                let path = crate::sync_wizard::expand_home(folder.trim());
                spawn(self, async move {
                    let result = engine
                        .sync_probe_folder(path)
                        .await
                        .map_err(|e| e.summary());
                    Action::SyncProbed { folder, result }
                });
            }
            WizardOutcome::Generate => self.generate_passphrase(),
            WizardOutcome::Copy => {
                let text = wizard
                    .form
                    .generated_text()
                    .map(|s| zeroize::Zeroizing::new(s.expose_secret().to_owned()));
                if let Some(text) = text {
                    self.copy_passphrase(&text);
                }
            }
            WizardOutcome::Create => {
                let request = SyncCreate {
                    folder: wizard.folder_path(),
                    passphrase: wizard.form.take_new(),
                    device_name: wizard.form.name.trim().to_owned(),
                    remember_key: wizard.form.remember,
                };
                spawn(self, async move {
                    Action::SyncSetUp(engine.sync_create(request).await.map_err(|e| e.summary()))
                });
            }
            WizardOutcome::Preview => {
                let request = SyncJoin {
                    folder: wizard.folder_path(),
                    passphrase: wizard.form.take_old(),
                    device_name: wizard.form.name.trim().to_owned(),
                    remember_key: wizard.form.remember,
                };
                spawn(self, async move {
                    Action::SyncPreviewed(
                        engine
                            .sync_join_preview(request)
                            .await
                            .map_err(|e| e.summary()),
                    )
                });
            }
            WizardOutcome::Commit { token, bulk } => spawn(self, async move {
                Action::SyncSetUp(
                    engine
                        .sync_join_commit(token, bulk)
                        .await
                        .map_err(|e| e.summary()),
                )
            }),
        }
    }

    /// A paste into the sync panel's form or the wizard.
    pub(crate) fn sync_paste(&mut self, text: &str) {
        match self.overlay {
            Some(Overlay::SyncWizard) => {
                if let Some(wizard) = &mut self.sync_wizard {
                    wizard.paste(text);
                }
            }
            Some(Overlay::Sync) => {
                let Some(panel) = &mut self.sync_panel else {
                    return;
                };
                if let Some(form) = panel.form_mut() {
                    form.paste(text);
                } else if let PanelMode::Rename { name, .. } = &mut panel.mode {
                    crate::form::paste_line(name, text, crate::sync_panel::DEVICE_NAME_MAX);
                }
            }
            _ => {}
        }
    }

    /// Something animates in the sync screens (spinner).
    pub(crate) fn sync_busy(&self) -> bool {
        self.sync_panel.as_ref().is_some_and(|p| p.busy())
            || self.sync_wizard.as_ref().is_some_and(|w| w.busy())
    }
}

#[cfg(test)]
mod tests;
