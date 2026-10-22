//! Tests of `managed_hosts`: rendering, validation, hand edits, loading
//! order, collisions, `ProxyJump` cycles and remembered passwords. Every test
//! runs under a temporary root (`Paths::rooted`), never the real `~/.ssh`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use secrecy::{ExposeSecret, SecretString};

use super::*;
use crate::secrets::MemoryStore;

struct Fixture {
    dir: tempfile::TempDir,
    config: EngineConfig,
    secrets: Arc<MemoryStore>,
}

impl Fixture {
    fn new(user_config: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".ssh")).expect("mkdir");
        std::fs::write(dir.path().join(".ssh/config"), user_config).expect("write");
        let secrets = Arc::new(MemoryStore::default());
        let config = EngineConfig {
            paths: Paths::rooted(dir.path()),
            local_user: "alice".into(),
            event_capacity: 64,
            secrets: Arc::clone(&secrets) as Arc<dyn SecretStore>,
            agent_socket: None,
            connect_timeout: crate::engine::DEFAULT_CONNECT_TIMEOUT,
            keepalive: crate::engine::DEFAULT_KEEPALIVE,
        };
        Self {
            dir,
            config,
            secrets,
        }
    }

    fn paths(&self) -> &Paths {
        &self.config.paths
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn target(&self) -> PathBuf {
        self.paths().managed_hosts()
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.target()).expect("hosts.conf")
    }

    /// A private key file under `~/.ssh` (content does not matter here).
    fn key(&self, name: &str) -> PathBuf {
        let path = self.root().join(".ssh").join(name);
        std::fs::write(&path, "not checked").expect("key");
        path
    }

    fn resolve(&self, alias: &str) -> crate::ssh_config::ResolvedHost {
        load_config(self.paths())
            .expect("load")
            .resolve(alias, &self.config.resolve_context(None))
    }

    fn create(&self, draft: &HostDraft) -> Result<HostSummary> {
        create(&self.config, draft)
    }

    fn secret(&self, key: &SecretKey) -> Option<String> {
        self.secrets
            .get(key)
            .expect("get")
            .map(|s| s.expose_secret().to_owned())
    }
}

fn draft(alias: &str, hostname: &str) -> HostDraft {
    HostDraft {
        alias: alias.to_owned(),
        hostname: hostname.to_owned(),
        ..HostDraft::default()
    }
}

