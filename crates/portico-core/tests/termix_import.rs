//! Termix import (ADR "Import Termix et imports vers les hôtes Portico"):
//! the exact plan, key selection and writing, reuse of local keys, a real
//! login with an imported key, re-imports that only update what they
//! created, collisions, bounds, and the guarantee that no password or key
//! material ends up in a plan, report, error, event or file it should not.
//!
//! `tests/fixtures/termix/termix_export.json` is synthetic: RFC 5737
//! addresses, `.test` names, fake passwords (`FAKEPASS123`, `FAKEKEYPASS`)
//! and keys generated for these tests only (`ssh-keygen`, never used
//! anywhere else): an ed25519 key (OpenSSH, clear, used by two hosts), an
//! ed25519 key encrypted with `FAKEKEYPASS`, an RSA 3072 key in PKCS#1 PEM,
//! and a garbage key.
// Test helpers fail loudly; scenario tests read best in one piece; the
// fixture's JSON is edited with `serde_json::Value` indexing.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines
)]

mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use portico_core::EngineEvent;
use portico_core::import::{
    ImportOptions, ImportPlan, KeyFormat, KeyId, KeySelection, NoticeCode, TermiusFormat,
};
use portico_core::managed_hosts::HostDraft;
use russh::keys::HashAlg;
use russh::keys::ssh_key::{PrivateKey, PublicKey};
use secrecy::SecretString;
use support::{Behaviour, Env, TestServer, never};

const ED25519: &str = "SHA256:fAtep8ghWl7Uqy2Puv62F3OqOuzAcVZ7KpQw2C21KOM";
const LOCKED: &str = "SHA256:tFX16mEH5d2m/QSoYLOE37Kjmb0YVisJXVRPcL8CmDs";
const RSA: &str = "SHA256:6ktWbDQJePrgzl71bBw1B5CFNzBRJcqP6BOcMw0/Ycw";
/// Strings that must never leave the export: passwords, the key
/// passphrase, and a line of each key's body.
const SECRETS: [&str; 6] = [
    "FAKEPASS123",
    "FAKEKEYPASS",
    "PRIVATE KEY",
    "FAKEKEYMATERIAL",
    "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW",
    "AAAECJWedZVKOB40lMdbfkssHZcqWs",
];

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/termix/termix_export.json")
}

fn export() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap()
}

/// The private key text of the fixture's host at `index`.
fn fixture_key(index: usize) -> String {
    export()["hosts"][index]["key"].as_str().unwrap().to_owned()
}

fn env() -> Env {
    Env::new("")
}

fn aliases(plan: &ImportPlan) -> Vec<&str> {
    plan.hosts.iter().map(|h| h.alias.as_str()).collect()
}

fn codes(notices: &[portico_core::import::ImportNotice]) -> Vec<(Option<String>, NoticeCode)> {
    notices.iter().map(|n| (n.label.clone(), n.code)).collect()
}

fn select(ids: &[&str]) -> ImportOptions {
    ImportOptions {
        keys: KeySelection::Some(
            ids.iter()
                .map(|id| KeyId::new(*id))
                .collect::<BTreeSet<_>>(),
        ),
        ..ImportOptions::default()
    }
}

fn assert_no_secret(what: &str, text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "{secret} in {what}");
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn hosts_conf(env: &Env) -> String {
    std::fs::read_to_string(env.dir.path().join("config/hosts.conf")).unwrap_or_default()
}

/// The `Host <alias>` block of `text`.
fn block(text: &str, alias: &str) -> String {
    text.split("\n\n")
        .find(|b| b.starts_with(&format!("Host {alias}\n")))
        .unwrap_or_else(|| panic!("no {alias} in {text}"))
        .trim_end()
        .to_owned()
}

