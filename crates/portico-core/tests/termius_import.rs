//! Termius import (Phase 5): plan, apply, idempotence, collisions, and
//! the guarantee that no secret of the export ends up anywhere.
//!
//! Fixtures (`tests/fixtures/termius/`) are synthetic: RFC 5737 / 3849
//! addresses, `.test` names and fake secrets (`FAKEPASS123`, `123456`).
// Test helpers fail loudly; the end-to-end apply test reads best in one piece.
#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use portico_core::import::{ImportOptions, ImportPlan, NoticeCode, TermiusFormat};
use portico_core::store::Paths;
use portico_core::{Engine, EngineConfig};

const SECRET: &str = "FAKEPASS123";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/termius")
        .join(name)
}

fn engine_with(paths: Paths) -> Engine {
    Engine::new(EngineConfig {
        paths,
        local_user: "alice".into(),
        event_capacity: 256,
        secrets: Arc::new(portico_core::secrets::MemoryStore::default()),
        agent_socket: None,
        connect_timeout: portico_core::engine::DEFAULT_CONNECT_TIMEOUT,
        keepalive: portico_core::engine::DEFAULT_KEEPALIVE,
    })
}

fn engine(root: &Path) -> Engine {
    engine_with(Paths::rooted(root))
}

fn aliases(plan: &ImportPlan) -> Vec<&str> {
    plan.hosts.iter().map(|h| h.alias.as_str()).collect()
}

fn host<'a>(plan: &'a ImportPlan, alias: &str) -> &'a portico_core::import::PlannedHost {
    plan.hosts
        .iter()
        .find(|h| h.alias == alias)
        .unwrap_or_else(|| panic!("no {alias} in {:?}", aliases(plan)))
}