fn code(result: Result<impl std::fmt::Debug>) -> String {
    result.expect_err("refused").summary().code
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

const HEADER: &str =
    "# Hosts created in Portico. This file is managed by Portico: do not edit it by hand.
# Portico never edits ~/.ssh/config. To use these hosts with ssh, scp and git too,
# add this line at the top of ~/.ssh/config:
#   Include ~/config/hosts.conf
";

#[test]
fn creates_the_exact_file_private_and_with_metadata() {
    let fx = Fixture::new("");
    let key = fx.key("id_ed25519");
    fx.create(&draft("bastion", "bastion.example.test"))
        .expect("bastion");
    let summary = fx
        .create(&HostDraft {
            alias: "web".into(),
            hostname: " 203.0.113.10 ".into(),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_file: Some(PathBuf::from("~/.ssh/id_ed25519")),
            proxy_jump: Some("bastion".into()),
            tags: vec!["#Prod".into(), "eu".into(), "prod".into()],
            group: Some(" Web ".into()),
            favorite: true,
            notes: Some("front".into()),
            password: PasswordChange::Keep,
        })
        .expect("web");

    assert_eq!(
        fx.file(),
        format!(
            "{HEADER}
Host bastion
    HostName bastion.example.test

Host web
    HostName 203.0.113.10
    User deploy
    Port 2222
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes
    ProxyJump bastion
"
        )
    );
    assert_eq!(summary.host.identity_file.as_deref(), Some(key.as_path()));
    assert_eq!(summary.meta.tags, ["Prod", "eu"]);
    assert_eq!(summary.meta.group.as_deref(), Some("Web"));
    assert!(summary.meta.favorite);
    assert_eq!(summary.file, fx.target());

    let (meta, _) = crate::hosts::load_meta(&fx.paths().hosts_meta()).expect("meta");
    assert_eq!(meta.hosts.get("web"), Some(&summary.meta));
    assert!(!meta.hosts.contains_key("bastion"), "no empty entry");

    // What Portico wrote, it reads back identically.
    let hosts = read_hosts(fx.paths()).expect("read");
    assert_eq!(render(fx.paths(), &hosts), fx.file());
    assert_eq!(hosts.get(1), Some(&summary.host));

    // And the engine resolves it.
    let resolved = fx.resolve("web");
    assert_eq!(resolved.hostname, "203.0.113.10");
    assert_eq!(resolved.user.as_deref(), Some("deploy"));
    assert_eq!(resolved.port, 2222);
    assert_eq!(resolved.identity_files, [key]);
    assert!(resolved.identities_only);
    assert_eq!(
        resolved.proxy_jump.first().map(|j| j.host.as_str()),
        Some("bastion")
    );

    #[cfg(unix)]
    {
        assert_eq!(mode(&fx.target()), 0o600);
        assert_eq!(mode(&fx.paths().config_dir), 0o700);
        assert_eq!(mode(&store::lock_path(&fx.target())), 0o600);
    }
}

#[test]
fn identity_paths_with_a_space_are_quoted() {
    let fx = Fixture::new("");
    let dir = fx.root().join("my keys");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let key = dir.join("id work");
    std::fs::write(&key, "k").expect("key");
    fx.create(&HostDraft {
        identity_file: Some(key.clone()),
        ..draft("work", "work.example.test")
    })
    .expect("create");
    assert!(
        fx.file()
            .contains("    IdentityFile \"~/my keys/id work\"\n    IdentitiesOnly yes\n"),
        "{}",
        fx.file()
    );
    assert_eq!(fx.resolve("work").identity_files, [key]);
    let hosts = read_hosts(fx.paths()).expect("read");
    assert_eq!(render(fx.paths(), &hosts), fx.file());
}

#[test]
fn updates_rename_and_move_metadata_and_jump_references() {
    let fx = Fixture::new("");
    fx.create(&HostDraft {
        tags: vec!["a".into()],
        ..draft("bastion", "198.51.100.1")
    })
    .expect("bastion");
    fx.create(&HostDraft {
        proxy_jump: Some("bastion".into()),
        ..draft("app", "10.0.0.2")
    })
    .expect("app");
    // An unknown key of the user's survives the rename.
    let meta_path = fx.paths().hosts_meta();
    let text = std::fs::read_to_string(&meta_path).expect("meta");
    std::fs::write(&meta_path, format!("{text}color = \"red\"\n")).expect("write");

    let summary = update(
        &fx.config,
        "bastion",
        &HostDraft {
            user: Some("ops".into()),
            port: Some(22),
            tags: vec!["a".into(), "b".into()],
            ..draft("jump", "198.51.100.1")
        },
    )
    .expect("rename");
    assert_eq!(summary.host.alias, "jump");
    assert_eq!(
        fx.file(),
        format!(
            "{HEADER}
Host jump
    HostName 198.51.100.1
    User ops
    Port 22

Host app
    HostName 10.0.0.2
    ProxyJump jump
"
        )
    );
    let meta_text = std::fs::read_to_string(&meta_path).expect("meta");
    assert!(meta_text.contains("[hosts.jump]"), "{meta_text}");
    assert!(meta_text.contains("color = \"red\""), "{meta_text}");
    assert!(!meta_text.contains("bastion"), "{meta_text}");
    assert_eq!(summary.meta.tags, ["a", "b"]);

    // The old alias is gone.
    assert_eq!(
        code(update(&fx.config, "bastion", &draft("x", "h"))),
        "host_not_found"
    );
    // An unchanged alias, other values.
    let summary = update(&fx.config, "app", &draft("app", "10.0.0.3")).expect("edit");
    assert_eq!(summary.host.proxy_jump, None);
    assert_eq!(fx.resolve("app").hostname, "10.0.0.3");
}

#[test]
fn deletes_host_metadata_and_passwords() {
    let fx = Fixture::new("");
    fx.create(&HostDraft {
        user: Some("root".into()),
        port: Some(2200),
        favorite: true,
        password: PasswordChange::Set(SecretString::from("s3cret".to_owned())),
        ..draft("db", "DB.example.test")
    })
    .expect("create");
    let unbound = SecretKey::UnboundHostPassword {
        user: "root".into(),
        host: "db.example.test".into(),
        port: 2200,
    };
    assert_eq!(fx.secret(&unbound).as_deref(), Some("s3cret"));
    // As if a first login had bound it to the server key.
    let host_key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .expect("key");
    let public = host_key.public_key().clone();
    std::fs::write(
        fx.paths().known_hosts(),
        format!(
            "[db.example.test]:2200 {}\n",
            public.to_openssh().expect("openssh")
        ),
    )
    .expect("known_hosts");
    let bound = SecretKey::HostPassword {
        user: "root".into(),
        host: "db.example.test".into(),
        port: 2200,
        host_key: public.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
    };
    fx.secrets
        .set(&bound, &SecretString::from("s3cret".to_owned()))
        .expect("set");

    assert_eq!(delete(&fx.config, "DB").expect("delete"), "db");
    assert_eq!(fx.file(), HEADER);
    assert_eq!(fx.secret(&unbound), None);
    assert_eq!(fx.secret(&bound), None);
    let (meta, _) = crate::hosts::load_meta(&fx.paths().hosts_meta()).expect("meta");
    assert!(meta.hosts.is_empty(), "{meta:?}");
    assert_eq!(code(delete(&fx.config, "db")), "host_not_found");
}

#[test]
fn a_password_shared_with_another_host_is_kept_on_delete() {
    let fx = Fixture::new("Host legacy\n    HostName db.example.test\n    User root\n");
    fx.create(&HostDraft {
        user: Some("root".into()),
        password: PasswordChange::Set(SecretString::from("pw".to_owned())),
        ..draft("db", "db.example.test")
    })
    .expect("create");
    delete(&fx.config, "db").expect("delete");
    let unbound = SecretKey::UnboundHostPassword {
        user: "root".into(),
        host: "db.example.test".into(),
        port: 22,
    };
    assert_eq!(
        fx.secret(&unbound).as_deref(),
        Some("pw"),
        "legacy still uses it"
    );
}

#[test]
fn changing_the_target_never_carries_the_password_along() {
    let fx = Fixture::new("");
    fx.create(&HostDraft {
        password: PasswordChange::Set(SecretString::from("pw".to_owned())),
        ..draft("db", "old.example.test")
    })
    .expect("create");
    let old = SecretKey::UnboundHostPassword {
        user: "alice".into(),
        host: "old.example.test".into(),
        port: 22,
    };
    let new = SecretKey::UnboundHostPassword {
        user: "alice".into(),
        host: "new.example.test".into(),
        port: 22,
    };
    // Renaming alone keeps it (same account).
    update(&fx.config, "db", &draft("db2", "old.example.test")).expect("rename");
    assert_eq!(fx.secret(&old).as_deref(), Some("pw"));
    // Another server: forgotten, not moved.
    update(&fx.config, "db2", &draft("db2", "new.example.test")).expect("move");
    assert_eq!(fx.secret(&old), None);
    assert_eq!(fx.secret(&new), None);
    // Set, then forget.
    update(
        &fx.config,
        "db2",
        &HostDraft {
            password: PasswordChange::Set(SecretString::from("pw2".to_owned())),
            ..draft("db2", "new.example.test")
        },
    )
    .expect("set");
    assert_eq!(fx.secret(&new).as_deref(), Some("pw2"));
    update(
        &fx.config,
        "db2",
        &HostDraft {
            password: PasswordChange::Forget,
            ..draft("db2", "new.example.test")
        },
    )
    .expect("forget");
    assert_eq!(fx.secret(&new), None);
}

#[test]
fn duplicates_any_host_and_user_hosts_stay_read_only() {
    let fx = Fixture::new("");
    let key = fx.key("id_prod");
    std::fs::write(
        fx.paths().ssh_config(),
        "Host bastion\n    HostName 198.51.100.1\n\n\
         Host prod\n    HostName 10.0.0.5\n    User root\n    Port 2200\n    \
         IdentityFile ~/.ssh/missing\n    IdentityFile ~/.ssh/id_prod\n    ProxyJump bastion\n\n\
         Host chain\n    HostName 10.0.0.6\n    ProxyJump bastion,prod\n\n\
         Host cmd\n    HostName 10.0.0.7\n    ProxyCommand nc %h %p\n",
    )
    .expect("config");
    crate::hosts::update_meta(
        &fx.paths().hosts_meta(),
        "prod",
        MetaChange::Tags(vec!["prod".into()]),
    )
    .expect("meta");

    let copy = duplicate(&fx.config, "prod", "prod-copy").expect("duplicate");
    assert_eq!(
        copy.host,
        ManagedHost {
            alias: "prod-copy".into(),
            hostname: "10.0.0.5".into(),
            user: Some("root".into()),
            port: Some(2200),
            identity_file: Some(key),
            proxy_jump: Some("bastion".into()),
        }
    );
    assert_eq!(copy.meta.tags, ["prod"]);
    // The copy resolves like the original.
    let (a, b) = (fx.resolve("prod"), fx.resolve("prod-copy"));
    assert_eq!(
        (a.hostname, a.user, a.port, a.proxy_jump),
        (b.hostname, b.user, b.port, b.proxy_jump)
    );
    // Without Port/User in the source, none are written.
    let plain = duplicate(&fx.config, "bastion", "bastion2").expect("dup");
    assert_eq!((plain.host.user, plain.host.port), (None, None));

    assert_eq!(
        code(duplicate(&fx.config, "chain", "c2")),
        "host_not_duplicable"
    );
    assert_eq!(
        code(duplicate(&fx.config, "cmd", "c3")),
        "host_not_duplicable"
    );
    assert_eq!(code(duplicate(&fx.config, "nope", "c4")), "host_not_found");
    assert_eq!(
        code(duplicate(&fx.config, "prod", "prod-copy")),
        "host_alias_taken"
    );
    assert_eq!(
        code(duplicate(&fx.config, "prod", "PROD")),
        "host_invalid_alias"
    );

    // Hosts of ~/.ssh/config cannot be edited or deleted.
    let err = update(&fx.config, "prod", &draft("prod", "10.0.0.9")).expect_err("read-only");
    let summary = err.summary();
    assert_eq!(summary.code, "host_read_only");
    assert!(
        summary
            .hint
            .as_deref()
            .is_some_and(|h| h.contains("duplicate it as a Portico host")),
        "{summary:?}"
    );
    assert_eq!(code(delete(&fx.config, "prod")), "host_read_only");
    assert!(
        std::fs::read_to_string(fx.paths().ssh_config())
            .expect("config")
            .contains("Port 2200"),
        "never written"
    );
}

#[test]
fn implicit_include_is_read_first_like_openssh() {
    // A user `Host *` at the top: with the Include line at the very top,
    // OpenSSH reads the Portico block first, so its `User` wins; a Portico
    // host without `User` gets the user's.
    let fx = Fixture::new("Host *\n    User x\n    Port 2200\n");
    fx.create(&HostDraft {
        user: Some("deploy".into()),
        ..draft("web", "203.0.113.10")
    })
    .expect("web");
    fx.create(&draft("plain", "203.0.113.11")).expect("plain");
    let web = fx.resolve("web");
    assert_eq!((web.user.as_deref(), web.port), (Some("deploy"), 2200));
    let plain = fx.resolve("plain");
    assert_eq!(plain.user.as_deref(), Some("x"));

    let config = load_config(fx.paths()).expect("load");
    let web_count = config
        .all_concrete_aliases()
        .iter()
        .filter(|a| a.name == "web")
        .count();
    assert_eq!(web_count, 1);
    assert!(!crate::import::include_active(fx.paths(), &fx.target()));
}

#[test]
fn explicit_include_is_not_loaded_twice_and_keeps_its_place() {
    // The user put the Include line at the bottom: OpenSSH (and Portico)
    // then read their `Host *` first.
    let fx = Fixture::new("Host *\n    User x\n\nInclude ~/config/hosts.conf\n");
    fx.create(&HostDraft {
        user: Some("deploy".into()),
        ..draft("web", "203.0.113.10")
    })
    .expect("web");
    let config = load_config(fx.paths()).expect("load");
    let occurrences: Vec<_> = config
        .all_concrete_aliases()
        .into_iter()
        .filter(|a| a.name == "web")
        .collect();
    assert_eq!(occurrences.len(), 1, "{occurrences:?}");
    assert_eq!(fx.resolve("web").user.as_deref(), Some("x"));
    assert!(crate::import::include_active(fx.paths(), &fx.target()));
    // Edits are checked against that same order.
    fx.create(&HostDraft {
        proxy_jump: Some("web".into()),
        ..draft("app", "10.0.0.2")
    })
    .expect("app");
    assert_eq!(
        code(update(
            &fx.config,
            "web",
            &HostDraft {
                proxy_jump: Some("app".into()),
                ..draft("web", "203.0.113.10")
            }
        )),
        "host_proxy_jump_cycle"
    );
}

#[test]
fn collisions_with_any_source_are_refused_ignoring_case() {
    let fx = Fixture::new("Host Prod web-*\n    HostName 10.0.0.1\n");
    let termius = crate::import::termius_target(fx.paths());
    std::fs::create_dir_all(termius.parent().expect("parent")).expect("mkdir");
    std::fs::write(&termius, "Host db\n    HostName 10.0.0.2\n").expect("termius");

    let err = fx.create(&draft("prod", "h")).expect_err("taken");
    assert!(
        matches!(
            &err,
            Error::ManagedHost(ManagedHostError::AliasTaken { file, line: 1, .. })
                if *file == fx.paths().ssh_config()
        ),
        "{err:?}"
    );
    assert_eq!(code(fx.create(&draft("db", "h"))), "host_alias_taken");
    // A wildcard pattern is not a host.
    fx.create(&draft("web-1", "h")).expect("web-1");
    assert_eq!(code(fx.create(&draft("web-1", "h2"))), "host_alias_taken");
    fx.create(&draft("api", "h")).expect("api");
    assert_eq!(
        code(update(&fx.config, "api", &draft("web-1", "h"))),
        "host_alias_taken"
    );
    assert_eq!(
        read_hosts(fx.paths()).expect("read").len(),
        2,
        "nothing else written"
    );
}

#[test]
// One table of hostile values, kept in one piece.
#[allow(clippy::too_many_lines)]
fn injection_attempts_are_refused() {
    let fx = Fixture::new("Host bastion\n    HostName 198.51.100.1\n");
    let key = fx.key("id_ok");
    let cases: Vec<(HostDraft, &str)> = vec![
        (draft("a\nProxyCommand x", "h"), "host_invalid_alias"),
        (draft("a b", "h"), "host_invalid_alias"),
        (draft("a=b", "h"), "host_invalid_alias"),
        (draft("a#b", "h"), "host_invalid_alias"),
        (draft("\"a\"", "h"), "host_invalid_alias"),
        (draft("%h", "h"), "host_invalid_alias"),
        (draft("-oProxyCommand=x", "h"), "host_invalid_alias"),
        (draft("a*", "h"), "host_invalid_alias"),
        (draft("none", "h"), "host_invalid_alias"),
        (draft("", "h"), "host_invalid_alias"),
        (
            draft("ok", "h\nProxyCommand nc evil 22"),
            "host_invalid_hostname",
        ),
        (draft("ok", "h p"), "host_invalid_hostname"),
        (draft("ok", "h=p"), "host_invalid_hostname"),
        (draft("ok", "h#p"), "host_invalid_hostname"),
        (draft("ok", "\"h\""), "host_invalid_hostname"),
        (draft("ok", "%h.example"), "host_invalid_hostname"),
        (draft("ok", "-oProxyCommand=x"), "host_invalid_hostname"),
        (draft("ok", ""), "host_invalid_hostname"),
        (
            HostDraft {
                user: Some("root\nProxyCommand x".into()),
                ..draft("ok", "h")
            },
            "host_invalid_user",
        ),
        (
            HostDraft {
                user: Some("-l".into()),
                ..draft("ok", "h")
            },
            "host_invalid_user",
        ),
        (
            HostDraft {
                user: Some("%u".into()),
                ..draft("ok", "h")
            },
            "host_invalid_user",
        ),
        (
            HostDraft {
                user: Some("a=b".into()),
                ..draft("ok", "h")
            },
            "host_invalid_user",
        ),
        (
            HostDraft {
                proxy_jump: Some("bastion\nProxyCommand x".into()),
                ..draft("ok", "h")
            },
            "host_invalid_proxy_jump",
        ),
        (
            HostDraft {
                proxy_jump: Some("user@bastion:22".into()),
                ..draft("ok", "h")
            },
            "host_invalid_proxy_jump",
        ),
        (
            HostDraft {
                proxy_jump: Some("-oProxyCommand=x".into()),
                ..draft("ok", "h")
            },
            "host_invalid_proxy_jump",
        ),
        (
            HostDraft {
                proxy_jump: Some("bastion,other".into()),
                ..draft("ok", "h")
            },
            "host_invalid_proxy_jump",
        ),
        (
            HostDraft {
                proxy_jump: Some("unknown".into()),
                ..draft("ok", "h")
            },
            "host_invalid_proxy_jump",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from(format!("{}\nProxyCommand x", key.display()))),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from("~/.ssh/id_%h")),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from("~/.ssh/id\"x")),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from("~/.ssh/id=x")),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from("~/.ssh/id#x")),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                identity_file: Some(PathBuf::from("~/.ssh/${HOME}")),
                ..draft("ok", "h")
            },
            "host_invalid_identity_file",
        ),
        (
            HostDraft {
                tags: vec!["bad tag".into()],
                ..draft("ok", "h")
            },
            "host_tag",
        ),
    ];
    for (bad, expected) in cases {
        let shown = format!("{bad:?}");
        let err = fx.create(&bad).expect_err(&shown);
        let summary = err.summary();
        assert_eq!(summary.code, expected, "{shown}");
        assert!(summary.hint.is_some(), "{shown}");
        assert!(!summary.message.contains('\n'), "value echoed: {summary:?}");
        assert!(
            !summary.message.contains("ProxyCommand"),
            "value echoed: {summary:?}"
        );
    }
    assert!(!fx.target().exists(), "nothing written");
    assert!(!fx.paths().hosts_meta().exists(), "nothing written");
}

