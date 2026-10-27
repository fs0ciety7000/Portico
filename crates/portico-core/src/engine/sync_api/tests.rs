//! Folder sync scenarios with several engines on one shared folder (S1b,
//! `docs/SYNC.md` §7): an in-memory provider (`folder::sim::SimFolder`)
//! that the tests drive like a real one (rollback, truncation, provider
//! copies, missing or read-only folder), and the real folder for the safe
//! root checks. Each engine has its own home (`Paths::rooted`) and an
//! in-memory secret store: no test touches `~/.ssh` or a real keychain.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;

use crate::engine::{DEFAULT_CONNECT_TIMEOUT, DEFAULT_KEEPALIVE, Engine, EngineConfig};
use crate::events::EngineEvent;
use crate::managed_hosts::HostDraft;
use crate::secrets::{MemoryStore, SecretKey, SecretStore};
use crate::snippets::{Snippet, SnippetScope};
use crate::store::Paths;
use crate::sync::api::{
    AlertKind, DisableOptions, FolderProbe, FolderReason, JoinBulkChoice, SyncCreate, SyncJoin,
    SyncOptions, SyncResolution, SyncStatus, SyncSubject, WaitReason,
};
use crate::sync::crypto::KdfBounds;
use crate::sync::folder::sim::SimFolder;
use crate::sync::passphrase::tests::{STRONG, typed};
use crate::sync::runner::{SyncCore, SyncTuning};

struct Device {
    _home: tempfile::TempDir,
    paths: Paths,
    engine: Engine,
    secrets: Arc<MemoryStore>,
}

fn tuning(sim: &SimFolder) -> SyncTuning {
    SyncTuning {
        kdf_bounds: KdfBounds::TEST,
        kdf_params: KdfBounds::TEST_PARAMS,
        opener: Arc::new(sim.clone()),
        poll: Duration::from_secs(3600),
        debounce: Duration::from_millis(50),
        op_timeout: Duration::from_secs(60),
        join_ttl: Duration::from_secs(600),
        background: false,
    }
}

fn engine_on(paths: &Paths, secrets: Arc<MemoryStore>, sim: &SimFolder) -> Engine {
    let engine = Engine::new(EngineConfig {
        paths: paths.clone(),
        local_user: "alice".into(),
        event_capacity: 1024,
        secrets,
        agent_socket: None,
        connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        keepalive: DEFAULT_KEEPALIVE,
        update_fetcher: crate::update::no_network(),
    });
    engine.set_sync_tuning(tuning(sim));
    engine
}

fn device(sim: &SimFolder) -> Device {
    let home = tempfile::tempdir().expect("tempdir");
    let paths = Paths::rooted(home.path());
    for dir in [&paths.ssh_dir, &paths.config_dir, &paths.data_dir] {
        std::fs::create_dir_all(dir).expect("mkdir");
    }
    let secrets = Arc::new(MemoryStore::default());
    let engine = engine_on(&paths, Arc::clone(&secrets), sim);
    Device {
        _home: home,
        paths,
        engine,
        secrets,
    }
}

/// The shared folder: a real directory (the safe-root checks run on it),
/// whose content is the in-memory provider.
fn shared() -> (tempfile::TempDir, PathBuf, SimFolder) {
    let dir = tempfile::tempdir().expect("tempdir");
    let folder = dir.path().join("Dropbox");
    std::fs::create_dir_all(&folder).expect("mkdir");
    (dir, folder, SimFolder::default())
}

async fn create(device: &Device, folder: &Path) -> SyncStatus {
    device
        .engine
        .sync_create(SyncCreate {
            folder: folder.to_path_buf(),
            passphrase: typed(STRONG),
            device_name: "Laptop".into(),
            remember_key: true,
        })
        .await
        .expect("create")
}

async fn join(device: &Device, folder: &Path, bulk: JoinBulkChoice) -> SyncStatus {
    let preview = device
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.to_path_buf(),
            passphrase: SecretString::from(STRONG.to_owned()),
            device_name: "Desktop".into(),
            remember_key: true,
        })
        .await
        .expect("preview");
    device
        .engine
        .sync_join_commit(preview.token, bulk)
        .await
        .expect("commit")
}

fn draft(alias: &str, hostname: &str) -> HostDraft {
    HostDraft {
        alias: alias.into(),
        hostname: hostname.into(),
        ..HostDraft::default()
    }
}

fn hosts(device: &Device) -> Vec<(String, String)> {
    crate::managed_hosts::read_hosts(&device.paths)
        .expect("hosts")
        .into_iter()
        .map(|h| (h.alias, h.hostname))
        .collect()
}

fn snippet(name: &str, command: &str) -> Snippet {
    Snippet {
        id: String::new(),
        name: name.into(),
        command: command.into(),
        description: None,
        scope: SnippetScope::Global,
        run: true,
    }
}

async fn sync(device: &Device) -> SyncStatus {
    device.engine.sync_now().await.expect("sync")
}

