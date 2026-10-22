//! Hosts created in Portico (phase 6b), end to end through the [`Engine`]:
//! events, inventory origins, a remembered password used by a real
//! (in-process) SSH login, and OpenSSH's own reading of `hosts.conf`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::process::Command;
use std::sync::atomic::Ordering;

use portico_core::EngineEvent;
use portico_core::engine::PromptResponse;
use portico_core::events::PromptKind;
use portico_core::hosts::HostOrigin;
use portico_core::managed_hosts::{HostDraft, PasswordChange};
use portico_core::secrets::{SecretKey, SecretStore};
use secrecy::{ExposeSecret, SecretString};
use support::{Behaviour, Env, TestServer, accept_host, never, new_key};

fn draft(alias: &str, hostname: &str) -> HostDraft {
    HostDraft {
        alias: alias.to_owned(),
        hostname: hostname.to_owned(),
        ..HostDraft::default()
    }
}

fn with_password(alias: &str, port: u16, password: &str) -> HostDraft {
    HostDraft {
        user: Some("alice".into()),
        port: Some(u32::from(port)),
        password: PasswordChange::Set(SecretString::from(password.to_owned())),
        ..draft(alias, "127.0.0.1")
    }
}

fn unbound(port: u16) -> SecretKey {
    SecretKey::UnboundHostPassword {
        user: "alice".into(),
        host: "127.0.0.1".into(),
        port,
    }
}

fn bound(server: &TestServer) -> SecretKey {
    SecretKey::HostPassword {
        user: "alice".into(),
        host: "127.0.0.1".into(),
        port: server.port,
        host_key: server
            .host_key
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string(),
    }
}

fn stored(env: &Env, key: &SecretKey) -> Option<String> {
    env.secrets
        .get(key)
        .unwrap()
        .map(|s| s.expose_secret().to_owned())
}

#[tokio::test]
async fn edits_emit_events_and_hosts_carry_their_origin() {
    let mut env = Env::new("Host mine\n    HostName 10.0.0.1\n");
    let summary = env
        .engine
        .create_host(HostDraft {
            favorite: true,
            ..draft("web", "203.0.113.10")
        })
        .await
        .unwrap();
    assert_eq!(summary.host.alias, "web");
    assert!(matches!(
        env.events.recv().await.unwrap(),
        EngineEvent::HostsChanged
    ));
    match env.events.recv().await.unwrap() {
        EngineEvent::HostMeta { alias, meta } => {
            assert_eq!(alias, "web");
            assert!(meta.favorite);
        }
        other => panic!("unexpected {other:?}"),
    }

    let inventory = env.engine.load_hosts().await.unwrap();
    let origins: Vec<(&str, HostOrigin)> = inventory
        .hosts
        .iter()
        .map(|h| (h.alias.as_str(), h.origin))
        .collect();
    assert_eq!(
        origins,
        [
            ("web", HostOrigin::Portico),
            ("mine", HostOrigin::SshConfig)
        ]
    );
    let web = inventory.hosts.first().unwrap();
    assert!(web.meta.favorite);
    assert_eq!(web.source.file, env.engine.config().paths.managed_hosts());

    assert_eq!(
        env.engine.managed_include_line(),
        "Include ~/config/hosts.conf"
    );
    assert!(!env.engine.managed_include_active().await.unwrap());

    let err = env.engine.delete_host("mine").await.unwrap_err();
    assert_eq!(err.summary().code, "host_read_only");
    env.engine
        .duplicate_as_managed("mine", "mine-copy")
        .await
        .unwrap();
    env.engine
        .update_host("mine-copy", draft("mine2", "10.0.0.1"))
        .await
        .unwrap();
    env.engine.delete_host("web").await.unwrap();
    let aliases: Vec<String> = env
        .engine
        .load_hosts()
        .await
        .unwrap()
        .hosts
        .into_iter()
        .map(|h| h.alias)
        .collect();
    assert_eq!(aliases, ["mine2", "mine"]);
}