#[test]
fn ports_are_1_to_65535() {
    let fx = Fixture::new("");
    for (port, ok) in [
        (0, false),
        (1, true),
        (65535, true),
        (65536, false),
        (u32::MAX, false),
    ] {
        let result = fx.create(&HostDraft {
            port: Some(port),
            ..draft(&format!("p{port}"), "h")
        });
        if ok {
            result.expect("valid port");
        } else {
            let err = result.expect_err("invalid port");
            assert_eq!(err.summary().code, "host_invalid_port");
        }
    }
}

#[cfg(unix)]
#[test]
fn identity_files_must_be_regular_files_inside_home() {
    let fx = Fixture::new("");
    let outside = tempfile::tempdir().expect("tempdir");
    let foreign = outside.path().join("id_foreign");
    std::fs::write(&foreign, "k").expect("write");
    let link = fx.root().join(".ssh/id_link");
    std::os::unix::fs::symlink(&foreign, &link).expect("symlink");
    let inside = fx.key("id_inside");
    let inside_link = fx.root().join(".ssh/id_inside_link");
    std::os::unix::fs::symlink(&inside, &inside_link).expect("symlink");
    std::fs::write(fx.root().join(".ssh/id_inside.pub"), "pub").expect("pub");

    for bad in [
        foreign.clone(),
        link,
        fx.root().join(".ssh/missing"),
        fx.root().join(".ssh"),
        fx.root().join(".ssh/../.ssh/id_inside"),
        fx.root().join(".ssh/id_inside.pub"),
        PathBuf::from(".ssh/id_inside"),
        PathBuf::from("~/../etc/passwd"),
    ] {
        let err = fx
            .create(&HostDraft {
                identity_file: Some(bad.clone()),
                ..draft("k", "h")
            })
            .expect_err(&bad.display().to_string());
        assert_eq!(err.summary().code, "host_invalid_identity_file", "{bad:?}");
    }
    // A link that stays inside home is fine (the link path is written).
    let summary = fx
        .create(&HostDraft {
            identity_file: Some(inside_link.clone()),
            ..draft("k", "h")
        })
        .expect("inside link");
    assert_eq!(summary.host.identity_file, Some(inside_link));
}