fn codes(notices: &[portico_core::import::ImportNotice]) -> Vec<(Option<String>, NoticeCode)> {
    notices.iter().map(|n| (n.label.clone(), n.code)).collect()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

#[tokio::test]
async fn template_csv_plan() {
    let raw = std::fs::read(fixture("termius_template.csv")).expect("fixture");
    assert!(raw.starts_with(b"\xEF\xBB\xBF"), "fixture keeps its BOM");
    assert!(raw.windows(2).any(|w| w == b"\r\n"), "fixture keeps CRLF");

    let dir = tempfile::tempdir().expect("tempdir");
    let engine = engine(dir.path());
    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");

    assert_eq!(plan.format, TermiusFormat::TemplateCsv);
    assert_eq!(plan.records, 9);
    assert_eq!(
        aliases(&plan),
        [
            "web-eu-1",
            "db-eu-1",
            "termius-at-198.51.100.7",
            "web-eu-1-2",
            "v6-box",
            "bad-port"
        ]
    );

    let web = host(&plan, "web-eu-1");
    assert_eq!(web.hostname, "203.0.113.10");
    assert_eq!(web.user.as_deref(), Some("deploy"));
    assert_eq!(web.port, None, "22 is left out");
    assert_eq!(web.group.as_deref(), Some("Production/EU"));
    assert_eq!(web.tags, ["web", "nginx"]);
    assert_eq!(web.notes.as_deref(), Some("Termius: web eu 1"));

    let db = host(&plan, "db-eu-1");
    assert_eq!(db.port, Some(2222));
    assert_eq!(db.tags, ["db", "postgres"]);
    assert_eq!(db.notes, None, "alias equals the label");

    let at = host(&plan, "termius-at-198.51.100.7");
    assert_eq!(
        (at.hostname.as_str(), at.port),
        ("198.51.100.7", Some(38444))
    );
    assert_eq!(at.group.as_deref(), Some("Production"));

    let staging = host(&plan, "web-eu-1-2");
    assert_eq!(staging.hostname, "staging.example.test");
    assert_eq!(staging.port, None);
    assert_eq!(staging.tags, ["web-serveur"]);
    assert_eq!(staging.group.as_deref(), Some("Staging"));

    let v6 = host(&plan, "v6-box");
    assert_eq!(v6.hostname, "2001:db8::10");
    assert_eq!(v6.tags, ["ipv6"]);

    let bad_port = host(&plan, "bad-port");
    assert_eq!(bad_port.port, None);

    assert_eq!(
        codes(&plan.skipped),
        [
            (
                Some("legacy-switch".into()),
                NoticeCode::UnsupportedProtocol
            ),
            (Some("no-host".into()), NoticeCode::MissingHostname),
            (Some("inject".into()), NoticeCode::InvalidHostname),
        ]
    );
    let warnings = codes(&plan.warnings);
    for expected in [
        (Some("web eu 1".into()), NoticeCode::TagNormalized),
        (Some("web eu 1".into()), NoticeCode::AliasRenamed),
        (Some("bad-port".into()), NoticeCode::InvalidPort),
    ] {
        assert!(warnings.contains(&expected), "{expected:?} in {warnings:?}");
    }
    let tag_warning = plan
        .warnings
        .iter()
        .find(|w| w.code == NoticeCode::TagNormalized)
        .expect("tag warning");
    assert!(
        tag_warning.message.contains("`web-serveur`"),
        "{tag_warning:?}"
    );

    assert_eq!((plan.secrets.passwords, plan.secrets.keys), (2, 1));
    let advice = plan.secrets.advice().expect("advice");
    assert!(
        advice.starts_with("2 passwords and 1 key ignored"),
        "{advice}"
    );
    assert!(plan.identity_files.is_empty());

    // Planning writes nothing.
    assert!(!plan.target.exists());
    assert!(!dir.path().join("config/hosts.toml").exists());
    assert_eq!(plan.target, dir.path().join("config/hosts.conf"));
    assert_eq!(plan.include_line, "Include ~/config/hosts.conf");
    assert!(plan.keys.is_empty(), "Termius exports never offer keys");
    assert!(plan.hosts.iter().all(|h| !h.update && h.key.is_none()));
}

#[tokio::test]
async fn apply_writes_only_safe_directives_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");
    let report = engine
        .apply_import(plan.clone(), ImportOptions::default())
        .await
        .expect("apply");

    let target = root.join("config/hosts.conf");
    assert_eq!(report.target, target);
    assert!(report.updated.is_empty());
    assert!(!report.unchanged);
    assert_eq!(report.imported.len(), 6);
    assert_eq!(report.meta_updated, 6);
    assert!(
        !report.include_active,
        "~/.ssh/config does not include it yet"
    );
    let text = std::fs::read_to_string(&target).expect("conf");
    #[cfg(unix)]
    {
        assert_eq!(mode(&target), 0o600);
        assert_eq!(mode(&root.join("config")), 0o700);
    }
    assert!(
        text.starts_with("# Hosts created in Portico. This file is managed by Portico"),
        "{text}"
    );
    assert!(text.contains("#   Include ~/config/hosts.conf\n"), "{text}");
    let body = text.split_once("\n\n").map(|(_, b)| b).expect("body");
    assert_eq!(
        body,
        "Host web-eu-1\n    HostName 203.0.113.10\n    User deploy\n\n\
         Host db-eu-1\n    HostName 203.0.113.11\n    User postgres\n    Port 2222\n\n\
         Host termius-at-198.51.100.7\n    HostName 198.51.100.7\n    User root\n    Port 38444\n\n\
         Host web-eu-1-2\n    HostName staging.example.test\n    User ubuntu\n\n\
         Host v6-box\n    HostName 2001:db8::10\n    User ops\n\n\
         Host bad-port\n    HostName 192.0.2.60\n    User root\n"
    );
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let keyword = line.split_whitespace().next().unwrap_or_default();
        assert!(
            ["Host", "HostName", "User", "Port"].contains(&keyword),
            "unexpected directive: {line}"
        );
    }
    assert!(!text.contains("ProxyCommand") && !text.contains("evil"));
    assert!(!text.contains("StrictHostKeyChecking") && !text.contains("IdentityFile"));

    let meta = std::fs::read_to_string(root.join("config/hosts.toml")).expect("hosts.toml");
    let meta: portico_core::hosts::HostMetaFile = toml::from_str(&meta).expect("toml");
    let web = meta.hosts.get("web-eu-1").expect("web");
    assert_eq!(web.group.as_deref(), Some("Production/EU"));
    assert_eq!(web.tags, ["web", "nginx"]);
    assert_eq!(web.notes.as_deref(), Some("Termius: web eu 1"));
    assert_eq!(
        meta.hosts.get("web-eu-1-2").expect("staging").tags,
        ["web-serveur"]
    );
    assert_eq!(meta.hosts.get("db-eu-1").expect("db").notes, None);

    // Portico hosts: loaded at once, editable; with the Include line too.
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        format!(
            "{}\n\nHost *\n    ServerAliveInterval 30\n",
            report.include_line
        ),
    )
    .expect("config");
    let inventory = engine.load_hosts().await.expect("load");
    let names: Vec<&str> = inventory.hosts.iter().map(|h| h.alias.as_str()).collect();
    assert_eq!(
        names,
        report
            .imported
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let db = inventory
        .hosts
        .iter()
        .find(|h| h.alias == "db-eu-1")
        .expect("db");
    assert_eq!(db.resolved.hostname, "203.0.113.11");
    assert_eq!(db.resolved.port, 2222);
    assert_eq!(db.resolved.user.as_deref(), Some("postgres"));
    assert_eq!(db.origin, portico_core::hosts::HostOrigin::Portico);
    assert_eq!(db.meta.tags, ["db", "postgres"]);

    // Re-import of the same export: no collision with its own previous
    // import, nothing rewritten.
    let before_conf = std::fs::read(&target).expect("conf");
    let before_meta = std::fs::read(root.join("config/hosts.toml")).expect("meta");
    let again = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan again");
    assert_eq!(aliases(&again), aliases(&plan));
    assert!(
        again.hosts.iter().all(|h| h.update),
        "updates its own hosts"
    );
    assert_eq!(again.replaces, Some(6));
    assert!(
        again
            .skipped
            .iter()
            .all(|n| n.code != NoticeCode::AliasCollision)
    );
    let report = engine
        .apply_import(again, ImportOptions::default())
        .await
        .expect("apply again");
    assert!(report.unchanged);
    assert!(report.include_active);
    assert_eq!(report.meta_updated, 0);
    assert_eq!(std::fs::read(&target).expect("conf"), before_conf);
    assert_eq!(
        std::fs::read(root.join("config/hosts.toml")).expect("meta"),
        before_meta
    );
}