#[tokio::test]
async fn a_remembered_password_logs_in_then_binds_to_the_host_key() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "s3cret-pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new("");
    env.trust(&server);
    env.engine
        .create_host(with_password("db", server.port, "s3cret-pw"))
        .await
        .unwrap();
    assert_eq!(
        stored(&env, &unbound(server.port)).as_deref(),
        Some("s3cret-pw")
    );

    // No prompt at all: the stored password is used.
    let (id, out) = env.connect("db", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(server.behaviour.logins.load(Ordering::SeqCst), 1);
    // Bound to the verified key, the unbound copy is gone.
    assert_eq!(stored(&env, &bound(&server)).as_deref(), Some("s3cret-pw"));
    assert_eq!(stored(&env, &unbound(server.port)), None);
    let logs = serde_json::to_string(&env.engine.session_logs(id).unwrap()).unwrap();
    for haystack in [out.transcript(), logs] {
        assert!(!haystack.contains("s3cret-pw"), "secret leaked");
    }

    // Next time, the bound password.
    let (_, again) = env.connect("db", never).await;
    assert!(again.connected(), "{:?}", again.phases);

    // Deleting the host forgets it.
    env.engine.delete_host("db").await.unwrap();
    assert_eq!(stored(&env, &bound(&server)), None);
}

#[tokio::test]
async fn keyboard_interactive_uses_and_binds_the_password_too() {
    let server = TestServer::start(Behaviour {
        keyboard_interactive: Some(("alice".into(), "kbd-pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new("");
    env.trust(&server);
    env.engine
        .create_host(with_password("kbd", server.port, "kbd-pw"))
        .await
        .unwrap();
    let (_, out) = env.connect("kbd", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(stored(&env, &bound(&server)).as_deref(), Some("kbd-pw"));
    assert_eq!(stored(&env, &unbound(server.port)), None);
}

#[tokio::test]
async fn the_password_waits_for_host_key_verification() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "pw-1".into())),
        ..Behaviour::default()
    })
    .await;

    // Unknown key: the user is asked first, then the password is used.
    let mut env = Env::new("");
    env.engine
        .create_host(with_password("new", server.port, "pw-1"))
        .await
        .unwrap();
    let (_, out) = env.connect("new", accept_host).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(
        matches!(
            out.prompts.as_slice(),
            [PromptKind::UnknownHostKey {
                sends_remembered_password: true,
                ..
            }]
        ),
        "the prompt says a remembered password follows: {:?}",
        out.prompts
    );

    // Changed key: refused before any authentication.
    let mut env = Env::new("");
    let other = new_key();
    env.append_known_hosts(&format!(
        "[127.0.0.1]:{} {}",
        server.port,
        other.public_key().to_openssh().unwrap()
    ));
    env.engine
        .create_host(with_password("moved", server.port, "pw-1"))
        .await
        .unwrap();
    let before = server.behaviour.logins.load(Ordering::SeqCst);
    let (_, out) = env.connect("moved", never).await;
    assert_eq!(out.failure_code(), Some("host_key_changed"));
    assert_eq!(server.behaviour.logins.load(Ordering::SeqCst), before);
    assert_eq!(stored(&env, &unbound(server.port)).as_deref(), Some("pw-1"));
}

