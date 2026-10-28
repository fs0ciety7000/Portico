//! Key rotation, adoption and forgetting a device on the sync thread
//! (S2a, `docs/SYNC.md` §13.3–13.4). A child of [`super`]: it shares the
//! pass's private state.
//!
//! ## One transition, forward only (§13.3.4)
//!
//! A rotation made here and an adoption of another device's rotation are
//! the same transition from epoch `N` to `N'`:
//!
//! 3. **durable intent**: `sync-key:<bundle>:<N'>` (indexed, when
//!    remembered), `bundle.next` (the target bundle), then `rotation.toml`
//!    last. From there the local floor is `N'` and nothing goes back;
//! 4. `bundle.toml` written **only** when the folder holds a strictly
//!    lower `(epoch, generation, revision)` (RR2), else the folder's newer
//!    revision of the same lineage is adopted, another lineage is a
//!    `RotationConflict`, a newer epoch is judged like any newer file;
//! 5. the replica is re-sealed under `N'` (its clear envelope says whether
//!    that already happened), or re-joined under a new id: with provenance
//!    when it holds writes of a forgotten device beyond its bound (R3), of
//!    unknown origin when it cannot be opened any more (RR4); never a
//!    re-join downward;
//! 6. `bundle.verified`, the slot history, `commitment.toml`,
//!    `sync.toml.epoch = N'`; `rotation.toml` and `bundle.next` removed;
//! 7. the normal pass that follows publishes under `N'` and removes the
//!    forgotten devices' files;
//! 8. `sync-key:<bundle>:<N>` is collected by every pass (R12).
//!
//! Every pass runs this until `rotation.toml` is gone: a crash or a
//! timeout at any step only delays it.

use std::collections::{BTreeMap, BTreeSet};

use secrecy::SecretString;

use super::{
    Alert, Issue, Session, SyncCore, conflict_id, fresh_replica, key_account, key_of_conflict, now,
};
use crate::secrets::SecretKey;
use crate::sync::SyncError;
use crate::sync::api::{
    AlertKind, ForgetPreview, NewerEpochFile, RevokedValueChoice, RevokedValueItem,
    RotationOutcome, RotationPeer, RotationPreview, SyncKeyEntry, SyncStatus,
};
use crate::sync::bundle::{self, ClearHeader, OpenedBundle, Roster};
use crate::sync::clock::DeviceId;
use crate::sync::container::{self, Binding, Expected, ReplicaContent};
use crate::sync::crypto::{self, DataKey, KdfBounds, KdfParams, KeyBytes, MAC_LEN};
use crate::sync::folder::FolderFile;
use crate::sync::lineage;
use crate::sync::model::{RecordKey, RecordValue, WireKey, WireValue};
use crate::sync::passphrase::NewPassphrase;
use crate::sync::project::{self, LocalView};
use crate::sync::state::{self, BundleCopy, HeldValue, Rotation, SyncConfig};

/// Version of `rotation.toml`.
const ROTATION_VERSION: u32 = 1;
/// Epochs probed for `sync-key:` entries written before the index (§13.3.8).
const KEY_PROBE_EPOCHS: u32 = 64;
/// First Portico version that reads a v2 bundle (Q5).
const FIRST_S2_VERSION: &str = "0.7.0";

/// Where a test may stop a transition, as a crash would (the next engine
/// on the same data dir must go forward).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrashPoint {
    /// Step 3: `bundle.next` written, `rotation.toml` not yet.
    BeforeRotationFile,
    /// Step 3 done.
    AfterIntent,
    /// Step 4 done.
    AfterFolderWrite,
    /// Step 5 done.
    AfterReplica,
    /// Step 6 done (the old key is still in the keychain).
    AfterLocalState,
}

/// What [`SyncCore::unlock_context`] hands to the derivation (no secret).
#[derive(Debug, Clone)]
pub(crate) struct UnlockContext {
    sync: SyncConfig,
    folder_text: String,
    verified_text: Option<String>,
    /// `bundle.next` of a transition in progress.
    next_text: Option<String>,
    /// The local floor (R2).
    floor: u32,
    /// `C` of the floor epoch: `commitment.toml`, or `bundle.next`'s
    /// during a transition (E1).
    floor_commit: Option<[u8; MAC_LEN]>,
    /// Verified slots of this bundle (RR1).
    history: Vec<String>,
    /// The data key of the current epoch is in memory or remembered: the
    /// previous passphrase brings nothing then (RR4 only helps a device
    /// without it).
    has_current_key: bool,
}

/// What the passphrase opened ([`unlock_decide`]).
#[derive(Debug)]
pub(crate) enum Unlocked {
    /// The bundle of the floor epoch: an ordinary unlock.
    Same(OpenedBundle),
    /// A newer epoch whose lineage reaches this device's: adoption.
    Adopt(OpenedBundle, String),
    /// The previous passphrase opened `bundle.verified` (RR4): the data
    /// key of the current epoch, kept in memory only, so the replica can be
    /// migrated by the adoption that follows.
    Previous(OpenedBundle),
}

/// Decides what a typed passphrase unlocks (blocking: Argon2, up to one
/// derivation per history slot). Writes nothing.
pub(crate) fn unlock_decide(
    context: &UnlockContext,
    passphrase: &SecretString,
    bounds: &KdfBounds,
) -> Result<Unlocked, SyncError> {
    let bundle_id = context.sync.bundle_bytes()?;
    match bundle::open(&context.folder_text, passphrase, bounds) {
        Ok(opened) => {
            if opened.header.bundle_id != bundle_id {
                return Err(SyncError::BundleTampered);
            }
            match opened.header.epoch.cmp(&context.floor) {
                std::cmp::Ordering::Less
                    if opened.header.epoch == context.sync.epoch && !context.has_current_key =>
                {
                    // A transition is pending and the replica is still
                    // sealed under the current epoch: its key, in memory
                    // only, lets the replica move forward (RR4). Never
                    // adopted nor published under.
                    Ok(Unlocked::Previous(opened))
                }
                std::cmp::Ordering::Less => Err(SyncError::Rollback {
                    what: "bundle.toml".to_owned(),
                }),
                std::cmp::Ordering::Equal => {
                    if context
                        .floor_commit
                        .is_some_and(|commit| commit != opened.commit)
                    {
                        // Another lineage at this epoch: a conflict choice,
                        // never an unlock.
                        return Err(SyncError::RotationConflict);
                    }
                    Ok(Unlocked::Same(opened))
                }
                std::cmp::Ordering::Greater => {
                    let commit = context.floor_commit.ok_or(SyncError::LineageUnknown)?;
                    lineage::verify_adoption(&opened, context.floor, &commit)?;
                    if reuses_history(&context.history, passphrase, bounds) {
                        return Err(SyncError::PassphraseReused);
                    }
                    Ok(Unlocked::Adopt(opened, context.folder_text.clone()))
                }
            }
        }
        Err(SyncError::WrongPassphrase) => {
            // A transition in progress: the new passphrase opens its own
            // target, whatever the folder holds now.
            if let Some(next) = context.next_text.as_deref()
                && let Ok(opened) = bundle::open(next, passphrase, bounds)
                && opened.header.epoch == context.floor
                && opened.header.bundle_id == bundle_id
            {
                return Ok(Unlocked::Same(opened));
            }
            // This device's own verified copy of its current epoch: the
            // folder was put back (it opens nothing newer), or a newer
            // epoch waits and the replica needs the current key first
            // (RR4, "previous": in memory only, and only when this device
            // does not hold that key already).
            let folder_epoch = bundle::peek(&context.folder_text).map(|c| c.epoch).ok();
            let previous = context.floor > context.sync.epoch
                || folder_epoch.is_some_and(|epoch| epoch > context.sync.epoch);
            if previous && context.has_current_key {
                return Err(SyncError::WrongPassphrase);
            }
            let verified = context
                .verified_text
                .as_deref()
                .ok_or(SyncError::WrongPassphrase)?;
            let opened = bundle::open(verified, passphrase, bounds)?;
            if opened.header.bundle_id != bundle_id || opened.header.epoch != context.sync.epoch {
                return Err(SyncError::WrongPassphrase);
            }
            Ok(if previous {
                Unlocked::Previous(opened)
            } else {
                Unlocked::Same(opened)
            })
        }
        Err(e) => Err(e),
    }
}

/// A conflict choice without a current conflict (FAIBLE-4).
fn no_conflict() -> crate::Error {
    SyncError::NotPending {
        alias: "bundle.toml".to_owned(),
    }
    .into()
}

/// The passphrase opens one of the verified slots (RR1): at most one
/// derivation per entry, stopping at the first that opens.
fn reuses_history(history: &[String], passphrase: &SecretString, bounds: &KdfBounds) -> bool {
    history
        .iter()
        .any(|text| bundle::passphrase_opens(text, passphrase, bounds))
}