#[tokio::test]
async fn collisions_are_skipped_or_prefixed_and_user_metadata_is_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        "Host db-eu-1 V6-BOX\n    HostName 10.0.0.1\n",
    )
    .expect("config");
    std::fs::create_dir_all(root.join("config")).expect("mkdir");
    std::fs::write(
        root.join("config/hosts.toml"),
        "# mine\n[hosts.web-eu-1]\ntags = [\"prod\", \"WEB\"]\ngroup = \"Mine\"\nnotes = \"keep me\"\nfavorite = true\n",
    )
    .expect("meta");
    let engine = engine(root);

    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(
        aliases(&plan),
        [
            "web-eu-1",
            "termius-at-198.51.100.7",
            "web-eu-1-2",
            "bad-port"
        ]
    );
    let collisions: Vec<_> = plan
        .skipped
        .iter()
        .filter(|n| n.code == NoticeCode::AliasCollision)
        .map(|n| n.label.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(collisions, ["db-eu-1", "v6-box"], "case-insensitive");

    engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    let text = std::fs::read_to_string(root.join("config/hosts.toml")).expect("meta");
    assert!(text.starts_with("# mine\n"), "{text}");
    let meta: portico_core::hosts::HostMetaFile = toml::from_str(&text).expect("toml");
    let web = meta.hosts.get("web-eu-1").expect("web");
    assert_eq!(web.tags, ["prod", "WEB", "nginx"], "union, existing first");
    assert_eq!(web.group.as_deref(), Some("Mine"));
    assert_eq!(web.notes.as_deref(), Some("keep me"));
    assert!(web.favorite);
    // The user's own config was not touched.
    assert_eq!(
        std::fs::read_to_string(root.join(".ssh/config")).expect("config"),
        "Host db-eu-1 V6-BOX\n    HostName 10.0.0.1\n"
    );

    // With the prefix, every host is imported.
    let plan = engine
        .plan_termius_import(
            fixture("termius_template.csv"),
            ImportOptions {
                prefix: true,
                ..ImportOptions::default()
            },
        )
        .await
        .expect("plan");
    assert_eq!(
        aliases(&plan),
        [
            "termius-web-eu-1",
            "termius-db-eu-1",
            "termius-at-198.51.100.7",
            "termius-web-eu-1-2",
            "termius-v6-box",
            "termius-bad-port"
        ]
    );
    assert_eq!(
        host(&plan, "termius-db-eu-1").notes.as_deref(),
        Some("Termius: db-eu-1")
    );
    assert!(
        plan.skipped
            .iter()
            .all(|n| n.code != NoticeCode::AliasCollision)
    );
}