#[tokio::test]
async fn the_plan_is_exact_and_writes_nothing() {
    let env = env();
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(plan.format, TermiusFormat::TermixJson);
    assert_eq!(plan.format.label(), "Termix JSON");
    assert_eq!(plan.records, 10);
    assert_eq!(
        aliases(&plan),
        [
            "hbd",
            "hbd-backup",
            "locked-box",
            "legacy-rsa",
            "broken-key",
            "web-prod",
            "stale-key",
            "evilproxycommand-touch-tmp-pwned",
        ]
    );
    let host = |alias: &str| plan.hosts.iter().find(|h| h.alias == alias).unwrap();

    let hbd = host("hbd");
    assert_eq!(hbd.hostname, "203.0.113.20");
    assert_eq!(hbd.port, Some(2222));
    assert_eq!(hbd.user.as_deref(), Some("root"));
    assert_eq!(hbd.key.as_ref().map(KeyId::as_str), Some(ED25519));
    assert_eq!(hbd.notes.as_deref(), Some("Termix: Hbd"));
    assert_eq!(hbd.source_id, "id:1");
    assert!(!hbd.favorite && !hbd.update);
    assert_eq!(host("hbd-backup").port, None, "a missing port means 22");
    assert_eq!(host("legacy-rsa").port, Some(2200), "a port as a string");
    assert_eq!(host("legacy-rsa").hostname, "legacy.example.test");
    assert_eq!(host("broken-key").key, None);
    assert_eq!(host("stale-key").key, None, "authType password");

    let web = host("web-prod");
    assert_eq!(web.group.as_deref(), Some("Production/EU"));
    assert_eq!(web.tags, ["web", "nginx-proxy"]);
    assert!(web.favorite, "pin");
    assert_eq!(web.key, None);

    let evil = host("evilproxycommand-touch-tmp-pwned");
    assert_eq!(
        evil.label, "evilProxyCommand touch /tmp/pwned",
        "line break removed"
    );
    assert_eq!(evil.hostname, "192.0.2.66");

    assert_eq!(
        codes(&plan.skipped),
        [
            (Some("inject ip".into()), NoticeCode::InvalidHostname),
            (Some("inject user".into()), NoticeCode::InvalidUser),
        ]
    );
    assert_eq!(
        codes(&plan.warnings),
        [
            (Some("Broken key".into()), NoticeCode::KeyUnreadable),
            (Some("Web Prod".into()), NoticeCode::IgnoredSetting),
            (Some("Web Prod".into()), NoticeCode::IgnoredSetting),
            (Some("Stale key".into()), NoticeCode::KeyNotUsed),
            (Some("Web Prod".into()), NoticeCode::TagNormalized),
        ]
    );
    let settings: Vec<&str> = plan
        .warnings
        .iter()
        .filter(|w| w.code == NoticeCode::IgnoredSetting)
        .map(|w| w.message.as_str())
        .collect();
    assert_eq!(
        settings,
        [
            "`tunnelConnections` (1) was not imported: Portico imports hosts only",
            "`socks5ProxyChain` (1) was not imported: Portico imports hosts only",
        ]
    );

    let keys: Vec<_> = plan
        .keys
        .iter()
        .map(|k| {
            (
                k.id.as_str(),
                k.fingerprint.as_str(),
                k.algorithm.as_str(),
                k.format,
                k.encrypted,
                k.aliases.clone(),
                k.local.clone(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        [
            (
                ED25519,
                ED25519,
                "ssh-ed25519",
                KeyFormat::OpenSsh,
                false,
                vec!["hbd".to_owned(), "hbd-backup".to_owned()],
                None
            ),
            (
                LOCKED,
                LOCKED,
                "ssh-ed25519",
                KeyFormat::OpenSsh,
                true,
                vec!["locked-box".to_owned()],
                None
            ),
            (
                RSA,
                RSA,
                "ssh-rsa",
                KeyFormat::Pem,
                false,
                vec!["legacy-rsa".to_owned()],
                None
            ),
        ]
    );
    assert_eq!(plan.key_material.len(), 3);
    // 2 passwords + 1 key passphrase; the garbage key and the unused one
    // (not offered), the 3 offered keys (nothing ticked yet).
    assert_eq!(
        (
            plan.secrets.passwords,
            plan.secrets.keys,
            plan.secrets.unticked_keys,
            plan.secrets.unreadable_keys,
            plan.secrets.keys_in_export
        ),
        (3, 2, 0, 1, 5)
    );
    assert_eq!(
        plan.secrets.advice().as_deref(),
        Some("Not imported: 3 passwords, 1 unreadable key, 1 other key.")
    );
    assert_eq!(plan.file_name(), "termix_export.json");
    assert_eq!(
        plan.delete_export_advice().as_deref(),
        Some(
            "Delete termix_export.json now: it holds 3 passwords and 5 private keys (in clear text or encrypted)."
        )
    );

    assert_eq!(plan.target, env.dir.path().join("config/hosts.conf"));
    assert_eq!(plan.include_line, "Include ~/config/hosts.conf");
    assert_eq!(plan.replaces, None);
    assert!(!plan.target.exists());
    assert!(!env.dir.path().join("config/hosts.toml").exists());
    assert!(!env.ssh("portico").exists());
    assert!(
        !env.dir.path().join("config").exists(),
        "planning creates nothing, not even a lock file"
    );

    assert_no_secret("plan json", &serde_json::to_string(&plan).unwrap());
    assert_no_secret("plan debug", &format!("{plan:?}"));
    assert_no_secret("plan pretty debug", &format!("{plan:#?}"));
}

#[tokio::test]
async fn no_key_is_imported_unless_selected() {
    let env = env();
    let plan = env
        .engine
        .plan_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(plan.format, TermiusFormat::TermixJson, "detected");
    let report = env
        .engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.imported.len(), 8);
    assert!(report.keys.imported.is_empty() && report.keys.reused.is_empty());
    assert_eq!(report.keys.skipped, 3);
    assert_eq!(report.secrets.keys, 5, "2 unusable + 3 not selected");
    assert_eq!(
        (report.secrets.unticked_keys, report.secrets.unreadable_keys),
        (3, 1)
    );
    assert_eq!(
        report.secrets.advice().as_deref(),
        Some("Not imported: 3 passwords, 3 unticked keys, 1 unreadable key, 1 other key.")
    );
    let conf = hosts_conf(&env);
    assert!(!conf.contains("IdentityFile"), "{conf}");
    assert!(!env.ssh("portico").exists());
    assert_eq!(
        block(&conf, "hbd"),
        "Host hbd\n    HostName 203.0.113.20\n    User root\n    Port 2222"
    );
    for line in conf
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let keyword = line.split_whitespace().next().unwrap();
        assert!(
            ["Host", "HostName", "User", "Port"].contains(&keyword),
            "unexpected line {line:?}"
        );
    }
    assert!(!conf.contains("ProxyCommand"), "{conf}");

    let inventory = env.engine.load_hosts().await.unwrap();
    let web = inventory
        .hosts
        .iter()
        .find(|h| h.alias == "web-prod")
        .unwrap();
    assert!(web.meta.favorite);
    assert_eq!(web.meta.tags, ["web", "nginx-proxy"]);
    assert_eq!(web.meta.group.as_deref(), Some("Production/EU"));
    assert_eq!(web.origin, portico_core::hosts::HostOrigin::Portico);
    let meta = std::fs::read_to_string(env.dir.path().join("config/hosts.toml")).unwrap();
    assert!(
        meta.contains("[hosts.web-prod.import]\nsource = \"termix\"\nid = \"id:6\"\nvalues = \""),
        "{meta}"
    );
}

#[tokio::test]
async fn some_keys_are_written_private_and_never_overwrite() {
    let env = env();
    // A file already named like the first key, and a dangling symlink
    // named like the second: both are kept, the keys move to `-2`.
    let dir = env.ssh("portico");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.join("legacy-rsa")).unwrap();
    }
    std::fs::write(dir.join("hbd"), "keep me").unwrap();

    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    let report = env
        .engine
        .apply_import(plan, select(&[ED25519, RSA, "SHA256:unknown"]))
        .await
        .unwrap();
    assert_eq!(
        report.keys.imported,
        [dir.join("hbd-2"), dir.join("legacy-rsa-2")]
    );
    assert_eq!(report.keys.skipped, 1, "the encrypted key was not selected");
    assert_eq!(std::fs::read_to_string(dir.join("hbd")).unwrap(), "keep me");

    let conf = hosts_conf(&env);
    for alias in ["hbd", "hbd-backup"] {
        assert!(
            block(&conf, alias)
                .ends_with("\n    IdentityFile ~/.ssh/portico/hbd-2\n    IdentitiesOnly yes"),
            "{conf}"
        );
    }
    assert!(block(&conf, "legacy-rsa").contains("IdentityFile ~/.ssh/portico/legacy-rsa-2\n"));
    assert!(!block(&conf, "locked-box").contains("IdentityFile"));

    #[cfg(unix)]
    {
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("hbd-2")), 0o600);
        assert_eq!(mode(&dir.join("legacy-rsa-2")), 0o600);
    }
    for (file, fingerprint) in [("hbd-2", ED25519), ("legacy-rsa-2", RSA)] {
        let private = PrivateKey::read_openssh_file(dir.join(file)).unwrap();
        assert!(!private.is_encrypted(), "no passphrase given");
        assert_eq!(
            private
                .public_key()
                .fingerprint(HashAlg::Sha256)
                .to_string(),
            fingerprint
        );
        let public = std::fs::read_to_string(dir.join(format!("{file}.pub"))).unwrap();
        let public = PublicKey::from_openssh(public.trim()).unwrap();
        assert_eq!(
            public.key_data(),
            private.public_key().key_data(),
            "{file}.pub"
        );
    }

    // Applying again: the copies are reused (same fingerprint under
    // ~/.ssh/portico), nothing new is written.
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        plan.keys.first().and_then(|k| k.local.clone()),
        Some(dir.join("hbd-2"))
    );
    let report = env
        .engine
        .apply_import(plan, select(&[ED25519, RSA]))
        .await
        .unwrap();
    assert!(report.keys.imported.is_empty());
    assert_eq!(
        report.keys.reused,
        [dir.join("hbd-2"), dir.join("legacy-rsa-2")]
    );
    assert!(report.unchanged, "{report:?}");
    assert!(!dir.join("hbd-3").exists());
}