fn issue_codes(issues: &[crate::sync::SyncIssue]) -> Vec<String> {
    issues.iter().map(|i| i.summary.code.clone()).collect()
}

// One end-to-end scenario: splitting it would hide the sequence.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn two_engines_converge_including_an_offline_conflict() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    a.engine
        .set_tags("web", vec!["prod".into()])
        .await
        .expect("tags");
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    a.engine
        .set_theme(Some("basalt".into()), None)
        .await
        .expect("theme");
    assert!(matches!(create(&a, &folder).await, SyncStatus::Idle { .. }));

    let preview = b
        .engine
        .sync_join_preview(SyncJoin {
            folder: folder.clone(),
            passphrase: SecretString::from(STRONG.to_owned()),
            device_name: "Desktop".into(),
            remember_key: true,
        })
        .await
        .expect("preview");
    assert_eq!(
        (
            preview.hosts,
            preview.host_meta,
            preview.snippets,
            preview.settings
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(preview.devices.len(), 1);
    assert!(hosts(&b).is_empty(), "the preview writes nothing");
    assert!(!b.paths.sync_dir().join("sync.toml").exists());
    let mut events = b.engine.subscribe();
    b.engine
        .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
        .await
        .expect("join");

    // Metadata, snippets and settings are applied; the new host waits for
    // a confirmation on this device (F1).
    assert!(hosts(&b).is_empty());
    let pending = b.engine.sync_pending_hosts().await.expect("pending");
    assert_eq!(pending.len(), 1);
    assert!(pending.first().is_some_and(|p| p.alias == "web" && p.new));
    let settings = b.engine.settings().await.expect("settings");
    assert_eq!(settings.theme.as_deref(), Some("basalt"));
    assert_eq!(b.engine.snippets().await.expect("snippets").len(), 1);
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(
        seen.iter()
            .any(|e| matches!(e, EngineEvent::SnippetsChanged))
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, EngineEvent::SettingsChanged { .. }))
    );
    assert!(seen.iter().any(|e| matches!(e, EngineEvent::Sync { .. })));

    b.engine
        .sync_confirm_hosts(vec!["web".into()])
        .await
        .expect("confirm");
    assert_eq!(hosts(&b), vec![("web".to_owned(), "10.0.0.1".to_owned())]);
    let meta = b.engine.load_hosts().await.expect("inventory");
    assert!(
        meta.hosts
            .iter()
            .any(|h| h.alias == "web" && h.meta.tags == ["prod"])
    );

    // Offline edits on both sides: a visible conflict, never silent.
    a.engine
        .set_tags("web", vec!["eu".into()])
        .await
        .expect("tags");
    b.engine
        .set_tags("web", vec!["us".into()])
        .await
        .expect("tags");
    sync(&a).await;
    sync(&b).await;
    sync(&a).await;
    for device in [&a, &b] {
        let conflicts = device.engine.sync_conflicts().await.expect("conflicts");
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        let conflict = conflicts.first().expect("conflict");
        assert_eq!(
            conflict.subject,
            SyncSubject::HostMeta {
                alias: "web".into(),
                field: "tags".into()
            }
        );
        assert_eq!(conflict.sides.len(), 2);
        assert!(conflict.sides.iter().any(|s| s.local));
    }
    let id = a
        .engine
        .sync_conflicts()
        .await
        .expect("conflicts")
        .first()
        .expect("one")
        .id
        .clone();
    a.engine
        .sync_resolve(id, SyncResolution::Union)
        .await
        .expect("resolve");
    sync(&b).await;
    sync(&a).await;
    for device in [&a, &b] {
        assert!(device.engine.sync_conflicts().await.expect("c").is_empty());
        let inventory = device.engine.load_hosts().await.expect("inventory");
        let tags: Vec<String> = inventory
            .hosts
            .iter()
            .find(|h| h.alias == "web")
            .map(|h| h.meta.tags.clone())
            .unwrap_or_default();
        assert_eq!(tags, ["eu", "us"]);
    }
    let devices = a.engine.sync_devices().await.expect("devices");
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().all(|d| d.label.contains('(')), "{devices:?}");
}