#[tokio::test]
async fn an_alias_defined_after_planning_is_not_shadowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let plan = engine
        .plan_termius_import(fixture("sshconfig"), ImportOptions::default())
        .await
        .expect("plan");
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(root.join(".ssh/config"), "Host db\n    HostName 10.0.0.9\n").expect("w");
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert_eq!(report.imported, ["prod-one"]);
    assert!(
        report
            .skipped
            .iter()
            .any(|n| n.code == NoticeCode::AliasCollision && n.label.as_deref() == Some("db"))
    );
}

#[tokio::test]
async fn a_tampered_plan_is_checked_again() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = engine(dir.path());
    let mut plan = engine
        .plan_termius_import(fixture("sshconfig"), ImportOptions::default())
        .await
        .expect("plan");
    if let Some(first) = plan.hosts.first_mut() {
        first.hostname = "192.0.2.1\nProxyCommand nc evil 22".into();
    }
    if let Some(second) = plan.hosts.get_mut(1) {
        second.alias = "db *".into();
    }
    plan.include_line = "x\nProxyCommand y".into();
    plan.hosts.push(portico_core::import::PlannedHost {
        alias: "github.com".into(),
        label: "github.com".into(),
        hostname: "203.0.113.66".into(),
        user: None,
        port: None,
        group: None,
        tags: Vec::new(),
        notes: None,
        record: 99,
        favorite: false,
        key: None,
        update: false,
        source_id: "label:github.com".into(),
    });
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert_eq!(report.imported, ["termius-github.com"], "prefixed again");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.code == NoticeCode::AliasLooksLikeHost)
    );
    assert_eq!(report.skipped.len(), 2);
    let text = std::fs::read_to_string(report.target).expect("conf");
    assert!(
        !text.contains("ProxyCommand") && !text.contains("Host db"),
        "{text}"
    );
    assert!(!text.contains("Host github.com\n"), "{text}");
    for line in text.lines().filter(|l| !l.is_empty()) {
        assert!(
            line.starts_with('#')
                || line.starts_with("Host ")
                || ["    HostName ", "    User ", "    Port "]
                    .iter()
                    .any(|p| line.starts_with(p)),
            "unexpected line: {line:?}"
        );
    }
}

#[tokio::test]
async fn a_host_the_user_defines_since_is_never_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let csv = root.join("export.csv");
    std::fs::write(
        &csv,
        "Label,Hostname/IP,Username\nweb,192.0.2.1,deploy\ndb,192.0.2.2,pg\n",
    )
    .expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert_eq!(report.imported, ["web", "db"]);
    let before = std::fs::read_to_string(&report.target).expect("conf");

    // The user then defines `web` in their own config: a re-import never
    // touches it (nor the imported copy), and still updates `db`.
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        "Host web\n    HostName 10.0.0.5\n",
    )
    .expect("config");
    std::fs::write(
        &csv,
        "Label,Hostname/IP,Username\nweb,192.0.2.1,deploy\ndb,192.0.2.20,pg\n",
    )
    .expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan again");
    assert_eq!(aliases(&plan), ["db"]);
    assert_eq!(
        codes(&plan.skipped),
        [(Some("web".into()), NoticeCode::AliasCollision)]
    );
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply again");
    assert_eq!(report.imported, ["db"]);
    assert_eq!(report.updated, ["db"]);
    let text = std::fs::read_to_string(&report.target).expect("conf");
    assert!(
        text.contains("Host db\n    HostName 192.0.2.20\n"),
        "{text}"
    );
    let web_block = |t: &str| {
        t.split("\n\n")
            .find(|b| b.starts_with("Host web"))
            .map(str::to_owned)
    };
    assert_eq!(web_block(&text), web_block(&before), "left as it is");
    assert_eq!(
        std::fs::read_to_string(root.join(".ssh/config")).expect("config"),
        "Host web\n    HostName 10.0.0.5\n"
    );
}