/// A rotation being prepared (no secret but the current data key, which
/// stays inside this process).
#[derive(Debug)]
pub(crate) struct RotationPlan {
    sync: SyncConfig,
    /// The bundle the rotation starts from.
    text: String,
    /// The folder's `bundle.toml` when planned (concurrent change check).
    folder_text: String,
    key: DataKey,
    roster: Roster,
    history: Vec<String>,
    forgotten: Vec<String>,
    enter_on: Vec<String>,
    /// The transition this rotation replaces ("keep my keys" while its
    /// own transition is in conflict).
    pending: Option<u32>,
}

/// How a rotation is authorized, on the derivation side.
#[derive(Debug)]
pub(crate) enum Authorization {
    /// The current passphrase must open the current slot and yield this
    /// device's data key.
    Passphrase(SecretString),
    /// This device's data key (reset-level confirmation, Q6; "keep my
    /// keys" after a conflict).
    HeldKey,
}

/// Builds the next epoch (blocking: Argon2). Refuses a new passphrase
/// that opens the current slot or any verified slot of the history (Q1,
/// RR1). Pure: nothing is written.
pub(crate) fn build_rotation(
    plan: &RotationPlan,
    authorization: &Authorization,
    new_passphrase: &NewPassphrase,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    crate::sync::passphrase::enforce(new_passphrase)?;
    let opened = match authorization {
        Authorization::Passphrase(current) => {
            let opened = bundle::open(&plan.text, current, bounds)?;
            if opened.data_key.bytes() != plan.key.bytes() {
                return Err(SyncError::BundleTampered);
            }
            opened
        }
        Authorization::HeldKey => bundle::open_with_key(&plan.text, &plan.key)?,
    };
    let new = new_passphrase.secret();
    if bundle::passphrase_opens(&plan.text, new, bounds)
        || reuses_history(&plan.history, new, bounds)
    {
        return Err(SyncError::PassphraseReused);
    }
    bundle::rotate(&opened, new_passphrase, plan.roster.clone(), params, bounds)
}

/// "Use the other device's keys" (§13.3.7): what the derivation checks.
#[derive(Debug, Clone)]
pub(crate) struct ConflictContext {
    sync: SyncConfig,
    folder_text: String,
    memory: Vec<state::RetainedEpoch>,
    /// This device's forgotten devices and their labels (E2).
    revoked: BTreeMap<DeviceId, String>,
    history: Vec<String>,
    /// This device's own transition in conflict (replaced by the choice).
    pending: Option<u32>,
    /// The local floor (R2): the other keys never take it lower.
    floor: u32,
}

/// Opens and checks the other lineage (blocking: Argon2): the lineage
/// from the last common epoch, the passphrase history, and **none of this
/// device's forgotten devices active there** (E2).
pub(crate) fn conflict_decide(
    context: &ConflictContext,
    passphrase: &SecretString,
    bounds: &KdfBounds,
) -> Result<OpenedBundle, SyncError> {
    let opened = bundle::open(&context.folder_text, passphrase, bounds)?;
    if opened.header.bundle_id != context.sync.bundle_bytes()? {
        return Err(SyncError::BundleTampered);
    }
    if opened.header.epoch < context.floor {
        return Err(SyncError::Rollback {
            what: "bundle.toml".to_owned(),
        });
    }
    lineage::verify_fork_adoption(&opened, &context.memory)?;
    if let Some(device) = opened
        .roster
        .devices
        .iter()
        .find(|d| context.revoked.contains_key(&d.id))
    {
        // Named as the other lineage lists it (this device's own roster
        // keeps no name for a forgotten device).
        return Err(SyncError::ConflictKeepsForgotten {
            device: device.name.label(&device.id),
        });
    }
    if reuses_history(&context.history, passphrase, bounds) {
        return Err(SyncError::PassphraseReused);
    }
    Ok(opened)
}

#[cfg(test)]
fn simulated_crash() -> SyncError {
    SyncError::Crypto("simulated crash")
}

impl SyncCore {
    /// Stops here when a test asked for a crash at `point`.
    // A no-op outside tests (the knob only exists there).
    #[cfg_attr(not(test), allow(clippy::unused_self, clippy::unnecessary_wraps))]
    fn crash_point(&self, point: CrashPoint) -> Result<(), SyncError> {
        #[cfg(test)]
        if self.tuning.crash_at == Some(point) {
            return Err(simulated_crash());
        }
        let _ = point;
        Ok(())
    }