#[tokio::test]
async fn sync_destination_change_held_until_confirmed() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("db", "10.0.0.1"))
        .await
        .expect("host");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    b.engine
        .sync_confirm_hosts(vec!["db".into()])
        .await
        .expect("confirm");
    assert_eq!(hosts(&b), vec![("db".to_owned(), "10.0.0.1".to_owned())]);

    // A redirects `db`: B keeps the old destination until it confirms.
    a.engine
        .update_host("db", draft("db", "203.0.113.9"))
        .await
        .expect("update");
    sync(&a).await;
    let status = sync(&b).await;
    assert!(
        matches!(
            status,
            SyncStatus::Idle {
                pending_confirmations: 1,
                ..
            }
        ),
        "{status:?}"
    );
    assert_eq!(hosts(&b), vec![("db".to_owned(), "10.0.0.1".to_owned())]);
    let pending = b.engine.sync_pending_hosts().await.expect("pending");
    let change = pending.first().expect("pending change");
    assert!(!change.new);
    assert_eq!(
        change.current.as_ref().map(|c| c.hostname.as_str()),
        Some("10.0.0.1")
    );
    assert_eq!(change.incoming.hostname, "203.0.113.9");
    assert!(change.devices.iter().any(|d| d.starts_with("Laptop (")));
    // More passes change nothing, and B's old value never goes back to A.
    sync(&b).await;
    sync(&a).await;
    assert_eq!(hosts(&a), vec![("db".to_owned(), "203.0.113.9".to_owned())]);

    // Refused: kept out. A new destination asks again; confirmed: written.
    b.engine
        .sync_reject_hosts(vec!["db".into()])
        .await
        .expect("reject");
    assert!(b.engine.sync_pending_hosts().await.expect("p").is_empty());
    assert_eq!(hosts(&b), vec![("db".to_owned(), "10.0.0.1".to_owned())]);
    a.engine
        .update_host("db", draft("db", "203.0.113.10"))
        .await
        .expect("update");
    sync(&a).await;
    sync(&b).await;
    assert_eq!(b.engine.sync_pending_hosts().await.expect("p").len(), 1);
    b.engine
        .sync_confirm_hosts(vec!["db".into()])
        .await
        .expect("confirm");
    assert_eq!(
        hosts(&b),
        vec![("db".to_owned(), "203.0.113.10".to_owned())]
    );
    // Unknown alias: not pending.
    let error = b
        .engine
        .sync_confirm_hosts(vec!["nope".into()])
        .await
        .expect_err("not pending");
    assert_eq!(error.summary().code, "sync_not_pending");
}

#[tokio::test]
async fn join_prefer_remote_does_not_confirm_redirects() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    create(&a, &folder).await;
    b.engine
        .create_host(draft("web", "10.9.9.9"))
        .await
        .expect("host");
    join(&b, &folder, JoinBulkChoice::PreferRemote).await;
    // The conflict is settled (remote side picked) …
    assert!(b.engine.sync_conflicts().await.expect("c").is_empty());
    // … but the redirect still waits for an explicit confirmation.
    assert_eq!(hosts(&b), vec![("web".to_owned(), "10.9.9.9".to_owned())]);
    let pending = b.engine.sync_pending_hosts().await.expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending.first().map(|p| p.incoming.hostname.as_str()),
        Some("10.0.0.1")
    );
    sync(&a).await;
    assert_eq!(hosts(&a), vec![("web".to_owned(), "10.0.0.1".to_owned())]);
}

#[tokio::test]
async fn sync_apply_refuses_hostname_like_alias() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    // Created in Portico on A: names that would shadow on B.
    a.engine
        .create_host(draft("github.com", "203.0.113.5"))
        .await
        .expect("host");
    a.engine
        .create_host(draft("db", "203.0.113.6"))
        .await
        .expect("host");
    a.engine
        .create_host(draft("ok", "203.0.113.7"))
        .await
        .expect("host");
    create(&a, &folder).await;
    // B's own config declares `db`.
    std::fs::write(b.paths.ssh_config(), "Host db\n  HostName 10.1.1.1\n").expect("w");
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let issues = b.engine.sync_issues().await.expect("issues");
    let shadowed: Vec<&str> = issues
        .iter()
        .filter(|i| i.summary.code == "sync_alias_shadowed")
        .map(|i| i.key.as_str())
        .collect();
    assert_eq!(shadowed, ["host:db", "host:github.com"], "{issues:?}");
    assert!(
        issues
            .iter()
            .all(|i| !i.summary.message.contains("203.0.113"))
    );
    let pending: Vec<String> = b
        .engine
        .sync_pending_hosts()
        .await
        .expect("pending")
        .into_iter()
        .map(|p| p.alias)
        .collect();
    assert_eq!(pending, ["ok"], "a shadowing host is never offered");
    // `~/.ssh/config` is never written.
    assert_eq!(
        std::fs::read_to_string(b.paths.ssh_config()).expect("r"),
        "Host db\n  HostName 10.1.1.1\n"
    );
    // "Skip on this device" removes the issue.
    b.engine
        .sync_skip_on_this_device("host:github.com".into())
        .await
        .expect("skip");
    let codes = issue_codes(&b.engine.sync_issues().await.expect("issues"));
    assert_eq!(
        codes.iter().filter(|c| *c == "sync_alias_shadowed").count(),
        1
    );
}