#[tokio::test]
async fn a_short_row_never_moves_a_password_into_another_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let csv = root.join("shifted.csv");
    std::fs::write(
        &csv,
        format!(
            "Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password,SSH_KEY\n\
             G,web,,192.0.2.1,ssh,deploy,{SECRET}\n\
             G,api,,192.0.2.2,ssh,22,svc,{SECRET},,extra\n"
        ),
    )
    .expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    assert!(plan.hosts.is_empty(), "{:?}", aliases(&plan));
    assert_eq!(
        codes(&plan.skipped),
        [
            (None, NoticeCode::ColumnCountMismatch),
            (None, NoticeCode::ColumnCountMismatch)
        ]
    );
    let first = plan.skipped.first().expect("first");
    assert_eq!(first.record, Some(2));
    assert!(
        first.message.contains("7 cells and the header 9"),
        "{first:?}"
    );
    let json = serde_json::to_string(&plan).expect("json");
    let debug = format!("{plan:?}");
    assert!(!json.contains("FAKEPASS"), "{json}");
    assert!(!debug.contains("FAKEPASS"), "{debug}");
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert!(report.imported.is_empty());
    let conf = std::fs::read_to_string(&report.target).unwrap_or_default();
    assert!(!conf.contains("FAKEPASS"), "{conf}");
    assert!(!format!("{report:?}").contains("FAKEPASS"));

    // Without a secret column, missing trailing cells are tolerated.
    let csv = root.join("short.csv");
    std::fs::write(&csv, "Label,Hostname/IP,Username,Port\nweb,192.0.2.1\n").expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(aliases(&plan), ["web"]);
}

#[tokio::test]
async fn an_alias_that_looks_like_another_host_is_prefixed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let csv = root.join("lookalike.csv");
    std::fs::write(
        &csv,
        "Label,Hostname/IP\ngithub.com,203.0.113.66\ndb.example.test,DB.example.test\nweb,192.0.2.1\n",
    )
    .expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(
        aliases(&plan),
        ["termius-github.com", "db.example.test", "web"]
    );
    assert_eq!(
        codes(&plan.warnings),
        [(Some("github.com".into()), NoticeCode::AliasLooksLikeHost)]
    );
    assert_eq!(
        host(&plan, "termius-github.com").notes.as_deref(),
        Some("Termius: github.com")
    );
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    let conf = std::fs::read_to_string(&report.target).expect("conf");
    assert!(
        conf.contains("Host termius-github.com\n    HostName 203.0.113.66\n"),
        "{conf}"
    );
    assert!(!conf.contains("Host github.com"), "{conf}");
}

#[tokio::test]
async fn notices_are_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let csv = root.join("tags.csv");
    let mut text = format!(
        "Label,Hostname/IP,Tags\nmany,192.0.2.1,\"{}\"\n",
        "!,".repeat(100_000)
    );
    // Every row gives up to 64 warnings: far more than the bound.
    for n in 0..40 {
        let _ = writeln!(text, "h{n},192.0.2.2,\"{}\"", "!,".repeat(100));
    }
    std::fs::write(&csv, text).expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.hosts.len(), 41);
    assert!(plan.warnings.len() <= 501, "{}", plan.warnings.len());
    let last = plan.warnings.last().expect("summary");
    assert_eq!(last.code, NoticeCode::NoticesTruncated);
    assert!(
        last.message.ends_with("more warnings not listed"),
        "{last:?}"
    );
    // The report keeps the bound, summary included once.
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert!(report.warnings.len() <= 501);
    assert_eq!(
        report
            .warnings
            .iter()
            .filter(|w| w.code == NoticeCode::NoticesTruncated)
            .count(),
        1
    );

    // Thousands of identical labels are renamed quickly (linear).
    let same = root.join("same.csv");
    let mut text = "Label,Hostname/IP\n".to_owned();
    for _ in 0..4999 {
        text.push_str("web,192.0.2.3\n");
    }
    std::fs::write(&same, text).expect("w");
    let started = std::time::Instant::now();
    let plan = engine
        .plan_termius_import(&same, ImportOptions::default())
        .await
        .expect("plan");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(plan.hosts.len(), 4999);
    assert_eq!(
        plan.hosts.last().map(|h| h.alias.as_str()),
        Some("web-4999")
    );
}

