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
//!
//! S2c-1 (`docs/SYNC.md` §13.12 "Ce que S2c-1 (UIs) doit brancher"): the
//! rotation dialogs ([`crate::sync_rotate`]) and their previews, the
//! adoption (the previous passphrase first on a device that does not
//! remember its key, RR4, then the new one), the rotation conflict's
//! "Use the other device's keys", "Restore key file", newer device files
//! (RR5), old keychain entries, held values (Received), "Leave the device
//! list" and "Join again as a new device".

use std::time::Instant;

use portico_core::sync::api::{ForgetDevice, RevokedValueChoice, RotateKeys, RotationChoice};
use portico_core::sync::{SyncCreate, SyncJoin, SyncOptions, SyncStatus};
use portico_core::{EngineEvent, ErrorSummary};
use secrecy::ExposeSecret;

use crate::action::Action;
use crate::app::{App, Overlay};
use crate::keys::{Load, Notice};
use crate::sync_panel::{PanelMode, PassField, SyncOutcome, SyncPanel, SyncTab};
use crate::sync_rotate::{RotateKind, RotateRequest, Rotated};
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
    /// Turn sync off, then the wizard on `folder` (forgotten device).
    DisableJoin(Option<std::path::PathBuf>),
    UseOther,
    Restore,
    Discard(String),
    RemoveKey(String),
    Held(String, RevokedValueChoice),
}