#[tokio::test]
async fn quarantined_or_held_record_never_becomes_tombstone() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("db", "10.0.0.1"))
        .await
        .expect("host");
    a.engine
        .create_host(draft("web", "10.0.0.2"))
        .await
        .expect("host");
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("snippet");
    create(&a, &folder).await;
    std::fs::write(b.paths.ssh_config(), "Host db\n  HostName 10.1.1.1\n").expect("w");
    // B's snippets.toml is unreadable: snippets are suspended there.
    std::fs::write(b.paths.snippets(), "[[snippet]\nbroken").expect("w");
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    for _ in 0..3 {
        sync(&b).await;
        sync(&a).await;
    }
    // Shadowed `db`, pending `web`, the suspended snippet: all still on A.
    assert_eq!(
        hosts(&a),
        vec![
            ("db".to_owned(), "10.0.0.1".to_owned()),
            ("web".to_owned(), "10.0.0.2".to_owned())
        ]
    );
    assert_eq!(a.engine.snippets().await.expect("s").len(), 1);
    assert!(a.engine.sync_conflicts().await.expect("c").is_empty());
}

#[tokio::test]
async fn synced_snippet_change_requires_confirmation_before_run() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    let saved = a
        .engine
        .save_snippet(snippet("Deploy", "make deploy"))
        .await
        .expect("snippet");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    // A's own snippet runs on A.
    a.engine
        .snippet_input_for(saved.id.clone(), Vec::new(), None, false)
        .await
        .expect("own snippet");
    // On B it was received: refused until confirmed.
    assert_eq!(
        b.engine.synced_snippets_unconfirmed().await.expect("u"),
        vec![saved.id.clone()]
    );
    let error = b
        .engine
        .snippet_input_for(saved.id.clone(), Vec::new(), None, false)
        .await
        .expect_err("unconfirmed");
    assert_eq!(error.summary().code, "sync_snippet_unconfirmed");
    b.engine
        .confirm_synced_snippet(saved.id.clone())
        .await
        .expect("confirm");
    assert_eq!(
        b.engine
            .snippet_input_for(saved.id.clone(), Vec::new(), None, false)
            .await
            .expect("confirmed"),
        b"make deploy\r"
    );
    // A changes it: B asks again.
    a.engine
        .save_snippet(Snippet {
            command: "make deploy && curl evil".into(),
            ..saved.clone()
        })
        .await
        .expect("edit");
    sync(&a).await;
    sync(&b).await;
    assert!(
        b.engine
            .snippet_input_for(saved.id.clone(), Vec::new(), None, false)
            .await
            .is_err()
    );
    // A local edit on B is B's own: it runs.
    b.engine
        .save_snippet(Snippet {
            command: "make deploy-safe".into(),
            ..saved.clone()
        })
        .await
        .expect("edit");
    sync(&b).await;
    b.engine
        .snippet_input_for(saved.id, Vec::new(), None, false)
        .await
        .expect("own edit");
}

#[tokio::test]
async fn deletions_propagate_as_tombstones() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    let saved = a
        .engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("s");
    a.engine
        .set_group("web", Some("Prod".into()))
        .await
        .expect("group");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    b.engine
        .sync_confirm_hosts(vec!["web".into()])
        .await
        .expect("confirm");
    assert_eq!(hosts(&b).len(), 1);
    a.engine.delete_snippet(&saved.id).await.expect("delete");
    a.engine.set_group("web", None).await.expect("clear");
    a.engine.delete_host("web").await.expect("delete");
    sync(&a).await;
    sync(&b).await;
    assert!(hosts(&b).is_empty());
    assert!(b.engine.snippets().await.expect("s").is_empty());
    let (meta, _) = crate::hosts::load_meta(&b.paths.hosts_meta()).expect("meta");
    assert!(meta.hosts.get("web").is_none_or(|m| m.group.is_none()));
}

#[tokio::test]
async fn restored_data_dir_rejoins_under_a_new_id_and_keeps_data() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let before = a.engine.sync_status().await.expect("s").device;
    // Back up A's sync state, then make changes A publishes.
    let backup = tempfile::tempdir().expect("tempdir");
    for file in ["sync.toml", "replica.snap"] {
        std::fs::copy(a.paths.sync_dir().join(file), backup.path().join(file)).expect("cp");
    }
    a.engine
        .save_snippet(snippet("One", "echo 1"))
        .await
        .expect("s");
    sync(&a).await;
    a.engine
        .save_snippet(snippet("Two", "echo 2"))
        .await
        .expect("s");
    sync(&a).await;
    // Restore the old state: A's own file is ahead of it.
    for file in ["sync.toml", "replica.snap"] {
        std::fs::copy(backup.path().join(file), a.paths.sync_dir().join(file)).expect("cp");
    }
    let status = sync(&a).await;
    assert!(
        matches!(
            status,
            SyncStatus::Alert {
                kind: AlertKind::Restored,
                ..
            }
        ),
        "{status:?}"
    );
    let after = a.engine.sync_status().await.expect("s").device;
    assert_ne!(before, after, "a new device id");
    // Nothing was lost, B converges, and A is quiet again.
    sync(&b).await;
    assert_eq!(b.engine.snippets().await.expect("s").len(), 2);
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
}

