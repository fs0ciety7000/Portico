//! The folder sync in the engine (phase 11, S1b, `docs/SYNC.md` §5.2–5.4).
//!
//! - **Thread**: the sync state machine ([`SyncCore`]) lives on one
//!   dedicated thread, `portico-sync`, fed by a bounded queue
//!   ([`SYNC_QUEUE`]): folder I/O can block for a long time (cloud
//!   hydration, a stalled network mount), so it never takes a runtime or
//!   `spawn_blocking` slot. Each operation is awaited with a timeout
//!   (60 s): past it the status becomes `FolderUnavailable { NotMounted }`
//!   and the operation finishes in the background. Argon2 (create, join,
//!   unlock, passphrase change) runs on `spawn_blocking`.
//! - **Task**: [`Engine::start_sync`] starts the polling task under a child
//!   of the engine's cancellation token: an immediate pass, then one every
//!   30 s, and one 5 s after the last local change ([`Engine::sync_touch`],
//!   called by every engine command that edits hosts, metadata, snippets
//!   or settings; hand edits are caught by the polling). On shutdown, a
//!   last pass publishes pending local changes.
//! - **Events**: [`EngineEvent::Sync`], [`EngineEvent::SyncConflicts`],
//!   [`EngineEvent::SyncPendingChanged`] (what awaits a confirmation
//!   changed, even at the same count), [`EngineEvent::SnippetsChanged`],
//!   [`EngineEvent::SettingsChanged`], plus `HostsChanged` / `HostMeta`
//!   after an application.
//! - **Timeout**: after `sync_folder_timeout` the operation may still
//!   complete in the background: UIs re-read the status, the pending
//!   confirmations and the conflicts (`docs/SYNC.md` §10).
//! - **S2a** (`docs/SYNC.md` §13.7): key rotation and forgetting a device
//!   ([`Engine::sync_rotate_keys`], [`Engine::sync_forget_device`]), the
//!   extended unlock that adopts another device's rotation, the rotation
//!   conflict choices, the explicit ways out (`sync_restore_bundle`,
//!   `sync_discard_newer_file`), old keychain entries, held values. The
//!   Argon2 work (new slot, history of slots) runs on `spawn_blocking`
//!   between two steps on the sync thread; the second refuses
//!   (`sync_busy`) when `bundle.toml` changed meanwhile.
//! - No network call, ever.

use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::SecretString;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use super::Engine;
use crate::error::{Error, Result};
use crate::events::EngineEvent;
use crate::sync::api::{
    DisableOptions, FolderProbe, FolderReason, ForgetDevice, ForgetPreview, JoinBulkChoice,
    JoinPreview, NewerEpochFile, PendingHostChange, RevokedValueChoice, RevokedValueItem,
    RotateAuth, RotateKeys, RotationChoice, RotationOutcome, RotationPreview, SyncConflict,
    SyncCreate, SyncDevice, SyncIssue, SyncJoin, SyncKeyEntry, SyncOptions, SyncOverview,
    SyncResolution, SyncStatus, UnconfirmedSnippet,
};
use crate::sync::bundle::{self, RosterDevice};
use crate::sync::clock::{DeviceId, DeviceName};
use crate::sync::passphrase::{GeneratedPassphrase, NewPassphrase};
use crate::sync::runner::{
    Authorization, SyncCore, SyncTuning, build_rotation, conflict_decide, unlock_decide,
};
use crate::sync::{SyncError, state};

/// Operations queued for the sync thread at most (beyond: `sync_busy`).
pub(crate) const SYNC_QUEUE: usize = 8;

type Job = Box<dyn FnOnce(&mut SyncCore) + Send>;

/// The engine's side of the sync thread.
#[derive(Debug)]
pub(crate) struct SyncHandle {
    jobs: Mutex<Option<SyncSender<Job>>>,
    tuning: Mutex<SyncTuning>,
    status: Arc<Mutex<SyncStatus>>,
    writers: crate::sync::runner::SnippetWriters,
    notify: Arc<Notify>,
    task: Mutex<Option<CancellationToken>>,
}