/// The status bar's words while an operation runs.
fn busy_words(op: &SyncOp) -> &'static str {
    match op {
        SyncOp::SyncNow => "Syncing…",
        SyncOp::Unlock => "Unlocking…",
        SyncOp::Change | SyncOp::Reset => "Deriving the new key…",
        SyncOp::Disable | SyncOp::DisableJoin(_) => "Disabling sync…",
        SyncOp::UseOther => "Checking the other device's keys…",
        SyncOp::Restore => "Restoring the key file…",
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

/// Spawns `make(tx)`, which sends its own actions (several reads in a
/// row). Outside a runtime nothing is spawned.
fn spawn_seq<M, F>(app: &App, make: M)
where
    M: FnOnce(tokio::sync::mpsc::UnboundedSender<Action>) -> F,
    F: Future<Output = ()> + Send + 'static,
{
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(make(app.tx.clone()));
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
                // A rotation whose answer was lost: the status tells (A4).
                if let Some(PanelMode::Rotate(flow)) = self.sync_panel.as_mut().map(|p| &mut p.mode)
                {
                    flow.checked(&overview);
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
            Action::SyncHeld(result) => {
                self.sync.stale_held = false;
                self.sync
                    .set_held(result.unwrap_or_default(), Instant::now());
            }
            Action::SyncKeyEntries(result) => {
                if let Some(panel) = &mut self.sync_panel {
                    panel.keys = match result {
                        Ok(list) => Load::Ready(list),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            Action::SyncNewerFiles(result) => {
                if let Some(panel) = &mut self.sync_panel {
                    panel.newer = match result {
                        Ok(list) => Load::Ready(list),
                        Err(e) => Load::Failed(e),
                    };
                    panel.newer_index = panel
                        .newer_index
                        .min(panel.newer_files().len().saturating_sub(1));
                }
            }
            Action::SyncRotationPreview(result) => {
                if let Some(PanelMode::Rotate(flow)) = self.sync_panel.as_mut().map(|p| &mut p.mode)
                {
                    flow.preview = match result {
                        Ok(preview) => Load::Ready(preview),
                        Err(e) => Load::Failed(e),
                    };
                }
            }
            Action::SyncForgetPreview(result) => {
                if let Some(PanelMode::Rotate(flow)) = self.sync_panel.as_mut().map(|p| &mut p.mode)
                {
                    flow.forget = Some(match result {
                        Ok(preview) => Load::Ready(preview),
                        Err(e) => Load::Failed(e),
                    });
                }
            }
            Action::SyncRotated(result) => self.sync_rotated(result),
            Action::SyncAdopted {
                previous_failed,
                result,
            } => self.sync_adopted(previous_failed, result),
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

    /// Received hosts, snippets and held values waiting for this device,
    /// one after the other: the sync thread's queue is short
    /// (`SYNC_QUEUE`), a burst of parallel reads would meet `sync_busy`.
    fn reload_received(&self) {
        let engine = self.engine.clone();
        spawn_seq(self, move |tx| async move {
            let pending = engine.sync_pending_hosts().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncPending(pending));
            let snippets = engine
                .synced_snippets_unconfirmed()
                .await
                .map_err(|e| e.summary());
            let _ = tx.send(Action::SyncSnippets(snippets));
            let held = engine.sync_revoked_values().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncHeld(held));
        });
    }

    /// Devices, issues, old keys and newer files (conflicts come with
    /// `refresh_sync`), one after the other (see [`Self::reload_received`]).
    fn reload_sync_panel(&self) {
        let engine = self.engine.clone();
        spawn_seq(self, move |tx| async move {
            let devices = engine.sync_devices().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncDevices(devices));
            let issues = engine.sync_issues().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncIssues(issues));
            let keys = engine.sync_key_entries().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncKeyEntries(keys));
            let newer = engine.sync_newer_files().await.map_err(|e| e.summary());
            let _ = tx.send(Action::SyncNewerFiles(newer));
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
                let checking = matches!(
                    self.sync_panel.as_ref().map(|p| &p.mode),
                    Some(PanelMode::Rotate(flow)) if flow.step == crate::sync_rotate::RotateStep::Checking
                );
                if was_enabled != enabled || checking {
                    // The epoch comes with the overview (A4).
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
                self.sync.stale_held = true;
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
            SyncOutcome::DisableAndJoin(options) => {
                let folder = self.sync.overview.as_ref().and_then(|o| o.folder.clone());
                self.run_op(SyncOp::DisableJoin(folder), async move {
                    engine.sync_disable(options).await.map(Some)
                });
            }
            SyncOutcome::LoadRotation { forget } => {
                spawn(self, async move {
                    Action::SyncRotationPreview(
                        engine
                            .sync_rotation_preview()
                            .await
                            .map_err(|e| e.summary()),
                    )
                });
                if let Some(id) = forget {
                    let engine = self.engine.clone();
                    spawn(self, async move {
                        Action::SyncForgetPreview(
                            engine
                                .sync_forget_preview(id)
                                .await
                                .map_err(|e| e.summary()),
                        )
                    });
                }
            }
            SyncOutcome::Rotate(request) => {
                // What a lost answer is compared with (A4).
                let epoch = self.sync.overview.as_ref().and_then(|o| o.epoch);
                if let Some(PanelMode::Rotate(flow)) = self.sync_panel.as_mut().map(|p| &mut p.mode)
                {
                    flow.epoch_before = epoch;
                }
                self.rotate(request);
            }
            SyncOutcome::Adopt { previous, new } => spawn(self, async move {
                // RR4: the previous passphrase opens this device's current
                // keys (in memory only), so the adoption keeps the local
                // data's provenance.
                if let Some(previous) = previous
                    && let Err(e) = engine.sync_unlock(previous).await
                {
                    return Action::SyncAdopted {
                        previous_failed: true,
                        result: Err(e.summary()),
                    };
                }
                Action::SyncAdopted {
                    previous_failed: false,
                    result: engine.sync_unlock(new).await.map_err(|e| e.summary()),
                }
            }),
            SyncOutcome::UseOther(passphrase) => self.run_op(SyncOp::UseOther, async move {
                engine
                    .sync_resolve_rotation_conflict(RotationChoice::UseOther(passphrase))
                    .await
                    .map(Some)
            }),
            SyncOutcome::Restore => self.run_op(SyncOp::Restore, async move {
                engine.sync_restore_bundle().await.map(Some)
            }),
            SyncOutcome::DiscardNewer {
                device,
                fingerprint,
                file,
            } => self.run_op(SyncOp::Discard(file), async move {
                engine
                    .sync_discard_newer_file(device, fingerprint)
                    .await
                    .map(Some)
            }),
            SyncOutcome::RemoveKey { account, label } => {
                self.run_op(SyncOp::RemoveKey(label), async move {
                    engine.sync_remove_key_entry(account).await.map(|()| None)
                });
            }
            SyncOutcome::Held {
                key,
                label,
                choice,
                hash,
            } => {
                self.run_op(SyncOp::Held(label, choice), async move {
                    engine.sync_revoked_value(key, choice, hash).await.map(Some)
                });
            }
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
        // A decided held value stays on screen until the reload: nothing
        // more is decided on it meanwhile (A3).
        if matches!(op, SyncOp::Held(..)) {
            self.sync.stale_held = true;
        }
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
                        "The key is no longer remembered, nor the key of a pending rotation: after a restart, Portico asks for the passphrase"
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
                    SyncOp::DisableJoin(_) => {
                        "Sync is off. Join the folder again as a new device.".to_owned()
                    }
                    SyncOp::UseOther => {
                        "The other device's keys are in use on this device".to_owned()
                    }
                    SyncOp::Restore => "The key file was restored from this device".to_owned(),
                    SyncOp::Discard(file) => format!("{file} was removed"),
                    SyncOp::RemoveKey(label) => format!("{label} removed from this keychain"),
                    SyncOp::Held(label, RevokedValueChoice::KeepAsMine) => {
                        format!("{label} kept as this device's value")
                    }
                    SyncOp::Held(label, _) => format!("{label}: the synced value is back"),
                };
                panel.mode = PanelMode::Browse;
                panel.notice = Some(Notice::ok(message));
                None
            }
            Err(e) => Some(e),
        };
        if let (SyncOp::DisableJoin(folder), None) = (op, &error) {
            let notice = panel.notice.take();
            self.open_wizard_at(folder.as_deref());
            if let (Some(wizard), Some(notice)) = (&mut self.sync_wizard, notice) {
                wizard.note = Some(notice.message);
            }
            return;
        }
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
            "sync_not_pending" if matches!(panel.mode, PanelMode::UseOther(_)) => {
                panel.mode = PanelMode::Browse;
                panel.notice = Some(Notice {
                    ok: false,
                    message: "the rotation conflict is over".into(),
                    hint: Some("The status was read again: see what it says now.".into()),
                });
            }
            "sync_not_pending" => {
                panel.notice = Some(Notice {
                    ok: false,
                    message: "it changed since it was shown".into(),
                    hint: Some("Review what is on screen now, then decide again.".into()),
                });
            }
            _ => match &mut panel.mode {
                PanelMode::Unlock(form) | PanelMode::Change(form) | PanelMode::UseOther(form) => {
                    form.refused(e);
                }
                PanelMode::Reset { form, confirm } => {
                    *confirm = None;
                    form.refused(e);
                }
                PanelMode::Rename { problem, .. } => *problem = Some(e),
                _ => panel.notice = Some(Notice::error(e)),
            },
        }
    }

    /// Runs a rotation dialog's request; the answer comes back as
    /// `SyncRotated`.
    fn rotate(&mut self, request: RotateRequest) {
        let engine = self.engine.clone();
        spawn(self, async move {
            let RotateRequest {
                kind,
                authorization,
                new_passphrase,
                remember_key,
            } = request;
            let with_outcome = |o: portico_core::sync::api::RotationOutcome| Rotated {
                status: o.status.clone(),
                outcome: Some(o),
            };
            let result = match kind {
                RotateKind::Rotate => engine
                    .sync_rotate_keys(RotateKeys {
                        authorization,
                        new_passphrase,
                        remember_key,
                    })
                    .await
                    .map(with_outcome),
                RotateKind::Forget { id, .. } => engine
                    .sync_forget_device(ForgetDevice {
                        device_ids: vec![id],
                        authorization,
                        new_passphrase,
                        remember_key,
                    })
                    .await
                    .map(with_outcome),
                RotateKind::KeepMine => engine
                    .sync_resolve_rotation_conflict(RotationChoice::KeepMine {
                        new_passphrase,
                        remember_key,
                    })
                    .await
                    .map(|status| Rotated {
                        status,
                        outcome: None,
                    }),
            };
            Action::SyncRotated(result.map_err(|e| e.summary()))
        });
    }

    fn sync_rotated(&mut self, result: Result<Rotated, ErrorSummary>) {
        // Done or not (a timeout may still finish): read it all again.
        self.refresh_sync();
        if let (Ok(rotated), Some(overview)) = (&result, &mut self.sync.overview) {
            overview.status = rotated.status.clone();
        }
        if let Some(PanelMode::Rotate(flow)) = self.sync_panel.as_mut().map(|p| &mut p.mode) {
            flow.finished(result);
        } else if let Err(e) = result {
            self.notify_error(e);
        }
    }

    fn sync_adopted(&mut self, previous_failed: bool, result: Result<SyncStatus, ErrorSummary>) {
        self.refresh_sync();
        if let (Ok(status), Some(overview)) = (&result, &mut self.sync.overview) {
            overview.status = status.clone();
        }
        let Some(panel) = &mut self.sync_panel else {
            if let Err(e) = result {
                self.notify_error(e);
            }
            return;
        };
        let e = match result {
            Ok(status) => {
                panel.mode = PanelMode::Browse;
                panel.notice = Some(Notice::ok(
                    if matches!(status, SyncStatus::Alert { .. } | SyncStatus::Locked) {
                        "The passphrase was taken: see what the status says now"
                    } else {
                        "The new keys are in use on this device"
                    },
                ));
                return;
            }
            Err(e) => e,
        };
        match (e.code.as_str(), &mut panel.mode) {
            ("sync_wrong_passphrase", PanelMode::Adopt(form)) if previous_failed => {
                form.refused_at(
                    PassField::Previous,
                    ErrorSummary {
                        hint: Some(
                            "If Portico has not restarted since the keys changed, leave it empty."
                                .into(),
                        ),
                        ..e
                    },
                );
            }
            // Nothing to type here: the status says what to do.
            ("sync_device_forgotten" | "sync_lineage_unknown" | "sync_rotation_conflict", _) => {
                panel.mode = PanelMode::Browse;
                panel.notice = Some(Notice::error(e));
            }
            // The adoption's own hint, by code (B3).
            (_, PanelMode::Adopt(form)) => form.refused_at(
                PassField::Old,
                crate::sync_panel::with_reused_hint(e, crate::sync_panel::REUSED_ADOPTION),
            ),
            _ => panel.notice = Some(Notice::error(e)),
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

    /// "Join again as a new device": the wizard on the folder step, the
    /// folder this device used filled in (`↵` looks at it).
    fn open_wizard_at(&mut self, folder: Option<&std::path::Path>) {
        self.open_wizard();
        if let (Some(wizard), Some(folder)) = (&mut self.sync_wizard, folder) {
            wizard.at_folder(&folder.to_string_lossy());
        }
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