#[tokio::test]
async fn provider_rollback_truncation_and_conflict_copies() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let a_id = a.engine.sync_status().await.expect("s").device.expect("id");
    let a_file = format!("devices/{a_id}.snap");
    let old = sim
        .with(|s| s.files.get(&a_file).cloned())
        .expect("a's file");
    a.engine
        .save_snippet(snippet("One", "echo 1"))
        .await
        .expect("s");
    sync(&a).await;
    sync(&b).await;
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1);

    // A rollback of A's file is refused and named.
    let current = sim
        .with(|s| s.files.insert(a_file.clone(), old.clone()))
        .expect("cur");
    let status = sync(&b).await;
    assert!(
        matches!(&status, SyncStatus::Alert { kind: AlertKind::Rollback, device: Some(d) } if d.starts_with("Laptop (")),
        "{status:?}"
    );
    assert_eq!(
        b.engine.snippets().await.expect("s").len(),
        1,
        "nothing undone"
    );

    // A torn upload is "waiting", never an error, never a deletion.
    a.engine
        .save_snippet(snippet("Two", "echo 2"))
        .await
        .expect("s");
    sync(&a).await;
    let newest = sim.with(|s| s.files.get(&a_file).cloned()).expect("new");
    sim.with(|s| {
        let mut torn = newest.clone();
        torn.truncate(torn.len() / 2);
        s.files.insert(a_file.clone(), torn);
    });
    let status = sync(&b).await;
    assert!(
        matches!(
            status,
            SyncStatus::Waiting {
                reason: WaitReason::PartialSync,
                ..
            }
        ),
        "{status:?}"
    );
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1);
    // The provider kept the full file as a conflict copy: it is used.
    sim.with(|s| {
        s.files.insert(
            format!("devices/{a_id} (conflicted copy).snap"),
            newest.clone(),
        );
    });
    sync(&b).await;
    assert_eq!(b.engine.snippets().await.expect("s").len(), 2);
    drop(current);
    // A removes its own verified older copies, and only those.
    sim.with(|s| {
        s.files
            .insert(format!("devices/{a_id} (1).snap"), old.clone());
        s.files
            .insert(format!("devices/{a_id} (2).snap"), b"junk".to_vec());
    });
    sync(&a).await;
    let left: Vec<String> = sim.with(|s| {
        s.files
            .keys()
            .filter(|k| k.starts_with(&format!("devices/{a_id} (")))
            .cloned()
            .collect()
    });
    assert_eq!(left, [format!("devices/{a_id} (2).snap")]);
}

#[tokio::test]
async fn missing_and_read_only_folders() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    sim.with(|s| s.missing = true);
    let error = a.engine.sync_now().await.expect_err("missing");
    assert_eq!(error.summary().code, "sync_folder_missing");
    assert_eq!(
        a.engine.sync_status().await.expect("s").status,
        SyncStatus::FolderUnavailable {
            reason: FolderReason::Missing
        }
    );
    sim.with(|s| s.missing = false);
    // Read-only: changes still arrive, publishing is reported.
    a.engine
        .save_snippet(snippet("One", "echo 1"))
        .await
        .expect("s");
    sync(&a).await;
    sim.with(|s| s.read_only = true);
    sync(&b).await;
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1);
    b.engine
        .save_snippet(snippet("Two", "echo 2"))
        .await
        .expect("s");
    sync(&b).await;
    let codes = issue_codes(&b.engine.sync_issues().await.expect("issues"));
    assert!(
        codes.contains(&"sync_folder_unwritable".to_owned()),
        "{codes:?}"
    );
    sim.with(|s| s.read_only = false);
    sync(&b).await;
    sync(&a).await;
    assert_eq!(a.engine.snippets().await.expect("s").len(), 2);
}