#[tokio::test]
async fn apply_is_all_or_nothing_when_metadata_does_not_fit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    std::fs::create_dir_all(root.join("config")).expect("mkdir");
    // hosts.toml a few bytes under its 1 MiB bound.
    let max = usize::try_from(portico_core::hosts::MAX_HOSTS_FILE_BYTES).expect("usize");
    let filler = format!("# {}\n", "x".repeat(max - 10));
    std::fs::write(root.join("config/hosts.toml"), &filler).expect("w");
    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");
    let target = plan.target.clone();
    let err = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect_err("too large");
    assert_eq!(err.summary().code, "file_too_large");
    assert!(!target.exists(), "hosts.conf must not be written");
    assert_eq!(
        std::fs::read_to_string(root.join("config/hosts.toml")).expect("meta"),
        filler
    );
}

#[tokio::test]
async fn local_export_credentials_csv() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plan = engine(dir.path())
        .plan_termius_import(fixture("credentials.csv"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.format, TermiusFormat::LocalExportCsv);
    assert_eq!(aliases(&plan), ["web-1", "cmd", "ops", "neg", "tab"]);
    let web = host(&plan, "web-1");
    assert_eq!(web.label, "Web 1");
    assert_eq!(web.notes.as_deref(), Some("Termius: Web 1"));
    assert_eq!(web.group.as_deref(), Some("Prod"));
    // The anti-formula apostrophe is removed before = + - @ only.
    let cmd = host(&plan, "cmd");
    assert_eq!(cmd.label, "=SUM(A1)");
    assert_eq!(cmd.port, Some(2200));
    assert_eq!(cmd.group.as_deref(), Some("Prod/EU"));
    let ops = host(&plan, "ops");
    assert_eq!(ops.label, "@ops box");
    assert_eq!(ops.user.as_deref(), Some("+ops"));
    assert_eq!(host(&plan, "neg").label, "-neg label");
    assert_eq!(host(&plan, "tab").label, "'tabbed", "kept before a tab");
    assert_eq!((plan.secrets.passwords, plan.secrets.keys), (1, 1));
    assert!(
        plan.identity_files.is_empty(),
        "key paths are counted, never kept"
    );
    assert!(!format!("{plan:?}").contains(".termius/keys"));
}

#[tokio::test]
async fn local_export_hosts_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let plan = engine(dir.path())
        .plan_termius_import(fixture("hosts.json"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.format, TermiusFormat::LocalExportJson);
    assert_eq!(plan.records, 3);
    assert_eq!(aliases(&plan), ["api-eu", "bastion"]);
    let api = host(&plan, "api-eu");
    assert_eq!((api.hostname.as_str(), api.port), ("203.0.113.30", None));
    assert_eq!(api.user.as_deref(), Some("svc"));
    assert_eq!(api.group.as_deref(), Some("Production/EU"));
    assert_eq!(api.notes.as_deref(), Some("Termius: API EU"));
    let bastion = host(&plan, "bastion");
    assert_eq!(
        (bastion.hostname.as_str(), bastion.port),
        ("2001:db8::20", Some(2022))
    );
    assert_eq!(bastion.notes, None);
    assert_eq!(
        codes(&plan.skipped),
        [(Some("broken".into()), NoticeCode::MissingHostname)]
    );
    assert_eq!((plan.secrets.passwords, plan.secrets.keys), (1, 1));
}

#[tokio::test]
async fn termius_cli_ssh_config_fragment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = engine(dir.path());
    let plan = engine
        .plan_termius_import(fixture("sshconfig"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.format, TermiusFormat::SshConfig);
    assert_eq!(aliases(&plan), ["prod-one", "db"]);
    let prod = host(&plan, "prod-one");
    assert_eq!(prod.label, "prod one");
    assert_eq!(prod.user.as_deref(), Some("admin"));
    assert_eq!(prod.port, None);
    assert_eq!(host(&plan, "db").port, Some(2201));
    assert_eq!(
        plan.identity_files,
        ["~/.termius/ssh_keys/prod one", "~/.termius/ssh_keys/db"]
    );
    assert_eq!(plan.secrets.total(), 0);
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert_eq!(report.identity_files.len(), 2);
    let text = std::fs::read_to_string(report.target).expect("conf");
    assert!(!text.to_lowercase().contains("identityfile"), "{text}");
    assert!(!text.contains(".termius"), "{text}");
}

