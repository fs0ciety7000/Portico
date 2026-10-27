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
    DisableOptions, FolderProbe, FolderReason, JoinBulkChoice, JoinPreview, PendingHostChange,
    SyncConflict, SyncCreate, SyncDevice, SyncIssue, SyncJoin, SyncOptions, SyncOverview,
    SyncResolution, SyncStatus, UnconfirmedSnippet,
};
use crate::sync::bundle::{self, RosterDevice};
use crate::sync::clock::{DeviceId, DeviceName};
use crate::sync::passphrase::{GeneratedPassphrase, NewPassphrase};
use crate::sync::runner::{SyncCore, SyncTuning};
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
    notify: Arc<Notify>,
    task: Mutex<Option<CancellationToken>>,
}

impl Default for SyncHandle {
    fn default() -> Self {
        Self {
            jobs: Mutex::new(None),
            tuning: Mutex::new(SyncTuning::default()),
            status: Arc::new(Mutex::new(SyncStatus::Disabled)),
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
            remember_key: config.is_none_or(|c| c.remember_key),
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
        let opened = Self::blocking(move || Ok(bundle::open(&text, &passphrase, &bounds)?)).await?;
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
    /// remembered, or the keychain was unavailable).
    pub async fn sync_unlock(&self, passphrase: SecretString) -> Result<SyncStatus> {
        let (sync, text) = self.on_sync(|core| core.configured_bundle_text()).await?;
        let bounds = self.sync_tuning().kdf_bounds;
        let opened = Self::blocking(move || Ok(bundle::open(&text, &passphrase, &bounds)?)).await?;
        let status = self
            .on_sync(move |core| core.finish_unlock(&sync, &opened))
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
        let status = self
            .on_sync(move |core| core.disable(options.forget_key, options.remove_my_file))
            .await?;
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

    // ── Snippets changed by sync (F8) ───────────────────────────────────

    /// The snippets changed by sync that this device has not confirmed
    /// yet, with the content to show in full and its hash (badge, review
    /// screen). Works while sync is locked or disabled (the gate stays).
    /// [`EngineEvent::SyncPendingChanged`] says when to call it again.
    pub async fn synced_snippets_unconfirmed(&self) -> Result<Vec<UnconfirmedSnippet>> {
        let paths = self.inner.config.paths.clone();
        let store = Arc::clone(&self.inner.snippets);
        Self::blocking(move || crate::sync::apply::unconfirmed_snippets(&paths, &store)).await
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