#[tokio::test]
async fn two_processes_share_one_device_without_forking() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    create(&a, &folder).await;
    // The GUI next to the TUI: same data dir and keychain.
    let gui = engine_on(&a.paths, Arc::clone(&a.secrets), &sim);
    let status = gui.sync_now().await.expect("pass");
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    gui.save_snippet(snippet("From GUI", "uptime"))
        .await
        .expect("s");
    gui.sync_now().await.expect("pass");
    let status = a.engine.sync_now().await.expect("pass");
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    assert_eq!(
        a.engine.sync_status().await.expect("s").device,
        gui.sync_status().await.expect("s").device
    );
    // While one process holds `sync.lock`, the other's background pass
    // skips, and "Sync now" waits for it.
    let lock = crate::sync::state::try_lock(&a.paths)
        .expect("lock")
        .expect("free");
    let writes = sim.with(|s| s.writes);
    a.engine
        .save_snippet(snippet("Later", "true"))
        .await
        .expect("s");
    let background = a
        .engine
        .on_sync(SyncCore::background_pass)
        .await
        .expect("skipped");
    assert!(
        matches!(background, SyncStatus::Idle { .. }),
        "{background:?}"
    );
    assert_eq!(sim.with(|s| s.writes), writes);
    let waiting = {
        let engine = a.engine.clone();
        tokio::spawn(async move { engine.sync_now().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(sim.with(|s| s.writes), writes, "waits for the lock");
    drop(lock);
    waiting.await.expect("join").expect("pass");
    assert!(sim.with(|s| s.writes) > writes);
}

#[tokio::test]
async fn disable_forgets_the_key_and_keeps_local_data() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    create(&a, &folder).await;
    let config = crate::sync::state::load_config(&a.paths)
        .expect("c")
        .expect("set up");
    let account = SecretKey::SyncDataKey {
        bundle: config.bundle_id.clone(),
        epoch: 1,
    };
    assert!(
        a.secrets.get(&account).expect("get").is_some(),
        "remembered"
    );
    // Not remembered: a restart is locked until the passphrase.
    a.engine
        .sync_set_options(SyncOptions {
            remember_key: false,
        })
        .await
        .expect("options");
    assert!(a.secrets.get(&account).expect("get").is_none());
    let restarted = engine_on(&a.paths, Arc::clone(&a.secrets), &sim);
    assert_eq!(
        restarted
            .sync_now()
            .await
            .expect_err("locked")
            .summary()
            .code,
        "sync_locked"
    );
    let error = restarted
        .sync_unlock(SecretString::from("wrong passphrase".to_owned()))
        .await
        .expect_err("wrong");
    assert_eq!(error.summary().code, "sync_wrong_passphrase");
    let status = restarted
        .sync_unlock(SecretString::from(STRONG.to_owned()))
        .await
        .expect("unlock");
    assert!(matches!(status, SyncStatus::Idle { .. }), "{status:?}");
    // Disable: key forgotten, local data kept, own file removed on demand.
    restarted
        .sync_set_options(SyncOptions { remember_key: true })
        .await
        .expect("options");
    assert!(a.secrets.get(&account).expect("get").is_some());
    let status = restarted
        .sync_disable(DisableOptions {
            forget_key: true,
            remove_my_file: true,
        })
        .await
        .expect("disable");
    assert_eq!(status, SyncStatus::Disabled);
    assert!(a.secrets.get(&account).expect("get").is_none());
    assert_eq!(hosts(&a), vec![("web".to_owned(), "10.0.0.1".to_owned())]);
    assert!(!a.paths.sync_dir().join("replica.snap").exists());
    assert!(
        sim.with(|s| s.files.keys().all(|k| !k.starts_with("devices/"))),
        "own file removed"
    );
    assert_eq!(
        restarted.sync_status().await.expect("s").status,
        SyncStatus::Disabled
    );
}

#[tokio::test]
async fn join_token_is_single_use_and_expires() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    let request = || SyncJoin {
        folder: folder.clone(),
        passphrase: SecretString::from(STRONG.to_owned()),
        device_name: "Desktop".into(),
        remember_key: false,
    };
    let preview = b
        .engine
        .sync_join_preview(request())
        .await
        .expect("preview");
    assert_eq!(
        b.engine
            .sync_join_commit("00".repeat(16), JoinBulkChoice::ReviewEach)
            .await
            .expect_err("unknown")
            .summary()
            .code,
        "sync_join_expired"
    );
    b.engine
        .sync_join_commit(preview.token.clone(), JoinBulkChoice::ReviewEach)
        .await
        .expect("join");
    assert_eq!(
        b.engine
            .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
            .await
            .expect_err("reused")
            .summary()
            .code,
        "sync_join_expired"
    );
    // Expired.
    let c = device(&sim);
    c.engine.set_sync_tuning(SyncTuning {
        join_ttl: Duration::ZERO,
        ..tuning(&sim)
    });
    let preview = c
        .engine
        .sync_join_preview(request())
        .await
        .expect("preview");
    assert_eq!(
        c.engine
            .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
            .await
            .expect_err("expired")
            .summary()
            .code,
        "sync_join_expired"
    );
    assert!(!c.paths.sync_dir().join("sync.toml").exists());
}

#[tokio::test]
async fn create_refuses_weak_passphrases_existing_bundles_and_private_folders() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let weak = a
        .engine
        .sync_create(SyncCreate {
            folder: folder.clone(),
            passphrase: typed("password1"),
            device_name: "Laptop".into(),
            remember_key: true,
        })
        .await
        .expect_err("weak");
    assert_eq!(weak.summary().code, "sync_passphrase_weak");
    assert!(sim.with(|s| s.files.is_empty()), "nothing written");
    let inside = a
        .engine
        .sync_probe_folder(a.paths.ssh_dir.clone())
        .await
        .expect("probe");
    assert!(matches!(inside, FolderProbe::Refused { .. }));
    assert_eq!(
        a.engine
            .sync_probe_folder(folder.clone())
            .await
            .expect("probe"),
        FolderProbe::Usable
    );
    create(&a, &folder).await;
    assert!(matches!(
        a.engine
            .sync_probe_folder(folder.clone())
            .await
            .expect("probe"),
        FolderProbe::ExistingBundle { .. }
    ));
    let b = device(&sim);
    let exists = b
        .engine
        .sync_create(SyncCreate {
            folder: folder.clone(),
            passphrase: typed(STRONG),
            device_name: "Desktop".into(),
            remember_key: true,
        })
        .await
        .expect_err("exists");
    assert_eq!(exists.summary().code, "sync_bundle_exists");
    let again = a
        .engine
        .sync_create(SyncCreate {
            folder,
            passphrase: typed(STRONG),
            device_name: "Laptop".into(),
            remember_key: true,
        })
        .await
        .expect_err("enabled");
    assert_eq!(again.summary().code, "sync_already_enabled");
}