#[tokio::test]
async fn include_line_with_spaces_is_accepted_by_the_parser() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let mut paths = Paths::rooted(root);
    paths.config_dir = root.join("Library/Application Support/dev.Portico.portico");
    let engine = engine_with(paths);
    let plan = engine
        .plan_termius_import(fixture("sshconfig"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(
        plan.include_line,
        "Include \"~/Library/Application Support/dev.Portico.portico/hosts.conf\""
    );
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        format!("{}\n", report.include_line),
    )
    .expect("w");
    let inventory = engine.load_hosts().await.expect("load");
    let names: Vec<&str> = inventory.hosts.iter().map(|h| h.alias.as_str()).collect();
    assert_eq!(names, ["prod-one", "db"]);
    assert!(inventory.warnings.is_empty(), "{:?}", inventory.warnings);
}

#[tokio::test]
async fn no_secret_reaches_plan_report_files_or_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    for name in ["termius_template.csv", "credentials.csv", "hosts.json"] {
        let plan = engine
            .plan_termius_import(fixture(name), ImportOptions::default())
            .await
            .expect("plan");
        let json = serde_json::to_string(&plan).expect("json");
        let debug = format!("{plan:?}");
        let report = engine
            .apply_import(plan, ImportOptions::default())
            .await
            .expect("apply");
        let report_json = serde_json::to_string(&report).expect("json");
        let report_debug = format!("{report:?}");
        let conf = std::fs::read_to_string(&report.target).expect("conf");
        let meta = std::fs::read_to_string(root.join("config/hosts.toml")).expect("meta");
        for (what, text) in [
            ("plan json", &json),
            ("plan debug", &debug),
            ("report json", &report_json),
            ("report debug", &report_debug),
            ("termius.conf", &conf),
            ("hosts.toml", &meta),
        ] {
            assert!(!text.contains(SECRET), "{name}: secret in {what}");
            assert!(!text.contains("123456"), "{name}: secret in {what}");
            assert!(!text.contains("SSH_KEY\""), "{name}: key cell in {what}");
        }
    }

    // A private key pasted in a non-secret column is counted, never kept.
    let csv = root.join("pasted.csv");
    std::fs::write(
        &csv,
        "Label,Hostname/IP,Username\n\"-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEKEYMATERIAL\n-----END OPENSSH PRIVATE KEY-----\",192.0.2.70,root\n",
    )
    .expect("w");
    let plan = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.secrets.keys, 1);
    assert_eq!(aliases(&plan), ["192.0.2.70"]);
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.code == NoticeCode::SecretInField)
    );
    assert!(!format!("{plan:?}").contains("FAKEKEYMATERIAL"));

    // Errors name the file and line, never a value.
    let broken = root.join("broken.csv");
    std::fs::write(
        &broken,
        format!("Label,Hostname/IP,Password\nweb,192.0.2.1,\"{SECRET}\n"),
    )
    .expect("w");
    let err = engine
        .plan_termius_import(&broken, ImportOptions::default())
        .await
        .expect_err("unterminated");
    let summary = err.summary();
    assert_eq!(summary.code, "import_csv");
    assert!(
        summary
            .message
            .ends_with("broken.csv:2: a quoted cell is never closed"),
        "{summary:?}"
    );
    assert!(!format!("{err:?}{summary:?}").contains(SECRET));
}