#[test]
fn proxy_jump_cycles_are_refused() {
    let fx = Fixture::new("Host *.corp\n    ProxyJump gw\n");
    fx.create(&draft("a", "10.0.0.1")).expect("a");
    fx.create(&HostDraft {
        proxy_jump: Some("A".into()),
        ..draft("b", "10.0.0.2")
    })
    .expect("b");
    assert_eq!(
        read_hosts(fx.paths())
            .expect("read")
            .get(1)
            .and_then(|h| h.proxy_jump.clone()),
        Some("a".into()),
        "written as declared"
    );
    let before = fx.file();
    let err = update(
        &fx.config,
        "a",
        &HostDraft {
            proxy_jump: Some("b".into()),
            ..draft("a", "10.0.0.1")
        },
    )
    .expect_err("cycle");
    assert_eq!(err.summary().code, "host_proxy_jump_cycle");
    assert!(err.to_string().contains("a → b → a"), "{err}");
    assert_eq!(fx.file(), before, "untouched");
    // Self jump.
    assert_eq!(
        code(fx.create(&HostDraft {
            proxy_jump: Some("c".into()),
            ..draft("c", "10.0.0.3")
        })),
        "host_proxy_jump_cycle"
    );
    // Through a user pattern: gw.corp → gw (via `Host *.corp`), gw → gw.corp.
    fx.create(&draft("gw.corp", "10.0.0.4")).expect("gw.corp");
    assert_eq!(
        code(fx.create(&HostDraft {
            proxy_jump: Some("gw.corp".into()),
            ..draft("gw", "10.0.0.5")
        })),
        "host_proxy_jump_cycle"
    );
}