#[tokio::test]
async fn all_keys_with_a_passphrase() {
    let env = env();
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    let mut options = select(&[ED25519, LOCKED, RSA]);
    options.key_passphrase = Some(SecretString::from("correct horse"));
    let report = env.engine.apply_import(plan, options).await.unwrap();
    let dir = env.ssh("portico");
    assert_eq!(
        report.keys.imported,
        [
            dir.join("hbd"),
            dir.join("locked-box"),
            dir.join("legacy-rsa")
        ]
    );
    assert_eq!(report.keys.skipped, 0);
    assert_eq!(report.secrets.keys, 2, "the two unusable keys");
    assert_eq!(report.secrets.unticked_keys, 0);

    // Clear keys are encrypted with the passphrase given…
    for (file, fingerprint) in [("hbd", ED25519), ("legacy-rsa", RSA)] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(text.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
        let private = PrivateKey::from_openssh(&text).unwrap();
        assert!(private.is_encrypted(), "{file}");
        assert!(private.decrypt("wrong").is_err());
        let clear = private.decrypt("correct horse").unwrap();
        assert_eq!(
            clear.public_key().fingerprint(HashAlg::Sha256).to_string(),
            fingerprint
        );
    }
    // …and the key already encrypted in the export is written as it is.
    let locked = std::fs::read_to_string(dir.join("locked-box")).unwrap();
    assert_eq!(locked, fixture_key(2));
    let private = PrivateKey::from_openssh(&locked).unwrap();
    assert!(private.decrypt("correct horse").is_err());
    assert_eq!(
        private
            .decrypt("FAKEKEYPASS")
            .unwrap()
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string(),
        LOCKED
    );
    assert!(
        block(&hosts_conf(&env), "locked-box").contains("IdentityFile ~/.ssh/portico/locked-box\n")
    );
}