#[tokio::test]
async fn unreadable_exports_are_typed_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let plan = |path: PathBuf| {
        let engine = engine.clone();
        async move {
            engine
                .plan_termius_import(path, ImportOptions::default())
                .await
                .expect_err("refused")
                .summary()
        }
    };

    let unknown = root.join("notes.txt");
    std::fs::write(&unknown, "just some notes\nnothing to import\n").expect("w");
    let summary = plan(unknown).await;
    assert_eq!(summary.code, "import_unknown_format");
    assert!(
        summary
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("Hostname/IP")),
        "{summary:?}"
    );

    let empty = root.join("empty.csv");
    std::fs::write(&empty, "").expect("w");
    assert_eq!(plan(empty).await.code, "import_unknown_format");

    let other_json = root.join("other.json");
    std::fs::write(&other_json, "{\"servers\": []}").expect("w");
    assert_eq!(plan(other_json).await.code, "import_unknown_format");

    let big = root.join("big.csv");
    let size = usize::try_from(portico_core::import::MAX_IMPORT_FILE_BYTES).expect("usize") + 1;
    std::fs::write(&big, "x".repeat(size)).expect("w");
    let summary = plan(big).await;
    assert_eq!(summary.code, "import_too_large");
    assert!(summary.hint.is_some());

    assert_eq!(
        plan(root.join("missing.csv")).await.code,
        "import_not_found"
    );
    assert_eq!(plan(root.to_path_buf()).await.code, "not_regular_file");

    let latin1 = root.join("latin1.csv");
    std::fs::write(&latin1, b"Label,Hostname/IP\ncaf\xe9,192.0.2.1\n").expect("w");
    assert_eq!(plan(latin1).await.code, "import_encoding");
}

#[tokio::test]
async fn a_second_import_says_what_it_updates_and_the_include_is_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");
    assert_eq!(plan.replaces, None, "nothing imported yet");
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert!(!report.include_active);

    let again = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan again");
    assert_eq!(again.replaces, Some(6));

    // An export where every record is skipped updates nothing, and never
    // removes the hosts of the earlier import.
    let csv = root.join("nothing.csv");
    std::fs::write(
        &csv,
        "Label,Hostname/IP,Protocol\nswitch,192.0.2.9,telnet\n",
    )
    .expect("w");
    let empty = engine
        .plan_termius_import(&csv, ImportOptions::default())
        .await
        .expect("plan empty");
    assert!(empty.hosts.is_empty());
    assert_eq!(empty.replaces, None);

    // The Include is found from the directive itself.
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        format!(
            "{}\n\nHost mine\n    HostName 10.0.0.1\n",
            report.include_line
        ),
    )
    .expect("config");
    let before = std::fs::read(&report.target).expect("conf");
    let report = engine
        .apply_import(empty, ImportOptions::default())
        .await
        .expect("apply empty");
    assert!(report.imported.is_empty());
    assert!(report.unchanged);
    assert!(report.include_active, "Include present");
    assert_eq!(std::fs::read(&report.target).expect("conf"), before);
    let inventory = engine.load_hosts().await.expect("load");
    assert_eq!(inventory.hosts.len(), 7, "6 imported + mine");
    assert!(inventory.warnings.is_empty(), "{:?}", inventory.warnings);
}

#[tokio::test]
async fn an_old_termius_conf_stays_read_only_and_is_never_shadowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let engine = engine(root);
    let old = portico_core::import::termius_target(&Paths::rooted(root));
    std::fs::create_dir_all(old.parent().expect("parent")).expect("mkdir");
    std::fs::write(&old, "Host web-eu-1\n    HostName 203.0.113.10\n").expect("w");
    std::fs::create_dir_all(root.join(".ssh")).expect("mkdir");
    std::fs::write(
        root.join(".ssh/config"),
        "Include ~/config/imports/termius.conf\n",
    )
    .expect("config");
    let plan = engine
        .plan_termius_import(fixture("termius_template.csv"), ImportOptions::default())
        .await
        .expect("plan");
    assert!(
        codes(&plan.skipped).contains(&(Some("web eu 1".into()), NoticeCode::AliasCollision)),
        "{:?}",
        plan.skipped
    );
    let report = engine
        .apply_import(plan, ImportOptions::default())
        .await
        .expect("apply");
    assert!(!report.imported.contains(&"web-eu-1".to_owned()));
    assert_eq!(
        std::fs::read_to_string(&old).expect("old"),
        "Host web-eu-1\n    HostName 203.0.113.10\n"
    );
    let inventory = engine.load_hosts().await.expect("load");
    let web = inventory
        .hosts
        .iter()
        .find(|h| h.alias == "web-eu-1")
        .expect("web");
    assert_eq!(web.origin, portico_core::hosts::HostOrigin::Imported);
}