#[tokio::test]
async fn hosts_file_unmanaged_suspends_hosts_only() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    a.engine
        .save_snippet(snippet("Disk", "df -h"))
        .await
        .expect("s");
    create(&a, &folder).await;
    let hand_edit = "Host mine\n  HostName 10.2.2.2\n  ForwardAgent yes\n";
    std::fs::write(b.paths.managed_hosts(), hand_edit).expect("w");
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let codes = issue_codes(&b.engine.sync_issues().await.expect("issues"));
    assert!(
        codes.contains(&"sync_hosts_file_unmanaged".to_owned()),
        "{codes:?}"
    );
    assert_eq!(
        std::fs::read_to_string(b.paths.managed_hosts()).expect("r"),
        hand_edit,
        "never rewritten"
    );
    assert_eq!(b.engine.snippets().await.expect("s").len(), 1);
    assert!(b.engine.sync_pending_hosts().await.expect("p").is_empty());
}

#[tokio::test]
async fn passphrase_change_alerts_the_other_devices() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    a.engine
        .sync_change_passphrase(
            SecretString::from(STRONG.to_owned()),
            typed(crate::sync::passphrase::tests::STRONG_2),
        )
        .await
        .expect("change");
    assert!(matches!(sync(&a).await, SyncStatus::Idle { .. }));
    let status = sync(&b).await;
    assert!(
        matches!(
            status,
            SyncStatus::Alert {
                kind: AlertKind::PassphraseChanged,
                ..
            }
        ),
        "{status:?}"
    );
    b.engine.sync_acknowledge_alerts().await.expect("ack");
    assert!(matches!(sync(&b).await, SyncStatus::Idle { .. }));
    // B holds the key: it can reset the passphrase; A is then alerted.
    b.engine
        .sync_reset_passphrase(typed(STRONG))
        .await
        .expect("reset");
    assert!(matches!(
        sync(&a).await,
        SyncStatus::Alert {
            kind: AlertKind::PassphraseChanged,
            ..
        }
    ));
}

#[tokio::test]
async fn background_task_publishes_local_changes() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    // A restarted with the background task on.
    let a_bg = engine_on(&a.paths, Arc::clone(&a.secrets), &sim);
    a_bg.set_sync_tuning(SyncTuning {
        background: true,
        ..tuning(&sim)
    });
    a_bg.start_sync();
    let a = Device { engine: a_bg, ..a };
    a.engine
        .save_snippet(snippet("Auto", "uptime"))
        .await
        .expect("s");
    let mut found = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        sync(&b).await;
        if b.engine.snippets().await.expect("s").len() == 1 {
            found = true;
            break;
        }
    }
    assert!(found, "the debounced pass published the change");
    assert!(a.engine.shutdown_and_wait(Duration::from_secs(10)).await);
}

#[tokio::test]
async fn settings_whitelist_is_applied_and_values_are_validated() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    std::fs::write(
        a.paths.settings(),
        "theme = \"marble\"\n[keymap.gui]\npalette = \"mod+p\"\n[gui]\ncopy_on_select = true\n[updates]\ncheck = true\n",
    )
    .expect("w");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let settings = b.engine.settings().await.expect("settings");
    assert_eq!(settings.theme.as_deref(), Some("marble"));
    assert_eq!(
        settings.keymap.gui.get("palette").map(String::as_str),
        Some("mod+p")
    );
    assert!(!settings.gui.copy_on_select, "never synced (F18)");
    assert_eq!(settings.updates.check, None, "never synced");
    let conflicts = b.engine.sync_conflicts().await.expect("c");
    assert!(
        conflicts
            .iter()
            .all(|c| !matches!(c.subject, SyncSubject::Setting { .. }))
    );
}