#[tokio::test]
async fn a_wrong_remembered_password_falls_back_to_the_prompt() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "right".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new("");
    env.trust(&server);
    env.engine
        .create_host(with_password("db", server.port, "wrong"))
        .await
        .unwrap();
    let (_, out) = env
        .connect("db", |kind| match kind {
            PromptKind::Password { .. } => PromptResponse::Secret {
                value: SecretString::from("right".to_owned()),
                remember: false,
            },
            other => panic!("unexpected {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(
        stored(&env, &bound(&server)),
        None,
        "a rejected one is not bound"
    );
    assert_eq!(
        stored(&env, &unbound(server.port)),
        None,
        "a rejected one is forgotten"
    );
}

/// The joined session log of `id`.
fn logs(env: &Env, id: portico_core::ids::SessionId) -> String {
    serde_json::to_string(&env.engine.session_logs(id).unwrap()).unwrap()
}

/// With `StrictHostKeyChecking accept-new`, nobody looked at the new host
/// key: the password typed with the host (never bound to a key) is not
/// sent to whichever server answered; the user is asked instead.
#[tokio::test]
async fn unbound_password_is_withheld_after_accept_new() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "s3cret-pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new("Host *\n    StrictHostKeyChecking accept-new\n");
    env.engine
        .create_host(with_password("db", server.port, "s3cret-pw"))
        .await
        .unwrap();
    let behaviour = std::sync::Arc::clone(&server.behaviour);
    let mut logins_at_prompt = None;
    let (id, out) = env
        .connect("db", |kind| match kind {
            PromptKind::Password { .. } => {
                logins_at_prompt = Some(behaviour.logins.load(Ordering::SeqCst));
                PromptResponse::Secret {
                    value: SecretString::from("s3cret-pw".to_owned()),
                    remember: false,
                }
            }
            other => panic!("unexpected {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(logins_at_prompt, Some(0), "nothing sent before the prompt");
    assert_eq!(server.behaviour.logins.load(Ordering::SeqCst), 1);
    assert!(
        env.known_hosts().contains(&server.known_hosts_line()),
        "key accepted automatically"
    );
    assert_eq!(
        stored(&env, &unbound(server.port)).as_deref(),
        Some("s3cret-pw"),
        "kept, never tried"
    );
    assert_eq!(stored(&env, &bound(&server)), None);
    let logs = logs(&env, id);
    assert!(
        logs.contains("remembered password withheld: new host key accepted automatically"),
        "{logs}"
    );
    assert!(!logs.contains("s3cret-pw") && !out.transcript().contains("s3cret-pw"));
}

/// keyboard-interactive: the server asks "Password:" again after the
/// remembered (wrong) one; the right one typed then opens the session, and
/// the wrong one is never bound to the key: it is forgotten.
#[tokio::test]
async fn a_rejected_unbound_password_is_never_bound() {
    let server = TestServer::start(Behaviour {
        keyboard_interactive: Some(("alice".into(), "right".into())),
        kbd_reask: true,
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new("");
    env.trust(&server);
    env.engine
        .create_host(with_password("kbd", server.port, "wrong"))
        .await
        .unwrap();
    let (id, out) = env
        .connect("kbd", |kind| match kind {
            PromptKind::KeyboardInteractive { .. } => {
                PromptResponse::Answers(vec![SecretString::from("right".to_owned())])
            }
            other => panic!("unexpected {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(
        matches!(
            out.prompts.as_slice(),
            [PromptKind::KeyboardInteractive { .. }]
        ),
        "{:?}",
        out.prompts
    );
    assert_eq!(stored(&env, &bound(&server)), None, "never bound");
    assert_eq!(stored(&env, &unbound(server.port)), None, "forgotten");
    let logs = logs(&env, id);
    assert!(
        logs.contains("remembered password was rejected and forgotten"),
        "{logs}"
    );

    // The same with the right one typed and remembered: that one is bound.
    env.engine
        .update_host(
            "kbd",
            HostDraft {
                password: PasswordChange::Set(SecretString::from("wrong".to_owned())),
                ..with_password("kbd", server.port, "unused")
            },
        )
        .await
        .unwrap();
    let (_, out) = env
        .connect("kbd", |kind| match kind {
            PromptKind::KeyboardInteractive { .. } => PromptResponse::Secret {
                value: SecretString::from("right".to_owned()),
                remember: true,
            },
            other => panic!("unexpected {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(stored(&env, &bound(&server)).as_deref(), Some("right"));
    assert_eq!(stored(&env, &unbound(server.port)), None);
}

/// A password bound to a server presenting a **host certificate** (no
/// `known_hosts` line names its key) is still forgotten with the host.
#[tokio::test]
async fn delete_forgets_a_password_bound_through_a_host_certificate() {
    let (host_ca, host_key) = (new_key(), new_key());
    let cert = support::user_cert(
        &host_ca,
        &host_key,
        "127.0.0.1",
        russh::keys::ssh_key::certificate::CertType::Host,
    );
    let server = TestServer::start_with(
        Behaviour {
            password: Some(("alice".into(), "s3cret-pw".into())),
            ..Behaviour::default()
        },
        host_key,
        vec![cert],
    )
    .await;
    let mut env = Env::new("");
    env.append_known_hosts(&format!(
        "@cert-authority [127.0.0.1]:{} {}",
        server.port,
        host_ca.public_key().to_openssh().unwrap()
    ));
    env.engine
        .create_host(with_password("db", server.port, "s3cret-pw"))
        .await
        .unwrap();
    let (_, out) = env.connect("db", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(stored(&env, &bound(&server)).as_deref(), Some("s3cret-pw"));
    assert_eq!(stored(&env, &unbound(server.port)), None);
    // The index names the key; no `known_hosts` line does.
    let meta = std::fs::read_to_string(env.engine.config().paths.hosts_meta()).unwrap();
    let fingerprint = server
        .host_key
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    assert!(meta.contains(&fingerprint), "{meta}");
    assert!(!meta.contains("s3cret-pw"));

    env.engine.delete_host("db").await.unwrap();
    assert_eq!(stored(&env, &bound(&server)), None, "forgotten");
    let meta = std::fs::read_to_string(env.engine.config().paths.hosts_meta()).unwrap_or_default();
    assert!(!meta.contains(&fingerprint), "index entry removed: {meta}");
}

/// `ssh -G` (OpenSSH's own resolution, no connection) agrees with Portico
/// once the `Include` line is at the top of the config.
#[tokio::test]
async fn openssh_reads_hosts_conf_like_portico() {
    let Ok(version) = Command::new("ssh").arg("-V").output() else {
        // No OpenSSH client on this machine: nothing to compare against.
        return;
    };
    assert!(version.status.success());

    let user_config = "Host *\n    User x\n    Port 2200\n";
    let env = Env::new(user_config);
    env.engine
        .create_host(draft("bastion", "198.51.100.1"))
        .await
        .unwrap();
    env.engine
        .create_host(HostDraft {
            user: Some("deploy".into()),
            port: Some(2222),
            proxy_jump: Some("bastion".into()),
            ..draft("web", "web.example.test")
        })
        .await
        .unwrap();

    let hosts_conf = env.engine.config().paths.managed_hosts();
    let openssh_config = env.dir.path().join("openssh_config");
    std::fs::write(
        &openssh_config,
        format!("Include {}\n{user_config}", hosts_conf.display()),
    )
    .unwrap();

    let inventory = env.engine.load_hosts().await.unwrap();
    for alias in ["web", "bastion"] {
        let out = Command::new("ssh")
            .arg("-G")
            .arg("-F")
            .arg(&openssh_config)
            .arg(alias)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        let value = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{key} ")))
                .unwrap_or_else(|| panic!("no {key} in {text}"))
                .to_owned()
        };
        let host = inventory.hosts.iter().find(|h| h.alias == alias).unwrap();
        let resolved = &host.resolved;
        assert_eq!(value("hostname"), resolved.hostname, "{alias}");
        assert_eq!(value("user"), resolved.user.clone().unwrap(), "{alias}");
        assert_eq!(value("port"), resolved.port.to_string(), "{alias}");
    }
    let web = inventory.hosts.iter().find(|h| h.alias == "web").unwrap();
    assert_eq!(
        (web.resolved.user.as_deref(), web.resolved.port),
        (Some("deploy"), 2222)
    );
    let bastion = inventory
        .hosts
        .iter()
        .find(|h| h.alias == "bastion")
        .unwrap();
    assert_eq!(
        (bastion.resolved.user.as_deref(), bastion.resolved.port),
        (Some("x"), 2200)
    );
    let out = Command::new("ssh")
        .args(["-G", "-F"])
        .arg(&openssh_config)
        .arg("web")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|l| l == "proxyjump bastion")
    );
}

#[tokio::test]
async fn edit_forms_read_the_file_values_not_the_resolved_ones() {
    let env = Env::new("");
    env.engine
        .create_host(HostDraft {
            user: Some("deploy".into()),
            port: Some(2222),
            ..draft("web", "203.0.113.10")
        })
        .await
        .unwrap();
    // The user puts the Include line at the bottom, under a block that sets
    // `User` and `Port` first: OpenSSH (and Portico) resolve those.
    let include = env.engine.managed_include_line();
    std::fs::write(
        env.dir.path().join(".ssh/config"),
        format!("Host web\n    User early\n    Port 2200\n\n{include}\n"),
    )
    .unwrap();
    let inventory = env.engine.load_hosts().await.unwrap();
    let resolved = inventory
        .hosts
        .iter()
        .find(|host| host.alias == "web")
        .unwrap();
    assert_eq!(resolved.resolved.user.as_deref(), Some("early"));
    assert_eq!(resolved.resolved.port, 2200);
    // The form still gets what hosts.conf holds, so saving keeps it.
    let own = env.engine.managed_host("WEB").await.unwrap().unwrap();
    assert_eq!(own.user.as_deref(), Some("deploy"));
    assert_eq!(own.port, Some(2222));
    assert!(env.engine.managed_host("mine").await.unwrap().is_none());
}