#[tokio::test]
async fn a_local_key_with_the_same_fingerprint_is_reused() {
    let env = env();
    std::fs::write(env.ssh("id_work"), fixture_key(0)).unwrap();
    let private = PrivateKey::from_openssh(fixture_key(0)).unwrap();
    std::fs::write(
        env.ssh("id_work.pub"),
        private.public_key().to_openssh().unwrap(),
    )
    .unwrap();
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        plan.keys.first().map(|k| (k.id.as_str(), k.local.clone())),
        Some((ED25519, Some(env.ssh("id_work"))))
    );
    let report = env
        .engine
        .apply_import(plan, select(&[ED25519]))
        .await
        .unwrap();
    assert_eq!(report.keys.reused, [env.ssh("id_work")]);
    assert!(report.keys.imported.is_empty());
    assert!(!env.ssh("portico").exists(), "nothing written");
    assert!(
        block(&hosts_conf(&env), "hbd")
            .ends_with("\n    IdentityFile ~/.ssh/id_work\n    IdentitiesOnly yes")
    );
}

#[tokio::test]
async fn a_session_logs_in_with_an_imported_key() {
    let private = PrivateKey::from_openssh(fixture_key(0)).unwrap();
    let server = TestServer::start(Behaviour {
        keys: vec![private.public_key().clone()],
        ..Behaviour::default()
    })
    .await;
    let mut env = env();
    env.trust(&server);
    let mut export = export();
    let first = export["hosts"][0].clone();
    let mut host = first;
    host["ip"] = "127.0.0.1".into();
    host["port"] = server.port.into();
    host["username"] = "alice".into();
    export["hosts"] = serde_json::Value::Array(vec![host]);
    let file = env.dir.path().join("termix.json");
    std::fs::write(&file, serde_json::to_string(&export).unwrap()).unwrap();

    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    let report = env
        .engine
        .apply_import(plan, select(&[ED25519]))
        .await
        .unwrap();
    assert_eq!(report.imported, ["hbd"]);
    // No other key exists: only the imported one can open the session.
    let (_, outcome) = env.connect("hbd", never).await;
    assert!(outcome.connected(), "{:?}", outcome.phases);
    assert_eq!(
        server
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_no_secret("session events", &outcome.transcript());
}

#[tokio::test]
async fn a_reimport_only_updates_hosts_it_created_and_nobody_edited() {
    let env = env();
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    env.engine
        .apply_import(plan, select(&[ED25519]))
        .await
        .unwrap();

    // `hbd` is edited by hand; `locked-box` only gets a tag (metadata is
    // not an edit); a host is created by hand under a name the next
    // export will use.
    env.engine
        .update_host(
            "hbd",
            HostDraft {
                alias: "hbd".into(),
                hostname: "10.9.9.9".into(),
                user: Some("root".into()),
                port: Some(2222),
                ..HostDraft::default()
            },
        )
        .await
        .unwrap();
    env.engine
        .set_tags("locked-box", vec!["mine".into()])
        .await
        .unwrap();
    env.engine
        .create_host(HostDraft {
            alias: "manual".into(),
            hostname: "10.0.0.7".into(),
            ..HostDraft::default()
        })
        .await
        .unwrap();

    let mut export = export();
    let hosts = export["hosts"].as_array_mut().unwrap();
    for host in hosts.iter_mut() {
        host["ip"] = match host["name"].as_str() {
            Some("Hbd") => "203.0.113.120".into(),
            Some("Locked box") => "203.0.113.122".into(),
            _ => host["ip"].clone(),
        };
    }
    hosts.push(serde_json::json!({"name": "manual", "ip": "203.0.113.200"}));
    let file = env.dir.path().join("termix-2.json");
    std::fs::write(&file, serde_json::to_string(&export).unwrap()).unwrap();

    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    assert!(!aliases(&plan).contains(&"hbd"));
    assert!(!aliases(&plan).contains(&"manual"));
    let skipped = codes(&plan.skipped);
    assert!(skipped.contains(&(Some("Hbd".into()), NoticeCode::EditedSinceImport)));
    assert!(skipped.contains(&(Some("manual".into()), NoticeCode::AliasCollision)));
    assert!(plan.hosts.iter().all(|h| h.update), "{:?}", plan.hosts);
    // Counts only the hosts written as they are: not `hbd` (edited, so
    // skipped) nor `locked-box` (a destination change, skipped unless
    // accepted).
    assert_eq!(plan.hosts.len(), 7);
    assert_eq!(plan.replaces, Some(6));
    assert_eq!(plan.updates(&ImportOptions::default()), 6);
    assert_eq!(
        plan.updates(&ImportOptions {
            allow_redirect: ["locked-box".to_owned()].into(),
            ..ImportOptions::default()
        }),
        7
    );
    // `locked-box` now points elsewhere: a destination change, applied
    // only with the user's consent for that alias.
    let redirected: Vec<&str> = plan
        .hosts
        .iter()
        .filter(|h| h.previous.is_some())
        .map(|h| h.alias.as_str())
        .collect();
    assert_eq!(redirected, ["locked-box"]);

    let mut events = env.engine.subscribe();
    let report = env
        .engine
        .apply_import(
            plan,
            ImportOptions {
                allow_redirect: ["locked-box".to_owned()].into(),
                ..ImportOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.updated, report.imported);
    assert!(matches!(
        events.recv().await.unwrap(),
        EngineEvent::HostsChanged
    ));

    let conf = hosts_conf(&env);
    assert!(
        block(&conf, "hbd").contains("HostName 10.9.9.9\n"),
        "edit kept"
    );
    assert_eq!(
        block(&conf, "hbd-backup"),
        "Host hbd-backup\n    HostName 203.0.113.21\n    User root\n    IdentityFile ~/.ssh/portico/hbd\n    IdentitiesOnly yes",
        "same destination: its key kept although not selected again"
    );
    assert!(block(&conf, "locked-box").contains("HostName 203.0.113.122\n"));
    assert_eq!(block(&conf, "manual"), "Host manual\n    HostName 10.0.0.7");
    let inventory = env.engine.load_hosts().await.unwrap();
    let locked = inventory
        .hosts
        .iter()
        .find(|h| h.alias == "locked-box")
        .unwrap();
    assert_eq!(locked.meta.tags, ["mine"], "user tags kept");

    // The same export once more: nothing changes, nothing is written.
    let before = (
        std::fs::read(env.dir.path().join("config/hosts.conf")).unwrap(),
        std::fs::read(env.dir.path().join("config/hosts.toml")).unwrap(),
    );
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    let report = env
        .engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    assert!(report.unchanged);
    assert_eq!(report.meta_updated, 0);
    assert_eq!(
        (
            std::fs::read(env.dir.path().join("config/hosts.conf")).unwrap(),
            std::fs::read(env.dir.path().join("config/hosts.toml")).unwrap(),
        ),
        before
    );

    // Deleting the edited host and importing again creates it afresh.
    env.engine.delete_host("hbd").await.unwrap();
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    let hbd = plan.hosts.iter().find(|h| h.alias == "hbd").unwrap();
    assert!(!hbd.update);
}

#[tokio::test]
async fn collisions_are_skipped_or_prefixed() {
    let env = Env::new("Host web-prod HBD\n    HostName 10.0.0.1\n");
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    let collisions: Vec<_> = plan
        .skipped
        .iter()
        .filter(|n| n.code == NoticeCode::AliasCollision)
        .map(|n| n.label.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(collisions, ["Hbd", "Web Prod"], "case-insensitive");
    assert_eq!(
        plan.keys.first().map(|k| k.aliases.clone()),
        Some(vec!["hbd-backup".to_owned()])
    );

    let plan = env
        .engine
        .plan_termix_import(
            fixture(),
            ImportOptions {
                prefix: true,
                ..ImportOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(plan.hosts.len(), 8);
    assert!(aliases(&plan).iter().all(|a| a.starts_with("termix-")));
    let report = env
        .engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    assert!(report.imported.contains(&"termix-web-prod".to_owned()));
    assert_eq!(
        std::fs::read_to_string(env.ssh("config")).unwrap(),
        "Host web-prod HBD\n    HostName 10.0.0.1\n",
        "never written"
    );
}

#[tokio::test]
async fn bounds_and_malformed_exports_are_typed_errors() {
    let env = env();
    let root = env.dir.path();
    let plan = |path: PathBuf| {
        let engine = env.engine.clone();
        async move {
            engine
                .plan_termix_import(path, ImportOptions::default())
                .await
                .expect_err("refused")
        }
    };

    let many = root.join("many.json");
    let hosts: Vec<String> = (0..=portico_core::import::MAX_IMPORT_RECORDS)
        .map(|i| format!("{{\"name\":\"h{i}\",\"ip\":\"192.0.2.1\"}}"))
        .collect();
    std::fs::write(&many, format!("{{\"hosts\":[{}]}}", hosts.join(","))).unwrap();
    assert_eq!(plan(many).await.summary().code, "import_too_many");

    let big = root.join("big.json");
    let size = usize::try_from(portico_core::import::MAX_IMPORT_FILE_BYTES).unwrap() + 1;
    std::fs::write(
        &big,
        format!("{{\"hosts\":[{{\"ip\":\"{}\"}}]}}", "x".repeat(size)),
    )
    .unwrap();
    assert_eq!(plan(big).await.summary().code, "import_too_large");

    let broken = root.join("broken.json");
    std::fs::write(
        &broken,
        "{\"hosts\":[{\"name\":\"a\",\"ip\":\"192.0.2.1\",\"password\":\"FAKEPASS123\" ",
    )
    .unwrap();
    let err = plan(broken).await;
    assert_eq!(err.summary().code, "import_json");
    assert_no_secret("error", &format!("{err:?} {err} {:?}", err.summary()));

    let typed = root.join("typed.json");
    std::fs::write(&typed, "{\"hosts\":{\"ip\":\"FAKEPASS123\"}}").unwrap();
    let err = plan(typed).await;
    assert_eq!(err.summary().code, "import_unknown_format");
    assert_no_secret("error", &format!("{err:?} {err} {:?}", err.summary()));

    let csv = root.join("termius.csv");
    std::fs::write(&csv, "Label,Hostname/IP\nweb,192.0.2.1\n").unwrap();
    assert_eq!(plan(csv).await.summary().code, "import_unknown_format");
}

#[tokio::test]
async fn no_secret_reaches_reports_files_events_or_errors() {
    let env = env();
    let mut events = env.engine.subscribe();
    let plan = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .unwrap();
    let mut options = select(&[ED25519, LOCKED, RSA]);
    options.key_passphrase = Some(SecretString::from("FAKEKEYPASS-new"));
    assert_no_secret("options debug", &format!("{options:?}"));
    assert_no_secret("options json", &serde_json::to_string(&options).unwrap());
    let report = env.engine.apply_import(plan, options).await.unwrap();
    assert_no_secret("report json", &serde_json::to_string(&report).unwrap());
    assert_no_secret("report debug", &format!("{report:?}"));
    assert_no_secret("hosts.conf", &hosts_conf(&env));
    assert_no_secret(
        "hosts.toml",
        &std::fs::read_to_string(env.dir.path().join("config/hosts.toml")).unwrap(),
    );
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert!(!seen.is_empty());
    assert_no_secret("events", &serde_json::to_string(&seen).unwrap());

    // A failing apply (hosts.conf edited by hand with a directive Portico
    // does not manage) names the line, never a value, and writes no key.
    let env = support::Env::new("");
    std::fs::create_dir_all(env.dir.path().join("config")).unwrap();
    std::fs::write(
        env.dir.path().join("config/hosts.conf"),
        "Host x\n    HostName 192.0.2.1\n    ProxyCommand nc %h %p\n",
    )
    .unwrap();
    let err = env
        .engine
        .plan_termix_import(fixture(), ImportOptions::default())
        .await
        .expect_err("unmanaged");
    assert_eq!(err.summary().code, "host_file_unmanaged");
    assert_no_secret("error", &format!("{err:?} {:?}", err.summary()));
    assert!(!env.ssh("portico").exists());
}

/// Writes a Termix export holding `hosts` and returns its path.
fn write_export(env: &Env, name: &str, hosts: &[serde_json::Value]) -> PathBuf {
    let file = env.dir.path().join(name);
    let export = serde_json::json!({ "hosts": hosts });
    std::fs::write(&file, serde_json::to_string(&export).unwrap()).unwrap();
    file
}

/// A re-import that would point an alias at another server is a
/// destination change: listed with the previous values, skipped unless
/// accepted for that alias, and then never keeps the old key. An alias
/// imported from another record is never updated.
#[tokio::test]
async fn reimport_never_redirects_an_alias_silently() {
    let env = env();
    let mut hbd = export()["hosts"][0].clone();
    let first = write_export(&env, "one.json", &[hbd.clone()]);
    let plan = env
        .engine
        .plan_termix_import(&first, ImportOptions::default())
        .await
        .unwrap();
    env.engine
        .apply_import(plan, select(&[ED25519]))
        .await
        .unwrap();
    let before = hosts_conf(&env);
    assert!(block(&before, "hbd").contains("IdentityFile ~/.ssh/portico/hbd"));

    // Same record, another server.
    hbd["ip"] = "198.51.100.99".into();
    let moved = write_export(&env, "moved.json", &[hbd.clone()]);
    let plan = env
        .engine
        .plan_termix_import(&moved, ImportOptions::default())
        .await
        .unwrap();
    let planned = plan.hosts.first().unwrap();
    assert!(planned.update);
    // Not counted as an update until accepted.
    assert_eq!(plan.replaces, None);
    assert_eq!(plan.updates(&ImportOptions::default()), 0);
    let previous = planned.previous.as_ref().expect("a destination change");
    assert_eq!(
        (previous.hostname.as_str(), previous.port),
        ("203.0.113.20", Some(2222))
    );
    assert!(
        codes(&plan.warnings).contains(&(Some("Hbd".into()), NoticeCode::DestinationChanged)),
        "{:?}",
        plan.warnings
    );

    // By default: skipped, nothing written.
    let report = env
        .engine
        .apply_import(plan.clone(), ImportOptions::default())
        .await
        .unwrap();
    assert!(report.imported.is_empty(), "{report:?}");
    assert_eq!(
        codes(&report.skipped),
        [(Some("Hbd".into()), NoticeCode::DestinationChanged)]
    );
    assert!(report.unchanged);
    assert_eq!(hosts_conf(&env), before);

    // Another record under the same alias (other id, same name and
    // server): never taken for this host.
    let mut other = hbd.clone();
    other["id"] = 99.into();
    other["ip"] = "203.0.113.20".into();
    let impostor = write_export(&env, "impostor.json", &[other]);
    let plan_other = env
        .engine
        .plan_termix_import(&impostor, ImportOptions::default())
        .await
        .unwrap();
    assert!(plan_other.hosts.is_empty(), "{:?}", plan_other.hosts);
    assert_eq!(
        codes(&plan_other.skipped),
        [(Some("Hbd".into()), NoticeCode::AliasCollision)]
    );
    let report = env
        .engine
        .apply_import(plan_other, ImportOptions::default())
        .await
        .unwrap();
    assert!(report.imported.is_empty());
    assert_eq!(hosts_conf(&env), before);

    // Accepted for this alias: pointed elsewhere, without the old key.
    let report = env
        .engine
        .apply_import(
            plan,
            ImportOptions {
                allow_redirect: ["hbd".to_owned()].into(),
                ..ImportOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.updated, ["hbd"]);
    assert_eq!(
        block(&hosts_conf(&env), "hbd"),
        "Host hbd\n    HostName 198.51.100.99\n    User root\n    Port 2222",
        "the key of the old server is not carried along"
    );
}

/// The encrypted OpenSSH text of `theirs` with the clear public half of
/// `claimed` spliced in (both ed25519: same length).
fn forged_key(theirs: &PrivateKey, claimed: &PrivateKey) -> String {
    use base64::Engine as _;
    let locked = theirs
        .encrypt(&mut russh::keys::key::safe_rng(), "FAKEKEYPASS")
        .unwrap()
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap();
    let body: String = locked.lines().filter(|l| !l.starts_with("-----")).collect();
    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(body)
        .unwrap();
    let from = theirs.public_key().key_data().ed25519().unwrap().0;
    let to = claimed.public_key().key_data().ed25519().unwrap().0;
    let at = bytes
        .windows(32)
        .position(|w| w == from)
        .expect("public half in clear");
    bytes[at..at + 32].copy_from_slice(&to);
    format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    )
}

fn random_key() -> PrivateKey {
    PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap()
}

fn key_host(id: u32, name: &str, key: &str) -> serde_json::Value {
    serde_json::json!({"id": id, "name": name, "ip": "203.0.113.30", "username": "root",
        "authType": "key", "key": key})
}

/// An encrypted OpenSSH private key whose clear public half claims to be
/// the user's own key (the private half is another key): the fingerprint
/// is marked unverified and never selects the local key; a verified copy
/// of the same id in the export wins; a `.pub` beside another private key
/// never lends it its fingerprint.
#[tokio::test]
async fn a_claimed_fingerprint_never_reuses_a_local_key() {
    let (mine, theirs) = (random_key(), random_key());
    let claimed = mine.public_key().fingerprint(HashAlg::Sha256).to_string();
    let forged = forged_key(&theirs, &mine);

    let env = env();
    env.write_key("id_mine", &mine, None);
    let file = write_export(&env, "forged.json", &[key_host(1, "victim", &forged)]);
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    let key = plan.keys.first().unwrap();
    assert_eq!(key.fingerprint, claimed, "the export claims it");
    assert!(key.encrypted && !key.fingerprint_verified);
    assert_eq!(key.local, None, "never matched with ~/.ssh/id_mine");
    let report = env
        .engine
        .apply_import(plan, select(&[claimed.as_str()]))
        .await
        .unwrap();
    assert!(report.keys.reused.is_empty(), "{:?}", report.keys);
    assert_eq!(report.keys.imported, [env.ssh("portico/victim")]);
    assert!(
        block(&hosts_conf(&env), "victim").contains("IdentityFile ~/.ssh/portico/victim\n"),
        "{}",
        hosts_conf(&env)
    );

    // The same id twice: the clear (verified) copy wins over the claim,
    // and it may then reuse the local key.
    let env = Env::new("");
    env.write_key("id_mine", &mine, None);
    let clear = mine
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap()
        .to_string();
    let file = write_export(
        &env,
        "both.json",
        &[
            key_host(1, "forged", &forged),
            key_host(2, "honest", &clear),
        ],
    );
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(plan.keys.len(), 1);
    let key = plan.keys.first().unwrap();
    assert!(key.fingerprint_verified && !key.encrypted);
    assert_eq!(key.local, Some(env.ssh("id_mine")));

    // A `.pub` of `mine` beside another private key: not a local copy.
    let env = Env::new("");
    env.write_key("id_other", &theirs, None);
    std::fs::write(
        env.ssh("id_other.pub"),
        mine.public_key().to_openssh().unwrap(),
    )
    .unwrap();
    let file = write_export(&env, "clear.json", &[key_host(2, "honest", &clear)]);
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(plan.keys.first().unwrap().local, None);
}

/// Short aliases that would shadow a name the configuration relies on get
/// the prefix: `localhost`, a `ProxyJump` host, a dotless `HostName`.
#[tokio::test]
async fn aliases_used_as_jump_hosts_get_the_prefix() {
    let env = Env::new(
        "Host app\n    HostName 10.0.0.5\n    ProxyJump gw\n\nHost db\n    HostName dbserver\n",
    );
    let host = |id: u32, name: &str, ip: &str| serde_json::json!({"id": id, "name": name, "ip": ip, "username": "root"});
    let file = write_export(
        &env,
        "short.json",
        &[
            host(1, "gw", "203.0.113.50"),
            host(2, "localhost", "203.0.113.51"),
            host(3, "dbserver", "203.0.113.52"),
            host(4, "plain", "203.0.113.53"),
            host(5, "self", "self"),
        ],
    );
    let plan = env
        .engine
        .plan_termix_import(&file, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(
        aliases(&plan),
        [
            "termix-gw",
            "termix-localhost",
            "termix-dbserver",
            "plain",
            "self"
        ]
    );
    let shadowing: Vec<_> = plan
        .warnings
        .iter()
        .filter(|w| w.code == NoticeCode::AliasLooksLikeHost)
        .map(|w| w.label.clone().unwrap_or_default())
        .collect();
    assert_eq!(shadowing, ["gw", "localhost", "dbserver"]);

    // Checked again when applying a plan edited by hand.
    let mut plan = plan;
    if let Some(first) = plan.hosts.first_mut() {
        first.alias = "gw".into();
    }
    let report = env
        .engine
        .apply_import(plan, ImportOptions::default())
        .await
        .unwrap();
    assert!(
        report.imported.contains(&"termix-gw".to_owned()),
        "{report:?}"
    );
    assert!(!report.imported.contains(&"gw".to_owned()));
    let conf = hosts_conf(&env);
    assert!(!conf.contains("Host gw\n") && !conf.contains("Host localhost\n"));
}