impl Default for SyncHandle {
    fn default() -> Self {
        Self {
            jobs: Mutex::new(None),
            tuning: Mutex::new(SyncTuning::default()),
            status: Arc::new(Mutex::new(SyncStatus::Disabled)),
            writers: Arc::default(),
            notify: Arc::new(Notify::new()),
            task: Mutex::new(None),
        }
    }
}

fn closed<T>(_: T) -> Error {
    Error::EngineClosed
}

impl Engine {
    fn sync_handle(&self) -> &SyncHandle {
        &self.inner.sync
    }

    fn sync_tuning(&self) -> SyncTuning {
        self.sync_handle()
            .tuning
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Test knobs: KDF bounds, the in-memory provider, short delays. Only
    /// before the first sync operation.
    #[cfg(test)]
    pub(crate) fn set_sync_tuning(&self, tuning: SyncTuning) {
        *self
            .sync_handle()
            .tuning
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = tuning;
    }

    /// The queue of the sync thread, started on first use.
    fn sync_jobs(&self) -> Result<SyncSender<Job>> {
        let mut slot = self
            .sync_handle()
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(jobs) = slot.as_ref() {
            return Ok(jobs.clone());
        }
        let mut core = SyncCore::new(
            self.inner.config.clone(),
            Arc::clone(&self.inner.snippets),
            self.inner.events.clone(),
            self.sync_tuning(),
            Arc::clone(&self.sync_handle().status),
            Arc::clone(&self.sync_handle().writers),
        );
        let (jobs, queue) = std::sync::mpsc::sync_channel::<Job>(SYNC_QUEUE);
        std::thread::Builder::new()
            .name("portico-sync".to_owned())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    job(&mut core);
                }
            })
            .map_err(|e| Error::io(self.inner.config.paths.sync_dir(), e))?;
        *slot = Some(jobs.clone());
        Ok(jobs)
    }

    /// Runs `op` on the sync thread, waiting at most the operation timeout.
    async fn on_sync<T, F>(&self, op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut SyncCore) -> Result<T> + Send + 'static,
    {
        let jobs = self.sync_jobs()?;
        let timeout = self.sync_tuning().op_timeout;
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |core| {
            // An expired join token (and its data key) never outlives the
            // next operation, whatever it is.
            core.purge_expired_joins();
            let _ = reply.send(op(core));
        });
        jobs.try_send(job).map_err(|e| match e {
            TrySendError::Full(_) => Error::from(SyncError::Busy),
            TrySendError::Disconnected(_) => Error::EngineClosed,
        })?;
        let Ok(result) = tokio::time::timeout(timeout, answer).await else {
            let status = SyncStatus::FolderUnavailable {
                reason: FolderReason::NotMounted,
            };
            *self
                .sync_handle()
                .status
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = status.clone();
            self.emit(EngineEvent::Sync { status });
            return Err(SyncError::FolderTimeout {
                secs: timeout.as_secs(),
            }
            .into());
        };
        result.map_err(closed)?
    }

    async fn blocking<T, F>(op: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        tokio::task::spawn_blocking(op).await.map_err(closed)?
    }

    // ── Background task ─────────────────────────────────────────────────

    /// Starts the background sync (called by each UI at launch, and after
    /// create / join): a pass now, then every 30 s and 5 s after a local
    /// change. Nothing happens while sync is not set up on this device.
    /// Needs a Tokio runtime.
    pub fn start_sync(&self) {
        if !self.sync_tuning().background {
            return;
        }
        let token = {
            let mut task = self
                .sync_handle()
                .task
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if task.as_ref().is_some_and(|t| !t.is_cancelled())
                || self.inner.shutdown.is_cancelled()
            {
                return;
            }
            let token = self.inner.shutdown.child_token();
            *task = Some(token.clone());
            token
        };
        let engine = self.clone();
        self.inner
            .tasks
            .spawn(async move { engine.sync_loop(token).await });
    }

    /// A local change was made (hosts, metadata, snippets, settings): the
    /// background sync publishes it after the debounce delay.
    pub(crate) fn sync_touch(&self) {
        self.sync_handle().notify.notify_one();
    }

    async fn sync_loop(&self, token: CancellationToken) {
        let tuning = self.sync_tuning();
        let notify = Arc::clone(&self.sync_handle().notify);
        let mut deadline = tokio::time::Instant::now();
        let mut first_change: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                biased;
                () = token.cancelled() => {
                    if first_change.is_some() && self.sync_is_set_up().await {
                        let _ = self.on_sync(SyncCore::pass).await;
                    }
                    break;
                }
                () = notify.notified() => {
                    let now = tokio::time::Instant::now();
                    let first = *first_change.get_or_insert(now);
                    let latest = first.checked_add(tuning.poll).unwrap_or(now);
                    deadline = now.checked_add(tuning.debounce).unwrap_or(now).min(latest);
                    continue;
                }
                () = tokio::time::sleep_until(deadline) => {}
            }
            let _ = self.on_sync(SyncCore::background_pass).await;
            first_change = None;
            let now = tokio::time::Instant::now();
            deadline = now.checked_add(tuning.poll).unwrap_or(now);
        }
    }

    /// `sync.toml` exists (a file stat, off the runtime threads).
    async fn sync_is_set_up(&self) -> bool {
        let paths = self.inner.config.paths.clone();
        Self::blocking(move || Ok(state::is_set_up(&paths)))
            .await
            .unwrap_or(false)
    }

    // ── Status and reports ──────────────────────────────────────────────

    /// The sync status (no folder access), with the folder and this
    /// device. After a lagged event bus, UIs call this again.
    pub async fn sync_status(&self) -> Result<SyncOverview> {
        let paths = self.inner.config.paths.clone();
        let config = Self::blocking(move || state::load_config(&paths)).await?;
        let stored = self
            .sync_handle()
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let status = match (&config, stored) {
            (None, _) => SyncStatus::Disabled,
            // Set up, no pass yet in this process.
            (Some(_), SyncStatus::Disabled) => SyncStatus::Syncing,
            (Some(_), status) => status,
        };
        Ok(SyncOverview {
            status,
            folder: config.as_ref().map(|c| c.folder.clone()),
            device: config.as_ref().map(|c| c.device.to_string()),
            remember_key: config.as_ref().is_none_or(|c| c.remember_key),
            epoch: config.as_ref().map(|c| c.epoch),
        })
    }

    /// One pass now ("Sync now").
    pub async fn sync_now(&self) -> Result<SyncStatus> {
        self.on_sync(SyncCore::pass).await
    }

    /// The conflicts found by the last pass.
    pub async fn sync_conflicts(&self) -> Result<Vec<SyncConflict>> {
        self.on_sync(|core| Ok(core.conflicts())).await
    }

    /// Issues and alerts of the last pass (shadowed alias, missing jump
    /// host or key, quarantined record, newer-Portico records, rollback…).
    pub async fn sync_issues(&self) -> Result<Vec<SyncIssue>> {
        self.on_sync(|core| Ok(core.issues())).await
    }

    /// Received hosts waiting for a confirmation on this device (F1).
    /// [`EngineEvent::SyncPendingChanged`] says when to call it again.
    pub async fn sync_pending_hosts(&self) -> Result<Vec<PendingHostChange>> {
        self.on_sync(|core| Ok(core.pending())).await
    }

    /// The devices of the bundle (names always with their id prefix).
    pub async fn sync_devices(&self) -> Result<Vec<SyncDevice>> {
        self.on_sync(|core| Ok(core.devices())).await
    }

    // ── Set-up ──────────────────────────────────────────────────────────

    /// What a folder holds (no write): usable, an existing sync, refused…
    pub async fn sync_probe_folder(&self, folder: std::path::PathBuf) -> Result<FolderProbe> {
        self.on_sync(move |core| Ok(core.probe(&folder))).await
    }

    /// A fresh 6-word passphrase (EFF list) for the wizard.
    pub async fn sync_generate_passphrase(&self) -> Result<GeneratedPassphrase> {
        Self::blocking(crate::sync::generate_passphrase).await
    }

    /// Creates a sync in `request.folder` (first device): checks the
    /// folder, derives the key (Argon2, `spawn_blocking`), remembers the
    /// data key when asked, writes `portico-sync/`, then a first pass.
    pub async fn sync_create(&self, request: SyncCreate) -> Result<SyncStatus> {
        let SyncCreate {
            folder,
            passphrase,
            device_name,
            remember_key,
        } = request;
        let name = DeviceName::parse(&device_name)?;
        let probe_folder = folder.clone();
        let root = self
            .on_sync(move |core| core.root_for_create(&probe_folder))
            .await?;
        let device = DeviceId::generate()?;
        let tuning = self.sync_tuning();
        let (text, opened) = Self::blocking(move || {
            let first = RosterDevice {
                id: device,
                name,
                added_at: time::OffsetDateTime::now_utc().unix_timestamp(),
                revoked: false,
            };
            Ok(bundle::create(
                &passphrase,
                first,
                tuning.kdf_params,
                &tuning.kdf_bounds,
            )?)
        })
        .await?;
        let status = self
            .on_sync(move |core| {
                core.finish_create(folder, &root, device, &text, &opened, remember_key)
            })
            .await?;
        self.start_sync();
        Ok(status)
    }

    /// Opens the sync of `request.folder` with the passphrase and reports
    /// what joining would bring. Writes nothing; the key is kept under a
    /// single-use token that expires (F14).
    pub async fn sync_join_preview(&self, request: SyncJoin) -> Result<JoinPreview> {
        let SyncJoin {
            folder,
            passphrase,
            device_name,
            remember_key,
        } = request;
        let name = DeviceName::parse(&device_name)?;
        let read_folder = folder.clone();
        let (root, text) = self
            .on_sync(move |core| core.read_bundle_text(&read_folder))
            .await?;
        let bounds = self.sync_tuning().kdf_bounds;
        let paths = self.inner.config.paths.clone();
        let opened = Self::blocking(move || {
            let opened = bundle::open(&text, &passphrase, &bounds)?;
            // A device that joins again keeps refusing its old passphrases
            // (§13.3.6): every verified slot of this bundle but the one
            // being joined.
            let current = bundle::peek(&text)?.slot_id;
            let history = state::load_history(&paths).unwrap_or_default();
            let reused = history.into_iter().any(|(_, _, old)| {
                bundle::peek(&old).is_ok_and(|clear| {
                    clear.bundle_id == opened.header.bundle_id && clear.slot_id != current
                }) && bundle::passphrase_opens(&old, &passphrase, &bounds)
            });
            if reused {
                return Err(SyncError::PassphraseReused.into());
            }
            Ok(opened)
        })
        .await?;
        self.on_sync(move |core| core.join_preview(folder, root, &opened, name, remember_key))
            .await
    }

    /// Joins with a token from [`Engine::sync_join_preview`] (single use).
    /// `PreferRemote` / `PreferLocal` settle conflicts as explicit
    /// resolutions; received hosts still wait for a confirmation (F1).
    pub async fn sync_join_commit(
        &self,
        token: String,
        bulk: JoinBulkChoice,
    ) -> Result<SyncStatus> {
        let status = self
            .on_sync(move |core| core.join_commit(&token, bulk))
            .await?;
        self.start_sync();
        Ok(status)
    }

    /// Unlocks sync on this device with the passphrase (the key was not
    /// remembered, or the keychain was unavailable), **or adopts** another
    /// device's key rotation (S2a, §13.3.5): the new bundle must continue
    /// this device's lineage (checked against its retained commitment), the
    /// passphrase must open none of the slots this device verified before
    /// (`sync_passphrase_reused`), and nothing below the local epoch floor
    /// is ever opened (`sync_rollback`). A forgotten device gets
    /// `sync_device_forgotten` and nothing is written. On a device that does
    /// not remember its key, the **previous** passphrase is accepted first
    /// (RR4): it unlocks the current keys in memory so the adoption that
    /// follows keeps the local data's provenance.
    pub async fn sync_unlock(&self, passphrase: SecretString) -> Result<SyncStatus> {
        let context = self.on_sync(SyncCore::unlock_context).await?;
        let bounds = self.sync_tuning().kdf_bounds;
        let decide = context.clone();
        let unlocked =
            Self::blocking(move || Ok(unlock_decide(&decide, &passphrase, &bounds)?)).await?;
        let status = self
            .on_sync(move |core| core.finish_unlock(&context, unlocked))
            .await?;
        self.start_sync();
        Ok(status)
    }

    // ── Decisions ───────────────────────────────────────────────────────

    /// Resolves a conflict (a new write that covers every side). The
    /// decision names the sides shown ([`SyncConflict::sides_hash`]):
    /// sides changed since are refused with `sync_not_pending` and nothing
    /// is recorded (the UI reloads the conflicts and shows them again).
    pub async fn sync_resolve(
        &self,
        id: String,
        resolution: SyncResolution,
        sides_hash: String,
    ) -> Result<SyncStatus> {
        self.on_sync(move |core| core.resolve(&id, &resolution, &sides_hash))
            .await
    }

    /// Confirms received host destinations on this device (F1): they are
    /// written to `hosts.conf` by the pass that follows. Each decision is
    /// `(alias, destination_hash)` as shown
    /// ([`PendingHostChange::destination_hash`]): a destination that
    /// changed since is refused with `sync_not_pending` and nothing is
    /// recorded. A confirmation is used once: written, or overtaken by a
    /// local edit of the host, it is forgotten.
    pub async fn sync_confirm_hosts(&self, decisions: Vec<(String, String)>) -> Result<SyncStatus> {
        self.on_sync(move |core| core.decide_hosts(&decisions, true))
            .await
    }

    /// Refuses received host destinations on this device (same
    /// `(alias, destination_hash)` decisions): kept out of `hosts.conf`
    /// until another destination arrives.
    pub async fn sync_reject_hosts(&self, decisions: Vec<(String, String)>) -> Result<SyncStatus> {
        self.on_sync(move |core| core.decide_hosts(&decisions, false))
            .await
    }

    /// "Skip on this device" for an issue's record (`SyncIssue::key`).
    pub async fn sync_skip_on_this_device(&self, key: String) -> Result<SyncStatus> {
        self.on_sync(move |core| core.skip(key)).await
    }

    /// Acknowledges the "passphrase changed elsewhere" alert.
    pub async fn sync_acknowledge_alerts(&self) -> Result<SyncStatus> {
        self.on_sync(SyncCore::acknowledge).await
    }

    // ── Passphrase, device, options ─────────────────────────────────────

    /// Changes the passphrase (same data key, new slot). Does **not**
    /// protect against an already leaked passphrase (F2): only a new
    /// bundle does in S1.
    pub async fn sync_change_passphrase(
        &self,
        old: SecretString,
        new: NewPassphrase,
    ) -> Result<SyncStatus> {
        let (_, previous) = self.on_sync(|core| core.configured_bundle_text()).await?;
        let tuning = self.sync_tuning();
        let current = previous.clone();
        let (text, opened) = Self::blocking(move || {
            Ok(bundle::change_passphrase(
                &current,
                &old,
                &new,
                tuning.kdf_params,
                &tuning.kdf_bounds,
            )?)
        })
        .await?;
        self.on_sync(move |core| core.finish_rewrap(&previous, &text, &opened))
            .await
    }

    /// Resets the passphrase from this device, which holds the data key
    /// (owner decision #8): the other devices get an alert to acknowledge.
    pub async fn sync_reset_passphrase(&self, new: NewPassphrase) -> Result<SyncStatus> {
        let (_, previous, opened) = self.on_sync(SyncCore::opened_with_key).await?;
        let tuning = self.sync_tuning();
        let (text, rewrapped) = Self::blocking(move || {
            Ok(bundle::rewrap(
                &opened,
                &new,
                tuning.kdf_params,
                &tuning.kdf_bounds,
            )?)
        })
        .await?;
        self.on_sync(move |core| core.finish_rewrap(&previous, &text, &rewrapped))
            .await
    }

    /// Renames this device (roster and next snapshot).
    pub async fn sync_rename_device(&self, name: String) -> Result<SyncStatus> {
        let name = DeviceName::parse(&name)?;
        self.on_sync(move |core| core.rename_device(name)).await
    }

    /// "Remember the key on this device" on or off.
    pub async fn sync_set_options(&self, options: SyncOptions) -> Result<SyncStatus> {
        self.on_sync(move |core| core.set_remember(options.remember_key))
            .await
    }

    /// Disables sync on this device: forgets the data key by default
    /// (F14), optionally removes this device's file; local hosts, snippets
    /// and settings are kept.
    pub async fn sync_disable(&self, options: DisableOptions) -> Result<SyncStatus> {
        let status = self.on_sync(move |core| core.disable(options)).await?;
        if let Some(task) = self
            .sync_handle()
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.cancel();
        }
        Ok(status)
    }

    // ── S2a: rotation, forgetting a device (`docs/SYNC.md` §13.7) ────────

    /// What a rotation would affect: the devices, those still on Portico
    /// < 0.7 (`sync_peers_outdated`, Q5), whether the bundle switches to
    /// version 2.
    pub async fn sync_rotation_preview(&self) -> Result<RotationPreview> {
        self.on_sync(|core| Ok(core.rotation_preview())).await
    }

    /// Rotates the keys (§13.3): a new epoch and data key under a
    /// mandatory new passphrase (refused when it opens a slot this device
    /// verified: `sync_passphrase_reused`). Crash-safe and forward only: a
    /// stalled folder leaves `Waiting { RotationPending }` and every pass
    /// carries it on. Every other device must type the new passphrase.
    pub async fn sync_rotate_keys(&self, request: RotateKeys) -> Result<RotationOutcome> {
        let RotateKeys {
            authorization,
            new_passphrase,
            remember_key,
        } = request;
        self.rotate(Vec::new(), authorization, new_passphrase, remember_key)
            .await
    }

    /// What forgetting a device would affect (§13.4.2).
    pub async fn sync_forget_preview(&self, device_id: String) -> Result<ForgetPreview> {
        self.on_sync(move |core| core.forget_preview(&device_id))
            .await
    }

    /// Forgets devices (§13.4): always a rotation, the devices moved to the
    /// roster's forgotten list with their exact `revoked_clock`, their
    /// files removed. Refused for this device (`sync_forget_self`) and for
    /// an id not in the list (`sync_device_unknown`).
    pub async fn sync_forget_device(&self, request: ForgetDevice) -> Result<RotationOutcome> {
        let ForgetDevice {
            device_ids,
            authorization,
            new_passphrase,
            remember_key,
        } = request;
        if device_ids.is_empty() {
            return Err(SyncError::DeviceUnknown {
                device: String::new(),
            }
            .into());
        }
        self.rotate(device_ids, authorization, new_passphrase, remember_key)
            .await
    }

    async fn rotate(
        &self,
        forget: Vec<String>,
        authorization: RotateAuth,
        new_passphrase: crate::sync::NewPassphrase,
        remember_key: bool,
    ) -> Result<RotationOutcome> {
        let plan = self
            .on_sync(move |core| core.rotation_plan(&forget))
            .await?;
        let authorization = match authorization {
            RotateAuth::CurrentPassphrase(current) => Authorization::Passphrase(current),
            RotateAuth::ResetConfirmed => Authorization::HeldKey,
        };
        self.finish_rotate(plan, authorization, new_passphrase, remember_key)
            .await
    }

    async fn finish_rotate(
        &self,
        plan: crate::sync::runner::RotationPlan,
        authorization: Authorization,
        new_passphrase: crate::sync::NewPassphrase,
        remember_key: bool,
    ) -> Result<RotationOutcome> {
        let tuning = self.sync_tuning();
        let (plan, text, next) = Self::blocking(move || {
            let (text, next) = build_rotation(
                &plan,
                &authorization,
                &new_passphrase,
                tuning.kdf_params,
                &tuning.kdf_bounds,
            )?;
            Ok((plan, text, next))
        })
        .await?;
        let outcome = self
            .on_sync(move |core| core.finish_rotation(&plan, &next, &text, remember_key))
            .await?;
        self.start_sync();
        Ok(outcome)
    }

    /// The user's answer to `RotationConflict` (§13.3.7). `UseOther`
    /// checks the other device's keys like an adoption and refuses them
    /// while they still list a device this device forgot (E2);
    /// `KeepMine` rotates again from this device's keys.
    pub async fn sync_resolve_rotation_conflict(
        &self,
        choice: RotationChoice,
    ) -> Result<SyncStatus> {
        match choice {
            RotationChoice::UseOther(passphrase) => {
                let context = self.on_sync(SyncCore::conflict_context).await?;
                let bounds = self.sync_tuning().kdf_bounds;
                let decide = context.clone();
                let other =
                    Self::blocking(move || Ok(conflict_decide(&decide, &passphrase, &bounds)?))
                        .await?;
                self.on_sync(move |core| core.finish_use_other(&context, &other))
                    .await
            }
            RotationChoice::KeepMine {
                new_passphrase,
                remember_key,
            } => {
                let plan = self.on_sync(SyncCore::keep_mine_plan).await?;
                let outcome = self
                    .finish_rotate(plan, Authorization::HeldKey, new_passphrase, remember_key)
                    .await?;
                Ok(outcome.status)
            }
        }
    }

    /// "Restore the key file from this device" (§13.3.6): rewrites
    /// `bundle.toml` from this device's verified copy, on explicit request;
    /// refused during a rotation conflict.
    pub async fn sync_restore_bundle(&self) -> Result<SyncStatus> {
        self.on_sync(SyncCore::restore_bundle).await
    }

    /// Device files that announced a newer epoch at the last pass (RR5).
    pub async fn sync_newer_files(&self) -> Result<Vec<NewerEpochFile>> {
        self.on_sync(|core| Ok(core.newer_files())).await
    }

    /// Removes a device file that announces a newer epoch, when nobody
    /// changed the keys (RR5). `fingerprint` is the one shown
    /// ([`NewerEpochFile::fingerprint`]): another content is refused with
    /// `sync_not_pending`.
    pub async fn sync_discard_newer_file(
        &self,
        device_id: String,
        fingerprint: String,
    ) -> Result<SyncStatus> {
        self.on_sync(move |core| core.discard_newer_file(&device_id, &fingerprint))
            .await
    }

    /// Old sync keys in the keychain (never the key in use, §13.3.8).
    pub async fn sync_key_entries(&self) -> Result<Vec<SyncKeyEntry>> {
        self.on_sync(|core| core.key_entries()).await
    }

    /// Removes an old sync key: only an indexed `sync-key:<bundle>:<epoch>`
    /// that is not the key in use (`sync_key_entry_invalid`,
    /// `sync_key_entry_current`, R15).
    pub async fn sync_remove_key_entry(&self, account: String) -> Result<()> {
        self.on_sync(move |core| core.remove_key_entry(&account))
            .await
    }

    /// Local values held out of sync until a decision (R3, RR4).
    pub async fn sync_revoked_values(&self) -> Result<Vec<RevokedValueItem>> {
        self.on_sync(|core| Ok(core.revoked_values())).await
    }

    /// Decides a held value ([`RevokedValueItem::key`]); nothing is
    /// preselected. `hash` is the [`RevokedValueItem::hash`] shown: a value
    /// that changed since is refused with `sync_not_pending` (reload).
    pub async fn sync_revoked_value(
        &self,
        key: String,
        choice: RevokedValueChoice,
        hash: String,
    ) -> Result<SyncStatus> {
        self.on_sync(move |core| core.decide_revoked_value(&key, choice, &hash))
            .await
    }

    // ── Snippets changed by sync (F8) ───────────────────────────────────

    /// The snippets changed by sync that this device has not confirmed
    /// yet, with the content to show in full and its hash (badge, review
    /// screen). Works while sync is locked or disabled (the gate stays).
    /// [`EngineEvent::SyncPendingChanged`] says when to call it again.
    pub async fn synced_snippets_unconfirmed(&self) -> Result<Vec<UnconfirmedSnippet>> {
        let paths = self.inner.config.paths.clone();
        let store = Arc::clone(&self.inner.snippets);
        let mut list =
            Self::blocking(move || crate::sync::apply::unconfirmed_snippets(&paths, &store))
                .await?;
        let writers = self
            .sync_handle()
            .writers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for snippet in &mut list {
            if let Some(devices) = writers.get(&snippet.id) {
                snippet.devices.clone_from(devices);
            }
        }
        Ok(list)
    }

    /// The user read the whole command of a snippet changed by sync and
    /// confirmed it: it runs from now on. `hash` is
    /// [`UnconfirmedSnippet::hash`] as shown: content received since (a
    /// new hash) is refused with `sync_not_pending` and stays blocked.
    pub async fn confirm_synced_snippet(&self, id: String, hash: String) -> Result<()> {
        let paths = self.inner.config.paths.clone();
        Self::blocking(move || {
            let confirmed = state::edit_confirmations(&paths, |c| {
                if c.snippets.get(&id) == Some(&hash) {
                    c.snippets.remove(&id);
                    true
                } else {
                    false
                }
            })?;
            if confirmed {
                Ok(())
            } else {
                Err(SyncError::NotPending { alias: id }.into())
            }
        })
        .await?;
        self.emit(EngineEvent::SyncPendingChanged);
        Ok(())
    }

    /// The bytes to type for snippet `id` (rendered with `values`; `run`
    /// overrides the snippet's own flag). Refused with
    /// `sync_snippet_unconfirmed` while a change received through sync
    /// has not been confirmed on this device (F8): UIs send snippets
    /// through this method.
    pub async fn snippet_input_for(
        &self,
        id: String,
        values: Vec<(String, String)>,
        run: Option<bool>,
        bracketed_paste: bool,
    ) -> Result<Vec<u8>> {
        let paths = self.inner.config.paths.clone();
        let store = Arc::clone(&self.inner.snippets);
        Self::blocking(move || {
            let snippet = store
                .list()?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| crate::snippets::SnippetError::NotFound { id: id.clone() })?;
            let confirmations = state::load_confirmations(&paths)?;
            if confirmations.snippets.get(&snippet.id)
                == Some(&crate::sync::apply::snippet_hash(
                    &snippet.command,
                    snippet.run,
                ))
            {
                return Err(SyncError::SnippetUnconfirmed { id: snippet.id }.into());
            }
            let pairs: Vec<(&str, &str)> = values
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let rendered = snippet.render_checked(&pairs)?;
            crate::snippets::snippet_input(&rendered, run.unwrap_or(snippet.run), bracketed_paste)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