    /// The history texts of this bundle (RR1).
    fn history_of(&self, bundle_id: &[u8; crypto::ID_LEN]) -> Vec<String> {
        state::load_history(&self.config.paths)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, _, text)| text)
            .filter(|text| bundle::peek(text).is_ok_and(|c| c.bundle_id == *bundle_id))
            .collect()
    }

    // ── Local cleanups (R12, RR6) ───────────────────────────────────────

    /// Leftovers of an interrupted step 3 (no `rotation.toml`):
    /// `bundle.next` and the keys of later epochs (RR6).
    pub(super) fn local_cleanups(&mut self, sync: &SyncConfig) {
        let paths = &self.config.paths;
        let pending = state::load_rotation(paths)
            .ok()
            .flatten()
            .filter(|r| r.bundle_id == sync.bundle_id);
        if pending.is_some() {
            return;
        }
        let _ = state::remove_bundle_copy(paths, BundleCopy::Next);
        let prefix = format!("sync-key:{}:", sync.bundle_id);
        let index = crate::secrets::load_index(paths).unwrap_or_default();
        for account in index.present_with_prefix(&prefix) {
            if let Some(SecretKey::SyncDataKey { epoch, .. }) = SecretKey::parse_sync_key(&account)
                && epoch > sync.epoch
            {
                let _ = self
                    .config
                    .secrets
                    .delete(&key_account(&sync.bundle_id, epoch));
            }
        }
    }

    /// Step 8, replayed by every pass (R12): the keys of this bundle below
    /// the current epoch (and not the next one of a transition) leave the
    /// keychain, then the index; the pre-S2 entries are probed once.
    pub(super) fn collect_old_keys(&mut self, sync: &SyncConfig) {
        let paths = &self.config.paths;
        let next = state::load_rotation(paths)
            .ok()
            .flatten()
            .filter(|r| r.bundle_id == sync.bundle_id)
            .map(|r| r.next_epoch);
        let prefix = format!("sync-key:{}:", sync.bundle_id);
        let index = crate::secrets::load_index(paths).unwrap_or_default();
        let mut epochs: BTreeSet<u32> = index
            .present_with_prefix(&prefix)
            .iter()
            .filter_map(|account| match SecretKey::parse_sync_key(account) {
                Some(SecretKey::SyncDataKey { epoch, .. }) => Some(epoch),
                _ => None,
            })
            .collect();
        let probe = (sync.bundle_id.clone(), sync.epoch);
        if self.probed_keys.as_ref() != Some(&probe) {
            let first = sync.epoch.saturating_sub(KEY_PROBE_EPOCHS).max(1);
            for epoch in first..sync.epoch {
                if self
                    .config
                    .secrets
                    .contains(&key_account(&sync.bundle_id, epoch))
                    .unwrap_or(false)
                {
                    epochs.insert(epoch);
                }
            }
            self.probed_keys = Some(probe);
        }
        for epoch in epochs {
            if epoch < sync.epoch && Some(epoch) != next {
                let _ = self
                    .config
                    .secrets
                    .delete(&key_account(&sync.bundle_id, epoch));
            }
        }
        self.keys.forget_below(&sync.bundle_id, sync.epoch);
    }

    /// Files of forgotten devices are ignored and removed, once per pass
    /// (§13.4.3: the authenticated roster justifies it).
    pub(super) fn remove_forgotten_files(session: &Session, revoked: &BTreeMap<DeviceId, u64>) {
        for device in revoked.keys() {
            if let Some(candidates) = session.listing.devices.get(device) {
                for candidate in candidates {
                    let _ = session.folder.remove(&candidate.file);
                }
            }
        }
    }

    // ── Held values (R3, RR4) ───────────────────────────────────────────

    /// Keeps the held values out of the diff and of the application (they
    /// join `view.unsynced`); a value of unknown origin equal to the
    /// synced one, or a key gone from the local files, is released.
    /// Returns the keys still held.
    pub(super) fn hold_values(session: &mut Session, view: &mut LocalView) -> BTreeSet<RecordKey> {
        let state = &session.replica.state;
        let mut held = BTreeSet::new();
        session.replica.local.held_values.retain(|(wire, value)| {
            let Some(key) = wire.decode() else {
                return false;
            };
            let Some(local) = view.records.get(&key) else {
                return false;
            };
            if value.from.is_none() && state.value(&key).as_ref() == Some(local) {
                return false;
            }
            held.insert(key);
            true
        });
        for key in &held {
            view.unsynced.insert(key.clone());
        }
        held
    }

    /// One warning per held value (`sync_revoked_value`).
    pub(super) fn held_issues(session: &Session) -> Vec<Issue> {
        session
            .replica
            .local
            .held_values
            .iter()
            .filter_map(|(wire, value)| {
                let key = wire.decode()?;
                Some(Issue {
                    key: format!("held:{}", conflict_id(&key)),
                    error: SyncError::RevokedValue {
                        kind: key.kind(),
                        device: value
                            .label
                            .clone()
                            .unwrap_or_else(|| "an unknown device".to_owned()),
                    },
                    record: Some(key),
                    warning: true,
                    skippable: false,
                })
            })
            .collect()
    }

    /// The held values, for the UIs.
    pub(crate) fn revoked_values(&self) -> Vec<RevokedValueItem> {
        self.report.held.clone()
    }

    /// A held value as the UIs show it: a secret is never shown.
    pub(super) fn held_shown(
        key: &RecordKey,
        value: Option<&RecordValue>,
    ) -> Option<crate::sync::api::SyncValue> {
        if matches!(key, RecordKey::Secret { .. }) {
            return None;
        }
        value.map(super::value_dto)
    }

    /// Hash of what a held value's decision is about (A3, S2c-1 review):
    /// the device it is attributed to, this device's value and the value
    /// the other devices hold. A decision names it, so a value that changed
    /// since it was shown is refused instead of decided unseen.
    pub(super) fn held_hash(
        device: Option<&str>,
        local: Option<&RecordValue>,
        synced: Option<&RecordValue>,
    ) -> String {
        let wire = |value: Option<&RecordValue>| {
            value.map_or_else(
                || "-".to_owned(),
                |v| {
                    WireValue::encode(v)
                        .map(|w| format!("+{}", w.as_str()))
                        .unwrap_or_default()
                },
            )
        };
        state::content_hash(
            "portico-sync/1 held value",
            &format!(
                "{}\0{}\0{}",
                device.map_or_else(|| "-".to_owned(), |d| format!("+{d}")),
                wire(local),
                wire(synced)
            ),
        )
    }

    /// The user's decision on a held value (no default, §13.4.2).
    pub(crate) fn decide_revoked_value(
        &mut self,
        id: &str,
        choice: RevokedValueChoice,
        shown: &str,
    ) -> crate::Result<SyncStatus> {
        let key = key_of_conflict(id).ok_or(SyncError::NotPending {
            alias: id.to_owned(),
        })?;
        let wire = WireKey::encode(&key)?;
        let paths = self.config.paths.clone();
        let snippets = std::sync::Arc::clone(&self.snippets);
        self.with_session(|session| {
            let not_pending = || -> crate::Error {
                SyncError::NotPending {
                    alias: id.to_owned(),
                }
                .into()
            };
            let held = &session.replica.local.held_values;
            let Some(index) = held.iter().position(|(key, _)| *key == wire) else {
                return Err(not_pending());
            };
            let label = held.get(index).and_then(|(_, value)| value.label.clone());
            let current = project::observe(&paths, &snippets);
            let hash = Self::held_hash(
                label.as_deref(),
                current.records.get(&key),
                session.replica.state.value(&key).as_ref(),
            );
            if hash != shown {
                // Changed since it was shown (a pass, an edit): reload.
                return Err(not_pending());
            }
            session.replica.local.held_values.remove(index);
            let mut applied = session.replica.local.applied_map();
            match choice {
                RevokedValueChoice::KeepAsMine => {
                    // The next pass sees it as a local write of this device.
                    applied.remove(&wire);
                }
                RevokedValueChoice::TakeSynced => {
                    let view = project::observe(&paths, &snippets);
                    if let Some(local) = view.records.get(&key) {
                        applied.insert(wire.clone(), WireValue::encode(local)?);
                    }
                    if session.replica.state.register(&key).is_none() {
                        // Nobody else holds it: deleted here through the
                        // sync, like any explicit deletion.
                        session.replica.state.write(
                            &mut session.clock,
                            &key,
                            &RecordValue::Tombstone,
                            now(),
                        )?;
                    }
                }
            }
            session.replica.local.set_applied(applied);
            Ok(())
        })?;
        Ok(self.status())
    }

    // ── Transition (steps 4–8) ──────────────────────────────────────────

    /// Carries a transition forward (module doc). `Ok` once it completed;
    /// an error leaves it pending (the status says why).
    // One ordered sequence (module doc): splitting it would scatter the
    // forward-only order.
    #[allow(clippy::too_many_lines)]
    pub(super) fn advance_transition(
        &mut self,
        sync: &SyncConfig,
        rotation: &Rotation,
    ) -> crate::Result<()> {
        let paths = self.config.paths.clone();
        if rotation.bundle_id != sync.bundle_id || rotation.next_epoch <= sync.epoch {
            // Not ours, or already done: forget it.
            state::remove_rotation(&paths)?;
            state::remove_bundle_copy(&paths, BundleCopy::Next)?;
            return Ok(());
        }
        self.stamp_keys(sync);
        let mut next_text =
            state::load_bundle_copy(&paths, BundleCopy::Next)?.ok_or(SyncError::Corrupt {
                what: "bundle.next",
                reason: "missing during a key change",
            })?;
        let next_clear = bundle::peek(&next_text)?;
        if next_clear.bundle_id != sync.bundle_bytes()? || next_clear.epoch != rotation.next_epoch {
            return Err(SyncError::Corrupt {
                what: "bundle.next",
                reason: "not the bundle of the key change",
            }
            .into());
        }
        let key_next = self.epoch_key(sync, next_clear.epoch);
        let folder = self.open_folder(sync)?;
        // ── Step 4: the folder, forward only (RR2) ──
        let folder_text = match Self::read_bundle(folder.as_ref()) {
            Ok(text) => Some(text),
            Err(SyncError::FolderNotPortico) => None,
            Err(e) => return Err(e.into()),
        };
        let folder_clear = match folder_text.as_deref().map(bundle::peek) {
            // Absent, or unreadable garbage: not a file of ours, nor a
            // newer one.
            None | Some(Err(SyncError::Corrupt { .. })) => None,
            Some(Ok(clear)) => Some(clear),
            Some(Err(e)) => return Err(e.into()),
        };
        let mut write_next = false;
        match &folder_clear {
            None => write_next = true,
            Some(found) if found.bundle_id != next_clear.bundle_id => {
                return Err(SyncError::BundleTampered.into());
            }
            Some(found) if found.epoch > next_clear.epoch => {
                // Forged from this transition's own lineage: rewritten
                // (§13.3.4); anything else stops here.
                Self::judge_from_next(&next_text, key_next.as_ref(), found)?;
                write_next = true;
            }
            Some(found) if found.epoch == next_clear.epoch && found.commit != next_clear.commit => {
                // Another lineage at the same epoch: a concurrent rotation
                // when its link N verifies under our K_rot_N, else forged
                // from our lineage (rewritten). Without our new key, the
                // clear headers alone never justify a rewrite.
                let key = key_next.as_ref().ok_or(SyncError::Locked)?;
                let next = bundle::open_with_key(&next_text, key)?;
                if lineage::judge_other_lineage(found, next_clear.epoch, &Self::memory_of(&next))
                    == SyncError::RotationConflict
                {
                    return Err(SyncError::RotationConflict.into());
                }
                write_next = true;
            }
            Some(found) if found.rank() < next_clear.rank() => write_next = true,
            Some(_) => {
                // Same lineage, revision ≥: the folder's file is adopted.
                if let Some(text) = folder_text.as_deref()
                    && text != next_text
                {
                    let key = key_next.as_ref().ok_or(SyncError::Locked)?;
                    bundle::open_with_key(text, key)?;
                    text.clone_into(&mut next_text);
                    state::save_bundle_copy(&paths, BundleCopy::Next, &next_text)?;
                }
            }
        }
        if write_next {
            folder
                .write(&FolderFile::Bundle, next_text.as_bytes())
                .map_err(|_| SyncError::RotationPending)?;
        }
        self.crash_point(CrashPoint::AfterFolderWrite)?;
        // ── Step 5: the replica ──
        let key_next = key_next.ok_or(SyncError::Locked)?;
        let next = bundle::open_with_key(&next_text, &key_next)?;
        if let Some(entry) = next.roster.revoked_entry(&sync.device) {
            return Err(Self::forgotten(&next.roster, entry).into());
        }
        let mut sync_next = sync.clone();
        self.migrate_replica(&mut sync_next, &next)?;
        self.crash_point(CrashPoint::AfterReplica)?;
        // ── Step 6: local state ──
        state::save_bundle_copy(&paths, BundleCopy::Verified, &next_text)?;
        if let Some(slot) = next.slot_id() {
            state::remember_slot(&paths, &slot, &next_text)?;
        }
        state::save_commitment(&paths, &sync.bundle_id, next.header.epoch, &next.commit)?;
        sync_next.epoch = next.header.epoch;
        state::save_config(&paths, &sync_next)?;
        state::remove_rotation(&paths)?;
        state::remove_bundle_copy(&paths, BundleCopy::Next)?;
        self.restamp_keys(&sync_next);
        self.crash_point(CrashPoint::AfterLocalState)?;
        // ── Step 8 (and every pass after) ──
        self.collect_old_keys(&sync_next);
        Ok(())
    }

    /// The lineage an opened bundle proves, as a lineage memory (its own
    /// epoch and every link it carries keys for).
    fn memory_of(opened: &OpenedBundle) -> Vec<state::RetainedEpoch> {
        let mut local = state::LocalReplica::default();
        for (epoch, key) in &opened.lineage_keys {
            if let Some(link) = opened.links.iter().find(|l| l.epoch == *epoch) {
                local.remember_epoch(*epoch, &link.digest, key.clone());
            }
        }
        local.remember_epoch(
            opened.header.epoch,
            &opened.commit,
            KeyBytes::of(&opened.keys.rotate),
        );
        local.lineage
    }

    /// A folder file newer than the transition's target (§13.3.4, last
    /// row), judged from the target's lineage: `Ok` when it is forged from
    /// that lineage (a link from its commitment with a false proof: the
    /// caller rewrites it), else the error that stops the transition.
    fn judge_from_next(
        next_text: &str,
        key_next: Option<&DataKey>,
        found: &ClearHeader,
    ) -> Result<(), SyncError> {
        let key = key_next.ok_or(SyncError::Locked)?;
        let next = bundle::open_with_key(next_text, key).map_err(|_| SyncError::BundleTampered)?;
        match lineage::judge_newer(
            found,
            next.header.epoch,
            &next.commit,
            Some(&next.keys.rotate),
            &Self::memory_of(&next),
        ) {
            lineage::Verdict::RotatedElsewhere => Err(SyncError::KeysRotated),
            lineage::Verdict::OtherLineage => Err(SyncError::RotationConflict),
            lineage::Verdict::ForgedFromOurs => Ok(()),
            lineage::Verdict::Tampered => Err(SyncError::BundleTampered),
        }
    }

    /// Step 5: the replica re-sealed under `next` (kept as is when its
    /// envelope already says so), or a re-join under a new id (R3, RR4).
    /// `sync` gets the new device id of a re-join.
    fn migrate_replica(&mut self, sync: &mut SyncConfig, next: &OpenedBundle) -> crate::Result<()> {
        let paths = self.config.paths.clone();
        let Some(epoch) = state::replica_epoch(&paths)? else {
            // No replica: the pass starts a fresh one (and its own-file
            // check re-joins when needed).
            return Ok(());
        };
        if epoch == next.header.epoch {
            return Ok(());
        }
        if epoch > next.header.epoch {
            // Never a re-join downward (§13.3.5).
            return Err(SyncError::Rollback {
                what: "bundle.toml".to_owned(),
            }
            .into());
        }
        let old = self.epoch_key(sync, epoch).and_then(|key| {
            let keys = crypto::derive_subkeys(&key, &next.header.bundle_id, epoch).ok()?;
            let expected = Expected {
                bundle_id: next.header.bundle_id,
                epoch,
                max_keyset_generation: u64::MAX,
                device: sync.device,
            };
            let opened = state::load_replica(&paths, &keys, &expected)
                .ok()
                .flatten()?;
            let commit =
                crypto::epoch_commitment(&keys.rotate, &next.header.bundle_id, epoch).ok()?;
            Some((opened, commit, KeyBytes::of(&keys.rotate)))
        });
        // Names of the devices of the old epoch (a forgotten device keeps
        // none in the new roster).
        let old_names = self
            .epoch_key(sync, epoch)
            .map(|key| Self::roster_names(&paths, &key))
            .unwrap_or_default();
        let Some((opened, commit, rotate)) = old else {
            // The replica does not open any more (a "don't remember"
            // device without its previous passphrase, RR4): a re-join where
            // every local difference is of unknown origin.
            return self.rejoin_unknown(sync, next);
        };
        let mut content = opened.content;
        content.local.remember_epoch(epoch, &commit, rotate);
        let (revoked, beyond) = Self::beyond_bounds(&content, &next.roster);
        if !beyond.is_empty() {
            return self.rejoin_with_provenance(sync, next, content, &revoked, &beyond, &old_names);
        }
        for (link_epoch, key) in &next.lineage_keys {
            if let Some(link) = next.links.iter().find(|l| l.epoch == *link_epoch) {
                content
                    .local
                    .remember_epoch(*link_epoch, &link.digest, key.clone());
            }
        }
        content.local.remember_epoch(
            next.header.epoch,
            &next.commit,
            KeyBytes::of(&next.keys.rotate),
        );
        content.local.known_keyset_generation = next.header.keyset_generation;
        content.local.epoch_created_here = next.header.epoch;
        // Republish under the new epoch.
        content.local.published_state = None;
        let seq = opened
            .binding
            .seq
            .checked_add(1)
            .ok_or(SyncError::Corrupt {
                what: "local replica",
                reason: "generation exhausted",
            })?;
        self.seal_replica(next, sync.device, seq, &content)
    }

    fn seal_replica(
        &self,
        next: &OpenedBundle,
        device: DeviceId,
        seq: u64,
        content: &ReplicaContent,
    ) -> crate::Result<()> {
        let binding = Binding {
            bundle_id: next.header.bundle_id,
            epoch: next.header.epoch,
            keyset_generation: next.header.keyset_generation,
            device,
            seq,
        };
        crate::store::create_private_dir_all(&self.config.paths.sync_dir())?;
        state::save_replica(&self.config.paths, &next.keys, &binding, content)
    }

    /// R3: the replica holds writes of a forgotten device beyond its bound
    /// (merged before this device learned the rotation). It re-joins under
    /// a new id with an empty state; every local key whose applied value
    /// came from such a write is held until the user decides; its former
    /// id is forgotten with its own last clock.
    fn rejoin_with_provenance(
        &mut self,
        sync: &mut SyncConfig,
        next: &OpenedBundle,
        old: ReplicaContent,
        revoked: &BTreeMap<DeviceId, u64>,
        beyond: &[DeviceId],
        old_names: &BTreeMap<DeviceId, String>,
    ) -> crate::Result<()> {
        let label = |device: &DeviceId| {
            old_names
                .get(device)
                .cloned()
                .unwrap_or_else(|| Self::label(&next.roster, device))
        };
        let mut held: Vec<(WireKey, HeldValue)> = Vec::new();
        for (wire, applied) in &old.local.applied {
            let Some(key) = wire.decode() else {
                continue;
            };
            let Some(register) = old.state.register(&key) else {
                continue;
            };
            let from = register
                .entries()
                .iter()
                .filter(|(_, entry)| entry.value == *applied)
                .map(|(dot, _)| dot)
                .find(|dot| {
                    revoked
                        .get(&dot.device)
                        .is_some_and(|bound| dot.counter > *bound)
                });
            if let Some(dot) = from {
                held.push((
                    wire.clone(),
                    HeldValue {
                        from: Some(dot.device),
                        label: Some(label(&dot.device)),
                    },
                ));
            }
        }
        for device in beyond {
            self.carry_alerts.push(Alert {
                kind: AlertKind::RevokedWrites,
                device: Some(label(device)),
                error: SyncError::RevokedWrites {
                    device: old.device_name.label(&sync.device),
                    forgotten: label(device),
                },
            });
        }
        let former = sync.device;
        let mut content = fresh_replica(old.device_name.clone(), next.header.keyset_generation);
        // What this device verified of the other devices' files stays true
        // (rollback and fork detection); only its own state starts over.
        content.high_water = old.high_water;
        content.local.held_values = held;
        content.local.retire.insert(former, old.clock);
        content.local.lineage = old.local.lineage;
        content.local.revoked_seen = old.local.revoked_seen;
        content.local.epoch_created_here = next.header.epoch;
        self.rejoin_as(sync, next, &content)
    }

    /// RR4: the replica cannot be opened: a re-join where every local
    /// record is held as "unknown origin" until the next passes compare it
    /// with the synced value (equal: released) or the user decides.
    fn rejoin_unknown(&mut self, sync: &mut SyncConfig, next: &OpenedBundle) -> crate::Result<()> {
        let view = project::observe(&self.config.paths, &self.snippets);
        let name = next
            .roster
            .devices
            .iter()
            .find(|d| d.id == sync.device)
            .map(|d| d.name.clone())
            .map_or_else(|| crate::sync::clock::DeviceName::parse("Portico"), Ok)?;
        let mut content = fresh_replica(name, next.header.keyset_generation);
        for key in view.records.keys() {
            if let Ok(wire) = WireKey::encode(key) {
                content.local.held_values.push((
                    wire,
                    HeldValue {
                        from: None,
                        label: None,
                    },
                ));
            }
        }
        content.local.epoch_created_here = next.header.epoch;
        content.local.remember_epoch(
            next.header.epoch,
            &next.commit,
            KeyBytes::of(&next.keys.rotate),
        );
        // Its former id is retired once its clock is known (FAIBLE-2).
        content.local.superseded.push(sync.device);
        self.carry_alerts.push(Alert {
            kind: AlertKind::Restored,
            device: None,
            error: SyncError::RestoredState {
                device: sync.device.short(),
            },
        });
        self.rejoin_as(sync, next, &content)
    }

    /// A new device id with `content` sealed under `next` (no dot of the
    /// former id is ever reused).
    fn rejoin_as(
        &mut self,
        sync: &mut SyncConfig,
        next: &OpenedBundle,
        content: &ReplicaContent,
    ) -> crate::Result<()> {
        let device = DeviceId::generate()?;
        sync.device = device;
        self.seal_replica(next, device, 1, content)
    }

    // ── Unlock and adoption (§13.3.5) ───────────────────────────────────

    /// What an unlock needs (no secret).
    pub(crate) fn unlock_context(&mut self) -> crate::Result<UnlockContext> {
        let paths = self.config.paths.clone();
        let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
        let rotation = state::load_rotation(&paths)?;
        let floor = state::floor(&sync, rotation.as_ref());
        let folder = self.open_folder(&sync)?;
        let folder_text = Self::read_bundle(folder.as_ref())?;
        let floor_commit = if floor > sync.epoch {
            state::load_bundle_copy(&paths, BundleCopy::Next)?
                .and_then(|text| bundle::peek(&text).ok())
                .filter(|clear| clear.epoch == floor)
                .and_then(|clear| clear.commit)
        } else {
            state::retained_commitment(&paths, &sync.bundle_id, sync.epoch)?
        };
        let bundle_id = sync.bundle_bytes()?;
        self.stamp_keys(&sync);
        let has_current_key = self.epoch_key(&sync, sync.epoch).is_some();
        Ok(UnlockContext {
            has_current_key,
            next_text: state::load_bundle_copy(&paths, BundleCopy::Next)?,
            verified_text: state::load_bundle_copy(&paths, BundleCopy::Verified)?,
            history: self.history_of(&bundle_id),
            sync,
            folder_text,
            floor,
            floor_commit,
        })
    }

    /// Finishes an unlock decided by [`unlock_decide`]: the key goes
    /// under the epoch of the bundle opened (never `sync.toml`'s), an
    /// adoption writes its durable intent (step 3), then a pass carries it
    /// forward. Nothing is written for a forgotten device.
    pub(crate) fn finish_unlock(
        &mut self,
        context: &UnlockContext,
        unlocked: Unlocked,
    ) -> crate::Result<SyncStatus> {
        let previous = matches!(unlocked, Unlocked::Previous(_));
        {
            let paths = self.config.paths.clone();
            crate::store::create_private_dir_all(&paths.sync_dir())?;
            let _lock = state::lock(&paths)?;
            let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
            let rotation = state::load_rotation(&paths)?;
            if sync.bundle_id != context.sync.bundle_id
                || state::floor(&sync, rotation.as_ref()) != context.floor
            {
                return Err(SyncError::Busy.into());
            }
            self.stamp_keys(&sync);
            match unlocked {
                Unlocked::Same(opened) => {
                    if sync.remember_key {
                        self.remember_key_at(
                            &sync.bundle_id,
                            opened.header.epoch,
                            &opened.data_key,
                        )?;
                    }
                    self.keys
                        .put(&sync.bundle_id, opened.header.epoch, &opened.data_key);
                }
                Unlocked::Previous(opened) => {
                    self.keys
                        .put(&sync.bundle_id, opened.header.epoch, &opened.data_key);
                }
                Unlocked::Adopt(opened, text) => {
                    if let Some(entry) = opened.roster.revoked_entry(&sync.device) {
                        let error = Self::forgotten(&opened.roster, entry);
                        self.set_status(super::status_for(&crate::Error::from(error.clone())));
                        return Err(error.into());
                    }
                    self.write_intent(&sync, &opened, &text, sync.remember_key)?;
                }
            }
        }
        if previous {
            // The current keys are in memory: the status says what is
            // still needed (usually the new passphrase).
            return Ok(self.pass().unwrap_or_else(|e| super::status_for(&e)));
        }
        self.pass()
    }

    /// Step 3: the durable intent of a transition to `next` (key, then
    /// `bundle.next`, then `rotation.toml` last). The caller holds
    /// `sync.lock`.
    fn write_intent(
        &mut self,
        sync: &SyncConfig,
        next: &OpenedBundle,
        text: &str,
        remember: bool,
    ) -> crate::Result<()> {
        let paths = self.config.paths.clone();
        let epoch = next.header.epoch;
        if remember {
            self.remember_key_at(&sync.bundle_id, epoch, &next.data_key)?;
        }
        self.keys.put(&sync.bundle_id, epoch, &next.data_key);
        state::save_bundle_copy(&paths, BundleCopy::Next, text)?;
        self.crash_point(CrashPoint::BeforeRotationFile)?;
        state::save_rotation(
            &paths,
            &Rotation {
                version: ROTATION_VERSION,
                bundle_id: sync.bundle_id.clone(),
                next_epoch: epoch,
            },
        )?;
        if remember != sync.remember_key {
            let mut updated = sync.clone();
            updated.remember_key = remember;
            state::save_config(&paths, &updated)?;
        }
        self.restamp_keys(sync);
        self.crash_point(CrashPoint::AfterIntent)?;
        Ok(())
    }

    // ── Rotation and forgetting (§13.3.4, §13.4) ────────────────────────

    /// Plans a rotation after a successful pass: refuses while an alert,
    /// a transition or a bundle rewrite is pending (`sync_busy`), checks
    /// the devices to forget and fixes their exact `revoked_clock`.
    pub(crate) fn rotation_plan(&mut self, forget: &[String]) -> crate::Result<RotationPlan> {
        let status = self.pass()?;
        if matches!(status, SyncStatus::Alert { .. }) {
            return Err(SyncError::Busy.into());
        }
        let paths = self.config.paths.clone();
        crate::store::create_private_dir_all(&paths.sync_dir())?;
        let _lock = state::lock(&paths)?;
        if state::load_rotation(&paths)?.is_some() {
            return Err(SyncError::Busy.into());
        }
        let mut alerts = Vec::new();
        let session = self.open_session(&mut alerts)?;
        if !alerts.is_empty() || session.roster_rewrite.is_some() {
            return Err(SyncError::Busy.into());
        }
        let me = session.sync.device;
        let mut roster = bundle::roster_v2(session.bundle.roster.clone());
        let mut forgotten = Vec::new();
        for id in forget {
            let device = DeviceId::from_hex(id)
                .ok_or_else(|| SyncError::DeviceUnknown { device: id.clone() })?;
            if device == me {
                return Err(SyncError::ForgetSelf.into());
            }
            let Some(index) = roster.devices.iter().position(|d| d.id == device) else {
                return Err(SyncError::DeviceUnknown {
                    device: device.short(),
                }
                .into());
            };
            let entry = roster.devices.remove(index);
            let verified = session
                .replica
                .high_water
                .devices
                .get(&device)
                .map_or(0, |mark| mark.clock);
            let revoked_clock = verified.max(session.replica.state.context().get(&device));
            forgotten.push(entry.name.label(&device));
            roster.revoked.push(bundle::RevokedDevice {
                id: device,
                revoked_at: now(),
                revoked_by: me,
                revoked_clock,
            });
        }
        if roster.revoked.len() > bundle::MAX_REVOKED_DEVICES {
            return Err(SyncError::RosterFull {
                max: bundle::MAX_REVOKED_DEVICES,
            }
            .into());
        }
        roster.revoked.sort_by_key(|entry| entry.id);
        let text = Self::read_bundle(session.folder.as_ref())?;
        let key = self.data_key_for(&session.sync)?;
        let check = bundle::open_with_key(&text, &key)?;
        if check.fingerprint != session.bundle.fingerprint {
            return Err(SyncError::Busy.into());
        }
        let enter_on = roster
            .devices
            .iter()
            .filter(|d| d.id != me)
            .map(|d| d.name.label(&d.id))
            .collect();
        Ok(RotationPlan {
            history: self.history_of(&session.bundle.header.bundle_id),
            pending: None,
            sync: session.sync.clone(),
            folder_text: text.clone(),
            text,
            key,
            roster,
            forgotten,
            enter_on,
        })
    }

    /// This device's newest lineage: `bundle.next` of a pending
    /// transition, else `bundle.verified`, opened with its key.
    fn own_lineage(
        &mut self,
        sync: &SyncConfig,
        pending: Option<u32>,
    ) -> crate::Result<OpenedBundle> {
        let paths = self.config.paths.clone();
        let (copy, epoch) = match pending {
            Some(epoch) => (BundleCopy::Next, epoch),
            None => (BundleCopy::Verified, sync.epoch),
        };
        let text = state::load_bundle_copy(&paths, copy)?.ok_or(SyncError::Corrupt {
            what: "bundle.verified",
            reason: "missing",
        })?;
        let key = self.epoch_key(sync, epoch).ok_or(SyncError::Locked)?;
        let mine = bundle::open_with_key(&text, &key)?;
        if mine.header.epoch != epoch {
            return Err(SyncError::Busy.into());
        }
        Ok(mine)
    }

    /// The folder's file is an authentic fork of `mine`'s lineage (§13.3.7):
    /// another lineage at its epoch whose earlier link verifies under a
    /// retained `K_rot`, or a newer epoch of another lineage.
    fn is_conflict(
        &self,
        sync: &SyncConfig,
        folder: &ClearHeader,
        mine: &OpenedBundle,
        pending: bool,
    ) -> bool {
        let mut memory = Self::memory_of(mine);
        if !pending
            && let Ok(keys) =
                crypto::derive_subkeys(&mine.data_key, &mine.header.bundle_id, mine.header.epoch)
        {
            memory.extend(self.lineage_memory(sync, &mine.header.bundle_id, &keys));
        }
        let epoch = mine.header.epoch;
        if folder.epoch == epoch && folder.commit != Some(mine.commit) {
            return lineage::judge_other_lineage(folder, epoch, &memory)
                == SyncError::RotationConflict;
        }
        folder.epoch > epoch
            && lineage::judge_newer(
                folder,
                epoch,
                &mine.commit,
                Some(&mine.keys.rotate),
                &memory,
            ) == lineage::Verdict::OtherLineage
    }

    /// "Keep this device's keys" after a conflict (§13.3.7): a rotation
    /// from this device's newest lineage, authorized by its data key:
    /// `bundle.verified`, or `bundle.next` when its own transition is the
    /// one in conflict (a fork at the epoch it was moving to: the new
    /// rotation then replaces that transition, one epoch further). The
    /// conflicting file in the folder is what step 4 then replaces
    /// (strictly lower epoch).
    pub(crate) fn keep_mine_plan(&mut self) -> crate::Result<RotationPlan> {
        let paths = self.config.paths.clone();
        crate::store::create_private_dir_all(&paths.sync_dir())?;
        let _lock = state::lock(&paths)?;
        let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
        self.stamp_keys(&sync);
        let pending = state::load_rotation(&paths)?.filter(|r| r.bundle_id == sync.bundle_id);
        let (text, epoch) = match &pending {
            Some(rotation) => (
                state::load_bundle_copy(&paths, BundleCopy::Next)?,
                rotation.next_epoch,
            ),
            None => (
                state::load_bundle_copy(&paths, BundleCopy::Verified)?,
                sync.epoch,
            ),
        };
        let text = text.ok_or(SyncError::Corrupt {
            what: "bundle.verified",
            reason: "missing",
        })?;
        let key = self.epoch_key(&sync, epoch).ok_or(SyncError::Locked)?;
        let mine = bundle::open_with_key(&text, &key)?;
        if mine.header.epoch != epoch {
            return Err(SyncError::Busy.into());
        }
        let folder = self.open_folder(&sync)?;
        let folder_text = Self::read_bundle(folder.as_ref())?;
        let folder_clear = bundle::peek(&folder_text)?;
        // Only an authentic fork is a conflict to resolve (FAIBLE-4).
        if !self.is_conflict(&sync, &folder_clear, &mine, pending.is_some()) {
            return Err(no_conflict());
        }
        let me = sync.device;
        let roster = bundle::roster_v2(mine.roster.clone());
        let enter_on = roster
            .devices
            .iter()
            .filter(|d| d.id != me)
            .map(|d| d.name.label(&d.id))
            .collect();
        Ok(RotationPlan {
            history: self.history_of(&mine.header.bundle_id),
            pending: pending.map(|r| r.next_epoch),
            sync,
            text,
            folder_text,
            key,
            roster,
            forgotten: Vec::new(),
            enter_on,
        })
    }

    /// Steps 3 and on of a rotation built by [`build_rotation`]: refused
    /// when the folder's `bundle.toml` changed since the plan.
    pub(crate) fn finish_rotation(
        &mut self,
        plan: &RotationPlan,
        next: &OpenedBundle,
        text: &str,
        remember: bool,
    ) -> crate::Result<RotationOutcome> {
        {
            let paths = self.config.paths.clone();
            let _lock = state::lock(&paths)?;
            let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
            let pending = state::load_rotation(&paths)?.map(|r| r.next_epoch);
            if sync != plan.sync || pending != plan.pending {
                return Err(SyncError::Busy.into());
            }
            let folder = self.open_folder(&sync)?;
            if Self::read_bundle(folder.as_ref())? != plan.folder_text {
                return Err(SyncError::Busy.into());
            }
            self.write_intent(&sync, next, text, remember)?;
        }
        // Committed at step 3: whatever the pass meets now (a stalled
        // folder, a conflict), the rotation only goes forward; the status
        // says where it stands.
        let status = match self.pass() {
            Ok(status) => status,
            Err(e) => super::status_for(&e),
        };
        Ok(RotationOutcome {
            epoch: next.header.epoch,
            status,
            enter_new_passphrase_on: plan.enter_on.clone(),
            forgotten: plan.forgotten.clone(),
            exposed_accounts: Vec::new(),
        })
    }

    /// What a rotation would affect (from the last pass).
    pub(crate) fn rotation_preview(&self) -> RotationPreview {
        // Outdated: older than the first S2 version **and** than this
        // build (a development build of an earlier version flags nothing).
        let minimum = [FIRST_S2_VERSION, env!("CARGO_PKG_VERSION")]
            .iter()
            .filter_map(|v| semver::Version::parse(v).ok())
            .min();
        let devices: Vec<RotationPeer> = self
            .report
            .devices
            .iter()
            .filter(|d| !d.revoked)
            .map(|d| {
                let id = DeviceId::from_hex(&d.id);
                let app_version = id.and_then(|id| self.peer_versions.get(&id).cloned());
                let outdated = !d.this_device
                    && match (&app_version, &minimum) {
                        (Some(version), Some(minimum)) => {
                            semver::Version::parse(version).map_or(true, |v| v < *minimum)
                        }
                        _ => false,
                    };
                RotationPeer {
                    id: d.id.clone(),
                    label: d.label.clone(),
                    this_device: d.this_device,
                    app_version,
                    outdated,
                    waiting: id.is_some_and(|id| self.withheld.contains(&id)),
                }
            })
            .collect();
        let outdated = devices
            .iter()
            .filter(|d| d.outdated)
            .map(|d| d.label.clone())
            .collect();
        RotationPreview {
            devices,
            outdated,
            format_upgrade: self.bundle_version == bundle::BUNDLE_V1,
            exposed_accounts: Vec::new(),
        }
    }

    /// What forgetting `device` would affect (§13.4.2).
    pub(crate) fn forget_preview(&mut self, device: &str) -> crate::Result<ForgetPreview> {
        let id = DeviceId::from_hex(device).ok_or_else(|| SyncError::DeviceUnknown {
            device: device.to_owned(),
        })?;
        let paths = self.config.paths.clone();
        crate::store::create_private_dir_all(&paths.sync_dir())?;
        let _lock = state::lock(&paths)?;
        let mut alerts = Vec::new();
        let session = self.open_session(&mut alerts)?;
        if id == session.sync.device {
            return Err(SyncError::ForgetSelf.into());
        }
        let entry = session
            .bundle
            .roster
            .devices
            .iter()
            .find(|d| d.id == id)
            .ok_or_else(|| SyncError::DeviceUnknown { device: id.short() })?;
        let last_written = session
            .replica
            .state
            .project()
            .known
            .iter()
            .filter(|(_, siblings)| {
                siblings
                    .iter()
                    .any(|s| !s.value.is_tombstone() && s.dots.iter().any(|d| d.device == id))
            })
            .map(|(key, _)| super::subject(key))
            .collect();
        Ok(ForgetPreview {
            id: id.to_string(),
            label: entry.name.label(&id),
            last_seen: session
                .replica
                .high_water
                .devices
                .get(&id)
                .map(|mark| mark.observed_at),
            last_written,
            exposed_accounts: Vec::new(),
            enter_new_passphrase_on: session
                .bundle
                .roster
                .devices
                .iter()
                .filter(|d| d.id != id && d.id != session.sync.device)
                .map(|d| d.name.label(&d.id))
                .collect(),
        })
    }

    // ── Conflicts, restore, discard (§13.3.5–13.3.7) ────────────────────

    /// What "use the other device's keys" checks.
    pub(crate) fn conflict_context(&mut self) -> crate::Result<ConflictContext> {
        let paths = self.config.paths.clone();
        let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
        let pending = state::load_rotation(&paths)?
            .filter(|r| r.bundle_id == sync.bundle_id)
            .map(|r| r.next_epoch);
        let key = self.data_key_for(&sync)?;
        let bundle_id = sync.bundle_bytes()?;
        let keys = crypto::derive_subkeys(&key, &bundle_id, sync.epoch)?;
        let memory = self.lineage_memory(&sync, &bundle_id, &keys);
        let mut revoked: BTreeMap<DeviceId, String> = BTreeMap::new();
        if let Some(text) = state::load_bundle_copy(&paths, BundleCopy::Verified)?
            && let Ok(mine) = bundle::open_with_key(&text, &key)
        {
            for entry in &mine.roster.revoked {
                revoked.insert(entry.id, Self::label(&mine.roster, &entry.id));
            }
        }
        // The devices this device's own pending transition forgets count
        // too (a fork landed while it waited for the folder).
        if let Some(epoch) = pending
            && let Some(next_key) = self.epoch_key(&sync, epoch)
            && let Some(text) = state::load_bundle_copy(&paths, BundleCopy::Next)?
            && let Ok(next) = bundle::open_with_key(&text, &next_key)
        {
            for entry in &next.roster.revoked {
                revoked
                    .entry(entry.id)
                    .or_insert_with(|| Self::label(&next.roster, &entry.id));
            }
        }
        let expected = Expected {
            bundle_id,
            epoch: sync.epoch,
            max_keyset_generation: u64::MAX,
            device: sync.device,
        };
        if let Ok(Some(replica)) = state::load_replica(&paths, &keys, &expected) {
            for device in replica.content.local.revoked_seen.keys() {
                revoked.entry(*device).or_insert_with(|| device.short());
            }
        }
        let folder = self.open_folder(&sync)?;
        let folder_text = Self::read_bundle(folder.as_ref())?;
        // Only an authentic fork is a conflict to resolve (FAIBLE-4).
        let mine = self.own_lineage(&sync, pending)?;
        if !self.is_conflict(
            &sync,
            &bundle::peek(&folder_text)?,
            &mine,
            pending.is_some(),
        ) {
            return Err(no_conflict());
        }
        Ok(ConflictContext {
            history: self.history_of(&bundle_id),
            floor: pending.unwrap_or(sync.epoch).max(sync.epoch),
            sync,
            folder_text,
            memory,
            revoked,
            pending,
        })
    }

    /// "Use the other device's keys", checked by [`conflict_decide`]: a
    /// newer epoch is adopted like any adoption; the same epoch replaces
    /// this device's keys and verified copy by the user's decision (its
    /// replica is re-sealed under the other keys, nothing is lost).
    pub(crate) fn finish_use_other(
        &mut self,
        context: &ConflictContext,
        other: &OpenedBundle,
    ) -> crate::Result<SyncStatus> {
        {
            let paths = self.config.paths.clone();
            let _lock = state::lock(&paths)?;
            let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
            let pending = state::load_rotation(&paths)?.map(|r| r.next_epoch);
            if sync != context.sync || pending != context.pending {
                return Err(SyncError::Busy.into());
            }
            let folder = self.open_folder(&sync)?;
            if Self::read_bundle(folder.as_ref())? != context.folder_text {
                return Err(SyncError::Busy.into());
            }
            if other.header.epoch > sync.epoch {
                // Replaces this device's own pending transition, if any.
                self.write_intent(&sync, other, &context.folder_text, sync.remember_key)?;
            } else if pending.is_none() {
                self.replace_lineage(&sync, other, &context.folder_text)?;
            } else {
                return Err(SyncError::Busy.into());
            }
        }
        self.pass()
    }

    /// Same epoch, other lineage, chosen by the user: the replica moves
    /// to the other keys (re-sealed, its bundle mark reset), then the key,
    /// the verified copy, the history and the commitment follow.
    fn replace_lineage(
        &mut self,
        sync: &SyncConfig,
        other: &OpenedBundle,
        text: &str,
    ) -> crate::Result<()> {
        let paths = self.config.paths.clone();
        let mine = self.data_key_for(sync)?;
        let keys = crypto::derive_subkeys(&mine, &other.header.bundle_id, sync.epoch)?;
        let expected = Expected {
            bundle_id: other.header.bundle_id,
            epoch: sync.epoch,
            max_keyset_generation: u64::MAX,
            device: sync.device,
        };
        let old_names = Self::roster_names(&paths, &mine);
        let mut sync_next = sync.clone();
        // The replica first: a crash before the keychain write leaves this
        // device's own key there (worst case, a re-join), never the other.
        if let Some(opened) = state::load_replica(&paths, &keys, &expected)? {
            let mut content = opened.content;
            // The bundle mark of this device's lineage means nothing for
            // the other one (chosen by the user).
            content.high_water.bundle = None;
            let (revoked, beyond) = Self::beyond_bounds(&content, &other.roster);
            if beyond.is_empty() {
                content.local.remember_epoch(
                    other.header.epoch,
                    &other.commit,
                    KeyBytes::of(&other.keys.rotate),
                );
                content.local.known_keyset_generation = other.header.keyset_generation;
                content.local.published_state = None;
                let seq = opened.binding.seq.saturating_add(1);
                self.seal_replica(other, sync.device, seq, &content)?;
            } else {
                // The other lineage forgot a device whose later writes this
                // replica holds (MOYEN-1 of the S2a review): the same
                // provenance re-join as an adoption (R3).
                self.rejoin_with_provenance(
                    &mut sync_next,
                    other,
                    content,
                    &revoked,
                    &beyond,
                    &old_names,
                )?;
                state::save_config(&paths, &sync_next)?;
            }
        }
        if sync.remember_key {
            self.remember_key_at(&sync.bundle_id, sync.epoch, &other.data_key)?;
        }
        state::save_bundle_copy(&paths, BundleCopy::Verified, text)?;
        if let Some(slot) = other.slot_id() {
            state::remember_slot(&paths, &slot, text)?;
        }
        state::save_commitment(&paths, &sync.bundle_id, other.header.epoch, &other.commit)?;
        self.keys.clear();
        self.keys
            .put(&sync.bundle_id, other.header.epoch, &other.data_key);
        self.restamp_keys(&sync_next);
        Ok(())
    }

    /// Labels of the devices of the bundle `key` opens in
    /// `bundle.verified` (a forgotten device keeps no name in a newer
    /// roster).
    fn roster_names(paths: &crate::store::Paths, key: &DataKey) -> BTreeMap<DeviceId, String> {
        state::load_bundle_copy(paths, BundleCopy::Verified)
            .ok()
            .flatten()
            .and_then(|text| bundle::open_with_key(&text, key).ok())
            .map(|old| {
                old.roster
                    .devices
                    .iter()
                    .map(|d| (d.id, d.name.label(&d.id)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The forgotten devices of `roster` and those whose writes `content`
    /// holds beyond their exact bound (§13.4.2).
    fn beyond_bounds(
        content: &ReplicaContent,
        roster: &Roster,
    ) -> (BTreeMap<DeviceId, u64>, Vec<DeviceId>) {
        let revoked: BTreeMap<DeviceId, u64> = roster
            .revoked
            .iter()
            .map(|entry| (entry.id, entry.revoked_clock))
            .collect();
        let beyond = content
            .state
            .context()
            .iter()
            .filter(|(device, counter)| revoked.get(device).is_some_and(|bound| **counter > *bound))
            .map(|(device, _)| *device)
            .collect();
        (revoked, beyond)
    }

    /// "Restore the key file from this device" (§13.3.6): rewrites
    /// `bundle.toml` from `bundle.verified` on the user's explicit request;
    /// refused during a rotation conflict (RR2).
    pub(crate) fn restore_bundle(&mut self) -> crate::Result<SyncStatus> {
        if matches!(
            self.status(),
            SyncStatus::Alert {
                kind: AlertKind::RotationConflict,
                ..
            }
        ) {
            return Err(SyncError::RotationConflict.into());
        }
        {
            let paths = self.config.paths.clone();
            let _lock = state::lock(&paths)?;
            let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
            self.stamp_keys(&sync);
            // During a transition, its own target is what this device
            // restores (FAIBLE-3 of the S2a review).
            let pending = state::load_rotation(&paths)?
                .filter(|r| r.bundle_id == sync.bundle_id)
                .map(|r| r.next_epoch);
            let (copy, epoch) = match pending {
                Some(epoch) => (BundleCopy::Next, epoch),
                None => (BundleCopy::Verified, sync.epoch),
            };
            let text = state::load_bundle_copy(&paths, copy)?.ok_or(SyncError::Corrupt {
                what: "bundle.verified",
                reason: "missing",
            })?;
            let key = self.epoch_key(&sync, epoch).ok_or(SyncError::Locked)?;
            let mine = bundle::open_with_key(&text, &key)?;
            if mine.header.epoch != epoch || mine.header.bundle_id != sync.bundle_bytes()? {
                return Err(SyncError::BundleTampered.into());
            }
            let mut memory = Self::memory_of(&mine);
            if pending.is_none() {
                let keys = crypto::derive_subkeys(&key, &mine.header.bundle_id, epoch)?;
                memory.extend(self.lineage_memory(&sync, &mine.header.bundle_id, &keys));
            }
            let folder = self.open_folder(&sync)?;
            let current = Self::read_bundle(folder.as_ref()).ok();
            if let Some(clear) = current.as_deref().and_then(|text| bundle::peek(text).ok()) {
                // Only an **authentic** fork or rotation is refused; a file
                // nobody of this lineage could write is what a restore is
                // for.
                if clear.epoch == epoch
                    && clear.commit != Some(mine.commit)
                    && lineage::judge_other_lineage(&clear, epoch, &memory)
                        == SyncError::RotationConflict
                {
                    return Err(SyncError::RotationConflict.into());
                }
                if clear.epoch > epoch {
                    match lineage::judge_newer(
                        &clear,
                        epoch,
                        &mine.commit,
                        Some(&mine.keys.rotate),
                        &memory,
                    ) {
                        // A genuine rotation of this lineage: adopt it.
                        lineage::Verdict::RotatedElsewhere => {
                            return Err(SyncError::KeysRotated.into());
                        }
                        lineage::Verdict::OtherLineage => {
                            return Err(SyncError::RotationConflict.into());
                        }
                        lineage::Verdict::ForgedFromOurs | lineage::Verdict::Tampered => {}
                    }
                }
            }
            folder.write(&FolderFile::Bundle, text.as_bytes())?;
        }
        self.pass()
    }

    /// The files that announced a newer epoch at the last pass (RR5).
    pub(crate) fn newer_files(&self) -> Vec<NewerEpochFile> {
        self.newer_files
            .iter()
            .map(|file| NewerEpochFile {
                device: file.device.to_string(),
                label: file.label.clone(),
                file: match &file.file {
                    FolderFile::Device(name) => format!("devices/{name}"),
                    FolderFile::Bundle => "bundle.toml".to_owned(),
                    FolderFile::BundleCopy(name) => name.clone(),
                },
                epoch: file.epoch,
                fingerprint: file.fingerprint.clone(),
            })
            .collect()
    }

    /// Removes a device file that announces a newer epoch, bound to the
    /// file shown (`fingerprint` of its bytes, RR5); an authentic file is
    /// published again by its device.
    pub(crate) fn discard_newer_file(
        &mut self,
        device: &str,
        fingerprint: &str,
    ) -> crate::Result<SyncStatus> {
        let not_pending = || SyncError::NotPending {
            alias: device.to_owned(),
        };
        let file = self
            .newer_files
            .iter()
            .find(|f| f.device.to_string() == device && f.fingerprint == fingerprint)
            .cloned()
            .ok_or_else(not_pending)?;
        {
            let paths = self.config.paths.clone();
            let _lock = state::lock(&paths)?;
            let sync = state::load_config(&paths)?.ok_or(SyncError::Disabled)?;
            let folder = self.open_folder(&sync)?;
            let bytes = folder
                .read(&file.file, container::MAX_CONTAINER_FILE_BYTES)?
                .ok_or_else(not_pending)?;
            if state::hex(&crypto::sha256(&bytes)) != fingerprint {
                return Err(not_pending().into());
            }
            folder.remove(&file.file)?;
        }
        self.newer_files.retain(|f| f.fingerprint != fingerprint);
        self.pass()
    }

    // ── Keychain entries (§13.3.8, R15) ─────────────────────────────────

    /// The current key entries (never listed nor removable): the floor
    /// epoch and, during a transition, the next one.
    fn current_keys(&self) -> Vec<SecretKey> {
        let paths = &self.config.paths;
        let Ok(Some(sync)) = state::load_config(paths) else {
            return Vec::new();
        };
        let mut keys = vec![key_account(&sync.bundle_id, sync.epoch)];
        if let Ok(Some(rotation)) = state::load_rotation(paths) {
            keys.push(key_account(&rotation.bundle_id, rotation.next_epoch));
        }
        keys
    }

    /// The indexed `sync-key:*` entries that are not the key in use.
    pub(crate) fn key_entries(&self) -> crate::Result<Vec<SyncKeyEntry>> {
        let index = crate::secrets::load_index(&self.config.paths)?;
        let current = self.current_keys();
        let bundle = state::load_config(&self.config.paths)?.map(|sync| sync.bundle_id);
        Ok(index
            .present_with_prefix("sync-key:")
            .into_iter()
            .filter_map(|account| SecretKey::parse_sync_key(&account))
            .filter(|key| !current.contains(key))
            .filter_map(|key| match key {
                SecretKey::SyncDataKey { bundle: id, epoch } => Some(SyncKeyEntry {
                    account: key_account(&id, epoch).account(),
                    this_bundle: bundle.as_deref() == Some(id.as_str()),
                    bundle: id,
                    epoch,
                }),
                _ => None,
            })
            .collect())
    }

    /// Removes an old sync key: only an **indexed** account of the exact
    /// form `sync-key:<32 hex>:<u32>` that is not the key in use (R15).
    pub(crate) fn remove_key_entry(&self, account: &str) -> crate::Result<()> {
        let key = SecretKey::parse_sync_key(account).ok_or(SyncError::KeyEntryInvalid)?;
        let index = crate::secrets::load_index(&self.config.paths)?;
        if !index
            .entries
            .get(account)
            .is_some_and(|entry| entry.present)
        {
            return Err(SyncError::KeyEntryInvalid.into());
        }
        if self.current_keys().contains(&key) {
            return Err(SyncError::KeyEntryCurrent.into());
        }
        self.config.secrets.delete(&key)
    }

    // ── Disable, leave the list (§13.4.5) ───────────────────────────────

    /// Disables sync on this device; local data is kept. `leave_roster`
    /// moves this device to the forgotten list with its own clock (an S2
    /// operation, best effort); `forget_key` also removes every indexed
    /// `sync-key:<bundle>:*` entry.
    pub(crate) fn disable(
        &mut self,
        options: crate::sync::api::DisableOptions,
    ) -> crate::Result<SyncStatus> {
        if let Some(sync) = state::load_config(&self.config.paths)? {
            let _lock = state::lock(&self.config.paths)?;
            if options.leave_roster {
                let _ = self.leave_roster();
            }
            if options.remove_my_file
                && let Ok(io) = self.open_folder(&sync)
            {
                let _ = io.remove(&FolderFile::primary(&sync.device));
            }
            if options.forget_key {
                self.config
                    .secrets
                    .delete(&super::data_key_account(&sync))?;
                let index = crate::secrets::load_index(&self.config.paths).unwrap_or_default();
                for account in index.present_with_prefix(&format!("sync-key:{}:", sync.bundle_id)) {
                    if let Some(key) = SecretKey::parse_sync_key(&account) {
                        self.config.secrets.delete(&key)?;
                    }
                }
            }
            state::remove_local_state(&self.config.paths)?;
        }
        self.keys.clear();
        self.joins.clear();
        self.report = super::Report::default();
        self.newer_files.clear();
        self.set_status(SyncStatus::Disabled);
        Ok(SyncStatus::Disabled)
    }

    /// Moves this device to the forgotten list (`revoked_by` = itself,
    /// `revoked_clock` = its own counter), in a v2 bundle. The caller
    /// holds `sync.lock`.
    fn leave_roster(&mut self) -> crate::Result<()> {
        let mut alerts = Vec::new();
        let session = self.open_session(&mut alerts)?;
        let me = session.sync.device;
        let mut roster = bundle::roster_v2(session.bundle.roster.clone());
        roster.devices.retain(|d| d.id != me);
        if roster.revoked_entry(&me).is_none() {
            roster.revoked.push(bundle::RevokedDevice {
                id: me,
                revoked_at: now(),
                revoked_by: me,
                revoked_clock: session.clock.counter(),
            });
        }
        roster.revoked.sort_by_key(|entry| entry.id);
        let (text, _) = bundle::to_v2(&session.bundle, roster)?;
        session.folder.write(&FolderFile::Bundle, text.as_bytes())?;
        Ok(())
    }
}