/// A device the tests write themselves (a newer Portico, a compromised
/// device holding the key): its snapshot is sealed with the bundle's keys.
fn forge_snapshot(sim: &SimFolder, state_json: &str, device: crate::sync::DeviceId) {
    use crate::sync::container::{Binding, SnapshotContent, seal_snapshot};
    let text = sim
        .with(|s| s.files.get("bundle.toml").cloned())
        .expect("bundle");
    let text = String::from_utf8(text).expect("utf8");
    let opened = crate::sync::bundle::open(
        &text,
        &SecretString::from(STRONG.to_owned()),
        &KdfBounds::TEST,
    )
    .expect("open");
    let state: crate::sync::model::SyncState = serde_json::from_str(state_json).expect("state");
    let content = SnapshotContent {
        device_name: crate::sync::clock::DeviceName::parse("Newer").expect("name"),
        app_version: "9.0.0".into(),
        published_at: 0,
        clock: 1,
        seen: std::collections::BTreeMap::new(),
        state,
    };
    let binding = Binding {
        bundle_id: opened.header.bundle_id,
        epoch: opened.header.epoch,
        keyset_generation: opened.header.keyset_generation,
        device,
        seq: 1,
    };
    let sealed = seal_snapshot(&opened.keys, &binding, &content).expect("seal");
    sim.with(|s| {
        s.files
            .insert(format!("devices/{device}.snap"), sealed.text.into_bytes())
    });
}

#[tokio::test]
async fn held_records_from_a_newer_portico_are_kept_and_never_applied() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    b.engine
        .sync_confirm_hosts(vec!["web".into()])
        .await
        .expect("confirm");
    let newer = crate::sync::DeviceId::from_hex("ffffffffffffffffffffffffffffffff").expect("id");
    let dot = format!(r#"{{"d":"{newer}","c":1}}"#);
    forge_snapshot(
        &sim,
        &format!(
            r#"{{"context":{{"{newer}":1}},"records":[
                {{"key":{{"kind":"host","alias":"web"}},"entries":[{{"dot":{dot},"at":0,"value":{{"type":"host","v":{{"hostname":"10.6.6.6","color":"red"}}}}}}]}},
                {{"key":{{"kind":"color_scheme","name":"x"}},"entries":[{{"dot":{dot},"at":0,"value":{{"type":"tombstone"}}}}]}}
            ]}}"#
        ),
        newer,
    );
    for device in [&b, &a, &b] {
        sync(device).await;
    }
    for device in [&a, &b] {
        // Held: neither applied, nor a deletion, nor a conflict to resolve.
        assert_eq!(
            hosts(device),
            vec![("web".to_owned(), "10.0.0.1".to_owned())]
        );
        let issues = device.engine.sync_issues().await.expect("issues");
        let held: Vec<&str> = issues
            .iter()
            .filter(|i| i.summary.code == "sync_record_needs_newer")
            .map(|i| i.key.as_str())
            .collect();
        assert_eq!(held, ["host:web", "newer:color_scheme"], "{issues:?}");
        assert!(
            device
                .engine
                .sync_pending_hosts()
                .await
                .expect("p")
                .is_empty()
        );
    }
    // A local edit over a held record is refused (and retried), never lost.
    a.engine
        .update_host("web", draft("web", "10.0.0.2"))
        .await
        .expect("local edit");
    sync(&a).await;
    let codes = issue_codes(&a.engine.sync_issues().await.expect("issues"));
    assert!(
        codes
            .iter()
            .filter(|c| *c == "sync_record_needs_newer")
            .count()
            >= 2,
        "{codes:?}"
    );
    assert_eq!(hosts(&a), vec![("web".to_owned(), "10.0.0.2".to_owned())]);
}

#[tokio::test]
async fn inflated_third_party_context_is_deferred_and_reported() {
    let (_dir, folder, sim) = shared();
    let a = device(&sim);
    let b = device(&sim);
    a.engine
        .create_host(draft("web", "10.0.0.1"))
        .await
        .expect("host");
    create(&a, &folder).await;
    join(&b, &folder, JoinBulkChoice::ReviewEach).await;
    let a_id = a.engine.sync_status().await.expect("s").device.expect("id");
    let evil = crate::sync::DeviceId::from_hex("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").expect("id");
    // Claims every past and future change of A, without any value.
    forge_snapshot(
        &sim,
        &format!(r#"{{"context":{{"{evil}":1,"{a_id}":999999}},"records":[]}}"#),
        evil,
    );
    let status = sync(&a).await;
    assert!(
        matches!(&status, SyncStatus::Alert { kind: AlertKind::ThirdPartyClaim, device: Some(d) } if d.starts_with("Newer (eeeeeeee")),
        "{status:?}"
    );
    let status = sync(&b).await;
    assert!(
        matches!(
            status,
            SyncStatus::Waiting {
                reason: WaitReason::Deferred,
                ..
            }
        ),
        "{status:?}"
    );
    // Nothing was dropped: A's host survives on both devices.
    assert_eq!(hosts(&a), vec![("web".to_owned(), "10.0.0.1".to_owned())]);
    assert_eq!(b.engine.sync_pending_hosts().await.expect("p").len(), 1);
    let codes = issue_codes(&b.engine.sync_issues().await.expect("issues"));
    assert!(codes.contains(&"sync_deferred".to_owned()), "{codes:?}");
}