#[test]
fn jump_hosts_in_use_cannot_be_deleted_or_renamed_away() {
    let fx = Fixture::new("");
    fx.create(&draft("bastion", "10.0.0.1")).expect("bastion");
    fx.create(&HostDraft {
        proxy_jump: Some("bastion".into()),
        ..draft("app", "10.0.0.2")
    })
    .expect("app");
    let err = delete(&fx.config, "bastion").expect_err("in use");
    assert_eq!(err.summary().code, "host_in_use");
    assert!(err.to_string().contains("app"), "{err}");
    // A host of the user's config jumping through it blocks a rename.
    std::fs::write(
        fx.paths().ssh_config(),
        "Host legacy\n    HostName 10.0.0.3\n    ProxyJump bastion\n",
    )
    .expect("config");
    assert_eq!(
        code(update(&fx.config, "bastion", &draft("gw", "10.0.0.1"))),
        "host_in_use"
    );
    delete(&fx.config, "app").expect("app goes");
}

#[test]
fn hand_edits_are_read_but_never_rewritten_silently() {
    let fx = Fixture::new("");
    fx.create(&draft("web", "10.0.0.1")).expect("web");
    let original = fx.file();

    // Normalized formatting is fine: rewritten without losing anything.
    std::fs::write(
        fx.target(),
        original.replace("    HostName 10.0.0.1", "  hostname=10.0.0.1"),
    )
    .expect("edit");
    fx.create(&draft("api", "10.0.0.2"))
        .expect("formatting only");

    for (edit, expected) in [
        ("    ProxyCommand nc evil 22\n", "host_file_unmanaged"),
        ("    ForwardAgent yes\n", "host_file_unmanaged"),
        ("# my note\n", "host_file_unmanaged"),
        ("    User root # admin\n", "host_file_unmanaged"),
        ("    HostName 10.0.0.9\n", "host_file_unmanaged"),
        ("Include ~/other.conf\n", "host_file_unmanaged"),
        ("Match all\n", "host_file_unmanaged"),
        ("Host a b\n    HostName h\n", "host_file_unmanaged"),
        ("Host web\n    HostName h\n", "host_file_unmanaged"),
        ("    IdentitiesOnly yes\n", "host_file_unmanaged"),
        ("    User \"unterminated\n", "host_file_invalid"),
    ] {
        let edited = format!("{original}{edit}");
        std::fs::write(fx.target(), &edited).expect("edit");
        let err = fx.create(&draft("new", "10.0.0.3")).expect_err(edit);
        let summary = err.summary();
        assert_eq!(summary.code, expected, "{edit:?}");
        assert!(summary.hint.is_some());
        assert!(summary.message.contains("hosts.conf:"), "{summary:?}");
        assert_eq!(
            code(update(&fx.config, "web", &draft("web", "10.0.0.4"))),
            expected
        );
        assert_eq!(code(delete(&fx.config, "web")), expected);
        assert_eq!(fx.file(), edited, "left untouched");
    }

    // Still read: a hand-added directive applies, like with OpenSSH.
    std::fs::write(
        fx.target(),
        format!("{original}    Port 2022\n    Compression yes\n"),
    )
    .expect("edit");
    let resolved = fx.resolve("web");
    assert_eq!(resolved.port, 2022);
    // An unparsable file is skipped with a warning; the user's hosts load.
    std::fs::write(
        fx.paths().ssh_config(),
        "Host mine\n    HostName 10.9.9.9\n",
    )
    .expect("cfg");
    std::fs::write(fx.target(), "Host web\n    HostName \"oops\n").expect("edit");
    let config = load_config(fx.paths()).expect("load");
    assert!(
        config
            .all_concrete_aliases()
            .iter()
            .any(|a| a.name == "mine")
    );
    assert!(
        config
            .warnings()
            .iter()
            .any(|w| w.source.file == fx.target() && w.source.line == 2),
        "{:?}",
        config.warnings()
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_file_is_never_replaced() {
    let fx = Fixture::new("");
    let real = fx.root().join("dotfiles-hosts.conf");
    std::fs::write(
        &real,
        format!("{HEADER}\nHost web\n    HostName 10.0.0.1\n"),
    )
    .expect("w");
    std::fs::create_dir_all(&fx.paths().config_dir).expect("mkdir");
    std::os::unix::fs::symlink(&real, fx.target()).expect("symlink");

    // Read through the link (sessions and inventory).
    assert_eq!(fx.resolve("web").hostname, "10.0.0.1");
    // Never written through it.
    assert_eq!(
        code(fx.create(&draft("api", "10.0.0.2"))),
        "not_regular_file"
    );
    assert!(
        std::fs::symlink_metadata(fx.target())
            .expect("meta")
            .file_type()
            .is_symlink()
    );
    assert!(
        !std::fs::read_to_string(&real)
            .expect("read")
            .contains("api")
    );
}

#[test]
fn a_failed_write_forgets_the_password_it_stored() {
    let fx = Fixture::new("");
    // The target is a directory: the write fails after the secret was set.
    std::fs::create_dir_all(fx.target()).expect("mkdir");
    let err = fx
        .create(&HostDraft {
            password: PasswordChange::Set(SecretString::from("pw".to_owned())),
            ..draft("web", "10.0.0.1")
        })
        .expect_err("not a file");
    assert_eq!(err.summary().code, "not_regular_file");
    assert_eq!(
        fx.secret(&SecretKey::UnboundHostPassword {
            user: "alice".into(),
            host: "10.0.0.1".into(),
            port: 22,
        }),
        None
    );
}

#[test]
fn origins_are_marked() {
    let fx =
        Fixture::new("Host mine\n    HostName 10.0.0.1\n\nInclude ~/config/imports/termius.conf\n");
    let termius = crate::import::termius_target(fx.paths());
    std::fs::create_dir_all(termius.parent().expect("parent")).expect("mkdir");
    std::fs::write(&termius, "Host imported\n    HostName 10.0.0.2\n").expect("termius");
    fx.create(&draft("created", "10.0.0.3")).expect("create");

    let config = load_config(fx.paths()).expect("load");
    let (meta, _) = crate::hosts::load_meta(&fx.paths().hosts_meta()).expect("meta");
    let mut hosts = crate::hosts::inventory(&config, &meta, &fx.config.resolve_context(None));
    mark_origins(fx.paths(), &mut hosts);
    let origins: Vec<(&str, HostOrigin)> =
        hosts.iter().map(|h| (h.alias.as_str(), h.origin)).collect();
    assert_eq!(
        origins,
        [
            ("created", HostOrigin::Portico),
            ("mine", HostOrigin::SshConfig),
            ("imported", HostOrigin::Imported),
        ]
    );
    assert_eq!(include_line_for(fx.paths()), "Include ~/config/hosts.conf");
}
