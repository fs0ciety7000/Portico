//! End-to-end session tests against an in-process russh server (phase 1a).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use portico_core::engine::PromptResponse;
use portico_core::events::{
    AuthMethod, CloseReason, EngineEvent, PromptKind, SessionEvent, SessionPhase,
};
use russh::keys::ssh_key::certificate::CertType;
use secrecy::SecretString;
use support::{Behaviour, Env, TestServer, accept_host, never, new_key, target_config, user_cert};

fn secret(s: &str) -> SecretString {
    SecretString::from(s.to_owned())
}

fn key_server(key: &russh::keys::PrivateKey) -> Behaviour {
    Behaviour {
        keys: vec![key.public_key().clone()],
        ..Behaviour::default()
    }
}

// ── host key verification ──────────────────────────────────────────────────

#[tokio::test]
async fn unknown_host_accepted_is_recorded_and_trusted_next_time() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);

    let (_, first) = env.connect("t", accept_host).await;
    assert!(first.connected(), "{:?}", first.phases);
    assert!(matches!(
        first.prompts.as_slice(),
        [PromptKind::UnknownHostKey { port, fingerprint, .. }]
            if *port == server.port && fingerprint.starts_with("SHA256:")
    ));
    assert!(
        env.known_hosts()
            .contains(&format!("[127.0.0.1]:{}", server.port))
    );

    let (_, second) = env.connect("t", never).await;
    assert!(second.connected(), "{:?}", second.phases);
}

#[tokio::test]
async fn unknown_host_rejected_fails_and_records_nothing() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);

    let (_, out) = env.connect("t", |_| PromptResponse::Cancel).await;
    assert_eq!(
        out.failure_code(),
        Some("host_key_rejected"),
        "{:?}",
        out.phases
    );
    assert!(env.known_hosts().is_empty());
}

#[tokio::test]
async fn strict_host_key_checking_refuses_unknown_without_prompt() {
    let server = TestServer::start(Behaviour::default()).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  StrictHostKeyChecking yes\n",
    ));
    let (_, out) = env.connect("t", never).await;
    assert_eq!(out.failure_code(), Some("host_key_unknown"));
}

#[tokio::test]
async fn accept_new_records_without_prompt() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  StrictHostKeyChecking accept-new\n  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    let (_, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(!env.known_hosts().is_empty());
}

#[tokio::test]
async fn changed_host_key_always_fails_hard() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    // Even `StrictHostKeyChecking no` must not let a changed key through.
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  StrictHostKeyChecking no\n  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    let impostor = new_key();
    env.append_known_hosts(&format!(
        "[127.0.0.1]:{} {}",
        server.port,
        impostor.public_key().to_openssh().unwrap()
    ));

    let (_, out) = env.connect("t", never).await;
    assert_eq!(out.failure_code(), Some("host_key_changed"));
    assert!(
        !out.phases
            .iter()
            .any(|p| matches!(p, SessionPhase::Connected))
    );
    assert!(out.events.iter().any(|e| matches!(
        e,
        EngineEvent::Session {
            event: SessionEvent::HostKeyChanged {
                known_hosts_line: 1,
                ..
            },
            ..
        }
    )));
    assert_eq!(
        server
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn changed_key_detected_with_hashed_entry_and_mixed_case_alias() {
    use base64::Engine as _;
    use hmac::{KeyInit, Mac};
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  HostKeyAlias Prod-DB\n  StrictHostKeyChecking accept-new\n  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    // `HashKnownHosts yes` entry for the lowercase name, holding another key.
    let salt = [9u8; 20];
    let mut mac = <hmac::Hmac<sha1::Sha1> as KeyInit>::new_from_slice(&salt).unwrap();
    mac.update(b"prod-db");
    let b64 = base64::engine::general_purpose::STANDARD;
    let impostor = new_key();
    env.append_known_hosts(&format!(
        "|1|{}|{} {}",
        b64.encode(salt),
        b64.encode(mac.finalize().into_bytes()),
        impostor.public_key().to_openssh().unwrap()
    ));
    let (_, out) = env.connect("t", never).await;
    assert_eq!(
        out.failure_code(),
        Some("host_key_changed"),
        "{:?}",
        out.phases
    );
    assert_eq!(
        server
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn accept_new_refuses_new_algorithm_for_known_host() {
    let server = TestServer::start(Behaviour::default()).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  StrictHostKeyChecking accept-new\n",
    ));
    let ecdsa = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ecdsa {
            curve: russh::keys::EcdsaCurve::NistP256,
        },
    )
    .unwrap();
    let line = format!(
        "[127.0.0.1]:{} {}",
        server.port,
        ecdsa.public_key().to_openssh().unwrap()
    );
    env.append_known_hosts(&line);
    let before = env.known_hosts();
    let (_, out) = env.connect("t", never).await;
    assert_eq!(
        out.failure_code(),
        Some("host_key_new_algorithm"),
        "{:?}",
        out.phases
    );
    assert_eq!(env.known_hosts(), before);
}

#[tokio::test]
async fn revoked_host_key_is_refused() {
    let server = TestServer::start(Behaviour::default()).await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    env.append_known_hosts(&format!(
        "@revoked * {}",
        server.host_key.public_key().to_openssh().unwrap()
    ));
    let (_, out) = env.connect("t", never).await;
    assert_eq!(out.failure_code(), Some("host_key_revoked"));
}

#[tokio::test]
async fn host_certificate_from_trusted_ca_needs_no_prompt() {
    let key = new_key();
    let (host_ca, host_key) = (new_key(), new_key());
    let cert = user_cert(&host_ca, &host_key, "127.0.0.1", CertType::Host);
    let server = TestServer::start_with(key_server(&key), host_key, vec![cert]).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.append_known_hosts(&format!(
        "@cert-authority [127.0.0.1]:{} {}",
        server.port,
        host_ca.public_key().to_openssh().unwrap()
    ));
    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    let logs = env.engine.session_logs(id).unwrap();
    assert!(
        logs.iter().any(|l| l.message.contains("trusted CA")),
        "{logs:#?}"
    );
}

// ── authentication ─────────────────────────────────────────────────────────

#[tokio::test]
async fn encrypted_key_asks_passphrase_retries_and_remembers() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, Some("correct horse"));
    env.trust(&server);

    let mut answers = vec![
        PromptResponse::Secret {
            value: secret("wrong"),
            remember: false,
        },
        PromptResponse::Secret {
            value: secret("correct horse"),
            remember: true,
        },
    ]
    .into_iter();
    let (_, out) = env
        .connect("t", |kind| {
            assert!(matches!(kind, PromptKind::KeyPassphrase { .. }), "{kind:?}");
            answers.next().unwrap()
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(out.prompts.len(), 2);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::PublicKey
    }));
    assert!(!out.transcript().contains("correct horse"));

    // Remembered: no prompt the second time.
    let (_, again) = env.connect("t", never).await;
    assert!(again.connected());
}

#[tokio::test]
async fn passphrase_is_not_asked_for_a_key_the_server_refuses() {
    let key = new_key();
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "s3cret".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, Some("unused"));
    env.trust(&server);

    let (_, out) = env
        .connect("t", |kind| match kind {
            PromptKind::Password { user, .. } if user == "alice" => PromptResponse::Secret {
                value: secret("s3cret"),
                remember: false,
            },
            other => panic!("unexpected prompt {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(out.prompts.len(), 1);
}

#[tokio::test]
async fn user_certificate_is_tried_first() {
    let (ca, key) = (new_key(), new_key());
    let server = TestServer::start(Behaviour {
        user_ca: Some(ca.public_key().clone()),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    let cert = user_cert(&ca, &key, "alice", CertType::User);
    std::fs::write(env.ssh("id_test-cert.pub"), cert.to_openssh().unwrap()).unwrap();
    env.trust(&server);

    let (_, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Certificate
    }));
}

#[tokio::test]
async fn password_retry_remember_and_no_leak() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "hunter22".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);

    let mut answers = vec![
        PromptResponse::Secret {
            value: secret("nope"),
            remember: false,
        },
        PromptResponse::Secret {
            value: secret("hunter22"),
            remember: true,
        },
    ]
    .into_iter();
    let (id, out) = env.connect("t", |_| answers.next().unwrap()).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(out.prompts.len(), 2);

    let logs = serde_json::to_string(&env.engine.session_logs(id).unwrap()).unwrap();
    for haystack in [out.transcript(), logs] {
        assert!(!haystack.contains("hunter22"));
        assert!(!haystack.contains("nope"));
    }
    let (_, again) = env.connect("t", never).await;
    assert!(again.connected(), "remembered password must be used");
}

#[tokio::test]
async fn keyboard_interactive() {
    let server = TestServer::start(Behaviour {
        keyboard_interactive: Some(("alice".into(), "otp-123".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    let (_, out) = env
        .connect("t", |kind| match kind {
            PromptKind::KeyboardInteractive { prompts, .. } => {
                assert_eq!(prompts.len(), 1);
                assert!(!prompts[0].echo);
                PromptResponse::Answers(vec![secret("otp-123")])
            }
            other => panic!("unexpected prompt {other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
}

#[tokio::test]
async fn all_methods_exhausted_reports_auth_failed() {
    let server = TestServer::start(Behaviour::default()).await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    let (_, out) = env.connect("t", wrong_password).await;
    assert_eq!(out.failure_code(), Some("auth_failed"), "{:?}", out.phases);
}

#[tokio::test]
async fn dismissing_the_password_prompt_ends_without_failure() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    let (_, out) = env.connect("t", |_| PromptResponse::Cancel).await;
    assert!(
        matches!(
            out.last(),
            SessionPhase::Closed {
                reason: CloseReason::UserRequested
            }
        ),
        "{:?}",
        out.phases
    );
}

/// Closing a tab while its host asks for a password must abort the
/// connection at once and withdraw the dialog (no orphan prompt that could
/// still be answered).
#[tokio::test]
async fn disconnect_while_prompting_aborts_and_withdraws_the_prompt() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    let id = env
        .engine
        .connect(portico_core::engine::ConnectRequest::new("t"))
        .unwrap();
    let prompt = loop {
        match tokio::time::timeout(Duration::from_secs(10), env.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            EngineEvent::Prompt(p) if p.session == Some(id) => break p.id,
            _ => {}
        }
    };
    env.engine.disconnect(id).await.unwrap();
    let (mut withdrawn, mut closed) = (false, false);
    while !(withdrawn && closed) {
        match tokio::time::timeout(Duration::from_secs(1), env.events.recv())
            .await
            .expect("disconnect must take effect within a second")
            .unwrap()
        {
            EngineEvent::PromptResolved { id: p } if p == prompt => withdrawn = true,
            EngineEvent::Session {
                id: sid,
                event: SessionEvent::Phase(phase),
            } if sid == id => {
                assert!(
                    matches!(
                        phase,
                        SessionPhase::Closed {
                            reason: CloseReason::UserRequested
                        }
                    ),
                    "{phase:?}"
                );
                closed = true;
            }
            _ => {}
        }
    }
    // The withdrawn prompt can no longer be answered.
    assert!(env.engine.respond(prompt, PromptResponse::Cancel).is_err());
}

#[tokio::test]
async fn shutdown_and_wait_closes_sessions_cleanly() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_ed25519\n",
    ));
    env.write_key("id_ed25519", &key, None);
    env.trust(&server);
    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(env.engine.shutdown_and_wait(Duration::from_secs(2)).await);
    let closed = env
        .wait_for(id, |e| {
            matches!(e, SessionEvent::Phase(SessionPhase::Closed { .. }))
        })
        .await;
    assert!(matches!(
        closed,
        SessionEvent::Phase(SessionPhase::Closed {
            reason: CloseReason::UserRequested
        })
    ));
}

fn wrong_password(kind: &PromptKind) -> PromptResponse {
    match kind {
        PromptKind::Password { .. } => PromptResponse::Secret {
            value: secret("wrong"),
            remember: false,
        },
        _ => PromptResponse::Cancel,
    }
}

// ── routing, lifecycle, latency ────────────────────────────────────────────

#[tokio::test]
async fn proxy_jump_two_hops() {
    let key = new_key();
    let bastion = TestServer::start(Behaviour {
        keys: vec![key.public_key().clone()],
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let target = TestServer::start(key_server(&key)).await;
    let config = format!(
        "{}{}",
        target_config("bastion", bastion.port, ""),
        target_config("target", target.port, "  ProxyJump bastion\n"),
    );
    let mut env = Env::new(&format!("IdentityFile ~/.ssh/id_test\n{config}"));
    env.write_key("id_test", &key, None);
    env.trust(&bastion);
    env.trust(&target);

    let (_, out) = env.connect("target", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    let hops: Vec<(u8, u8)> = out
        .phases
        .iter()
        .filter_map(|p| match p {
            SessionPhase::Connecting { hop, hops, .. } => Some((*hop, *hops)),
            _ => None,
        })
        .collect();
    assert_eq!(hops, [(1, 2), (2, 2)]);
    assert_eq!(
        target
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn proxy_jump_cycle_fails_fast() {
    let mut env = Env::new("Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n");
    let (_, out) = env.connect("a", never).await;
    assert_eq!(out.failure_code(), Some("proxy_jump_cycle"));
}

#[tokio::test]
async fn connection_refused_has_actionable_hint() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut env = Env::new(&target_config("t", port, ""));
    let (_, out) = env.connect("t", never).await;
    let SessionPhase::Failed { error } = out.last() else {
        panic!("{:?}", out.phases)
    };
    assert_eq!(error.code, "connect");
    assert!(error.hint.as_deref().unwrap_or_default().contains("sshd"));
}

#[tokio::test]
async fn silent_server_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s); // accept, never speak SSH
        }
    });
    let mut env = Env::with_timeout(&target_config("t", port, ""), Duration::from_millis(300));
    let (_, out) = env.connect("t", never).await;
    assert_eq!(out.failure_code(), Some("timeout"));
}

#[tokio::test]
async fn phases_latency_banner_and_disconnect() {
    let key = new_key();
    let server = TestServer::start(Behaviour {
        keys: vec![key.public_key().clone()],
        banner: Some("Authorized use only".into()),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);

    let (id, out) = env.connect("t", never).await;
    let kinds: Vec<&str> = out
        .phases
        .iter()
        .map(|p| match p {
            SessionPhase::Resolving => "resolving",
            SessionPhase::Connecting { .. } => "connecting",
            SessionPhase::Handshake => "handshake",
            SessionPhase::VerifyingHostKey => "verifying",
            SessionPhase::Authenticating { .. } => "authenticating",
            SessionPhase::Connected => "connected",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "resolving",
            "connecting",
            "handshake",
            "verifying",
            "authenticating",
            "connected"
        ]
    );
    assert!(out.events.iter().any(|e| matches!(
        e,
        EngineEvent::Session { event: SessionEvent::Banner { text }, .. } if text == "Authorized use only"
    )));

    let latency = env
        .wait_for(id, |e| matches!(e, SessionEvent::Latency { .. }))
        .await;
    let SessionEvent::Latency { rtt } = latency else {
        unreachable!()
    };
    assert!(rtt < Duration::from_secs(2));

    env.engine.disconnect(id).await.unwrap();
    let closed = env
        .wait_for(id, |e| {
            matches!(e, SessionEvent::Phase(SessionPhase::Closed { .. }))
        })
        .await;
    assert!(matches!(
        closed,
        SessionEvent::Phase(SessionPhase::Closed {
            reason: CloseReason::UserRequested
        })
    ));
    assert!(!env.engine.session_logs(id).unwrap().is_empty());
}

// ── ssh-agent ──────────────────────────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn agent_identity_signs() {
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = support::start_agent(agent_dir.path(), std::slice::from_ref(&key)).await;
    let mut env = Env::with_agent(&target_config("t", server.port, ""), socket);
    env.trust(&server);
    let (_, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Agent
    }));
}

#[cfg(unix)]
#[tokio::test]
async fn identities_only_hides_unrelated_agent_keys() {
    let (agent_key, configured) = (new_key(), new_key());
    let server = TestServer::start(key_server(&agent_key)).await;
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = support::start_agent(agent_dir.path(), std::slice::from_ref(&agent_key)).await;
    let mut env = Env::with_agent(
        &target_config(
            "t",
            server.port,
            "  IdentitiesOnly yes\n  IdentityFile ~/.ssh/id_other\n",
        ),
        socket,
    );
    env.write_key("id_other", &configured, None);
    env.trust(&server);
    let (_, out) = env.connect("t", wrong_password).await;
    assert_eq!(out.failure_code(), Some("auth_failed"), "{:?}", out.phases);
    assert!(!out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Agent
    }));
}

// ── review regressions (protocol) ──────────────────────────────────────────

#[tokio::test]
async fn proxy_command_never_connects_directly() {
    let server = TestServer::start(Behaviour::default()).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  ProxyCommand nc %h %p\n",
    ));
    env.trust(&server);
    let (_, out) = env.connect("t", never).await;
    assert_eq!(out.failure_code(), Some("unsupported"));
    assert_eq!(
        server
            .behaviour
            .connections
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[cfg(unix)]
#[tokio::test]
async fn agent_refusal_moves_on_to_password() {
    let key = new_key();
    let server = TestServer::start(Behaviour {
        keys: vec![key.public_key().clone()],
        password: Some(("alice".into(), "pw".into())),
        ..Behaviour::default()
    })
    .await;
    let agent_dir = tempfile::tempdir().unwrap();
    let socket =
        support::start_agent_with(agent_dir.path(), std::slice::from_ref(&key), true).await;
    let mut env = Env::with_agent(&target_config("t", server.port, ""), socket);
    env.trust(&server);
    let (_, out) = env
        .connect("t", |k| match k {
            PromptKind::Password { .. } => PromptResponse::Secret {
                value: secret("pw"),
                remember: false,
            },
            other => panic!("{other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Agent
    }));
    assert!(out.phases.contains(&SessionPhase::Authenticating {
        method: AuthMethod::Password
    }));
}

#[tokio::test]
async fn dismissed_multi_step_keyboard_interactive_does_not_hang() {
    let server = TestServer::start(Behaviour {
        keyboard_interactive: Some(("alice".into(), "x".into())),
        kbd_two_steps: true,
        password: Some(("alice".into(), "pw".into())),
        ..Behaviour::default()
    })
    .await;
    let mut env = Env::new(&target_config("t", server.port, ""));
    env.trust(&server);
    let (_, out) = env
        .connect("t", |k| match k {
            PromptKind::KeyboardInteractive { .. } => PromptResponse::Cancel,
            PromptKind::Password { .. } => PromptResponse::Secret {
                value: secret("pw"),
                remember: false,
            },
            other => panic!("{other:?}"),
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
}

#[cfg(unix)]
#[tokio::test]
async fn public_key_only_identity_selects_the_agent_key() {
    let (wanted, other) = (new_key(), new_key());
    let server = TestServer::start(key_server(&wanted)).await;
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = support::start_agent(agent_dir.path(), &[other, wanted.clone()]).await;
    let mut env = Env::with_agent(
        &target_config(
            "t",
            server.port,
            "  IdentitiesOnly yes\n  IdentityFile ~/.ssh/k.pub\n",
        ),
        socket,
    );
    std::fs::write(env.ssh("k.pub"), wanted.public_key().to_openssh().unwrap()).unwrap();
    env.trust(&server);
    let (_, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    // Only the selected key was offered.
    assert_eq!(
        server
            .behaviour
            .pubkey_offers
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[cfg(unix)]
#[tokio::test]
async fn preferred_authentications_password_skips_keys() {
    let key = new_key();
    let server = TestServer::start(Behaviour {
        keys: vec![key.public_key().clone()],
        password: Some(("alice".into(), "pw".into())),
        ..Behaviour::default()
    })
    .await;
    let agent_dir = tempfile::tempdir().unwrap();
    let socket = support::start_agent(agent_dir.path(), std::slice::from_ref(&key)).await;
    let mut env = Env::with_agent(
        &target_config("t", server.port, "  PreferredAuthentications password\n"),
        socket,
    );
    env.trust(&server);
    let (_, out) = env
        .connect("t", |_| PromptResponse::Secret {
            value: secret("pw"),
            remember: false,
        })
        .await;
    assert!(out.connected(), "{:?}", out.phases);
    assert_eq!(
        server
            .behaviour
            .pubkey_offers
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

// ── phase 1b: PTY ──────────────────────────────────────────────────────────

async fn next_output(handle: &mut portico_core::engine::TerminalHandle, needle: &[u8]) -> Vec<u8> {
    let mut seen = Vec::new();
    while !seen.windows(needle.len()).any(|w| w == needle) {
        let chunk = tokio::time::timeout(Duration::from_secs(5), handle.output.recv())
            .await
            .expect("terminal output in time")
            .expect("terminal still open");
        seen.extend_from_slice(&chunk);
    }
    seen
}

#[tokio::test]
async fn terminal_echo_resize_exit() {
    use portico_core::engine::PtySize;
    use portico_core::events::TerminalEvent;

    let key = new_key();
    let server = TestServer::start(Behaviour {
        pty_echo: true,
        ..key_server(&key)
    })
    .await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);

    let size = PtySize {
        cols: 100,
        rows: 30,
        pixel_width: 0,
        pixel_height: 0,
    };
    let mut term = env.engine.open_terminal(id, size).await.unwrap();
    assert_eq!(
        server.behaviour.term.lock().unwrap().as_deref(),
        Some("xterm-256color")
    );
    next_output(&mut term, b"welcome").await;

    term.input
        .send(bytes::Bytes::from_static(b"hello"))
        .await
        .unwrap();
    next_output(&mut term, b"hello").await;

    term.input.resize(PtySize {
        cols: 120,
        rows: 40,
        pixel_width: 0,
        pixel_height: 0,
    });
    term.input
        .send(bytes::Bytes::from_static(b"\rexit\r"))
        .await
        .unwrap();

    let mut exited = None;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), env.events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            EngineEvent::Terminal {
                event: TerminalEvent::Exited { status, .. },
                ..
            } => exited = Some(status),
            EngineEvent::Terminal {
                event: TerminalEvent::Closed,
                ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(exited, Some(Some(3)));
    assert_eq!(
        *server.behaviour.last_resize.lock().unwrap(),
        Some((120, 40))
    );
}

#[tokio::test]
async fn terminal_requested_while_connecting_is_served_once_connected() {
    use portico_core::engine::PtySize;
    let key = new_key();
    let server = TestServer::start(Behaviour {
        pty_echo: true,
        ..key_server(&key)
    })
    .await;
    let env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let id = env
        .engine
        .connect(portico_core::engine::ConnectRequest::new("t"))
        .unwrap();
    let size = PtySize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    let mut term = env.engine.open_terminal(id, size).await.unwrap();
    next_output(&mut term, b"welcome").await;
}

#[tokio::test]
async fn terminal_on_a_server_refusing_shells_reports_it() {
    use portico_core::engine::PtySize;
    let key = new_key();
    let server = TestServer::start(key_server(&key)).await; // no pty_echo: channel rejected
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let (id, _) = env.connect("t", never).await;
    let size = PtySize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    let err = env.engine.open_terminal(id, size).await.unwrap_err();
    assert!(
        matches!(err.summary().code.as_str(), "ssh" | "terminal_refused"),
        "{err}"
    );
}

// ── phase 1b review regressions (protocol) ────────────────────────────────

async fn pty_env(
    behaviour: Behaviour,
    key: &russh::keys::PrivateKey,
) -> (TestServer, Env, portico_core::ids::SessionId) {
    let server = TestServer::start(behaviour).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", key, None);
    env.trust(&server);
    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    (server, env, id)
}

fn size() -> portico_core::engine::PtySize {
    portico_core::engine::PtySize {
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[tokio::test]
async fn ctrl_c_is_delivered_during_an_output_flood() {
    let key = new_key();
    let (server, env, id) = pty_env(
        Behaviour {
            pty_echo: true,
            flood: true,
            ..key_server(&key)
        },
        &key,
    )
    .await;
    let mut term = env.engine.open_terminal(id, size()).await.unwrap();
    // Slow UI: let the flood saturate every queue before typing.
    tokio::time::sleep(Duration::from_millis(300)).await;
    term.input
        .send(bytes::Bytes::from_static(b"\x03"))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !server.behaviour.received.lock().unwrap().contains(&0x03) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "Ctrl-C starved by output"
        );
        // Keep draining a little, like a slow UI.
        let _ = tokio::time::timeout(Duration::from_millis(20), term.output.recv()).await;
    }
}

#[tokio::test]
async fn pty_modes_are_sent() {
    let key = new_key();
    let (server, env, id) = pty_env(
        Behaviour {
            pty_echo: true,
            ..key_server(&key)
        },
        &key,
    )
    .await;
    let _term = env.engine.open_terminal(id, size()).await.unwrap();
    let modes = server.behaviour.modes.lock().unwrap().clone();
    for wanted in [
        (russh::Pty::VERASE, 0x7f),
        (russh::Pty::ICRNL, 1),
        (russh::Pty::IUTF8, 1),
    ] {
        assert!(modes.contains(&wanted), "{wanted:?} missing from {modes:?}");
    }
}

#[tokio::test]
async fn dropping_the_view_closes_the_channel() {
    let key = new_key();
    let (server, env, id) = pty_env(
        Behaviour {
            pty_echo: true,
            ..key_server(&key)
        },
        &key,
    )
    .await;
    let mut term = env.engine.open_terminal(id, size()).await.unwrap();
    next_output(&mut term, b"welcome").await;
    // A second terminal keeps the session alive: only the first must close.
    let mut other = env.engine.open_terminal(id, size()).await.unwrap();
    next_output(&mut other, b"welcome").await;
    drop(term);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while server
        .behaviour
        .closed_channels
        .load(std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "channel never closed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The other terminal still works.
    other
        .input
        .send(bytes::Bytes::from_static(b"ping"))
        .await
        .unwrap();
    next_output(&mut other, b"ping").await;
}

#[tokio::test]
async fn exit_signal_then_eof_without_close_is_handled() {
    use portico_core::events::TerminalEvent;
    let key = new_key();
    let (server, mut env, id) = pty_env(
        Behaviour {
            pty_echo: true,
            ..key_server(&key)
        },
        &key,
    )
    .await;
    let term = env.engine.open_terminal(id, size()).await.unwrap();
    term.input
        .send(bytes::Bytes::from_static(b"kill\r"))
        .await
        .unwrap();
    let mut signal = None;
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), env.events.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            EngineEvent::Terminal {
                event: TerminalEvent::Exited { signal: s, .. },
                ..
            } => signal = s,
            EngineEvent::Terminal {
                event: TerminalEvent::Closed,
                ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(signal.as_deref(), Some("TERM"));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while server
        .behaviour
        .closed_channels
        .load(std::sync::atomic::Ordering::SeqCst)
        == 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "client never closed after EOF"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ── Phase 4: recent sessions and debug logs ────────────────────────────────

/// Polls `sessions_with_logs` until `id` is listed as ended.
async fn ended_info(
    env: &Env,
    id: portico_core::ids::SessionId,
) -> portico_core::logs::SessionLogInfo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(info) = env
            .engine
            .sessions_with_logs()
            .into_iter()
            .find(|i| i.id == id && i.ended.is_some())
        {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session never ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn connected_sessions_are_recent_and_logs_outlive_them() {
    use portico_core::logs::SessionOutcome;

    let key = new_key();
    let server = TestServer::start(key_server(&key)).await;
    let mut env = Env::new(&format!(
        "{}{}",
        target_config("t", server.port, "  IdentityFile ~/.ssh/id_test\n"),
        target_config("nokey", server.port, "  IdentityFile ~/.ssh/missing\n"),
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);

    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    // Recorded in the background, then announced.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), env.events.recv())
            .await
            .expect("recent never recorded")
            .unwrap();
        if matches!(event, EngineEvent::RecentChanged) {
            break;
        }
    }
    let recent = env.engine.recent(10).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!((recent[0].alias.as_str(), recent[0].count), ("t", 1));
    assert!(
        env.dir.path().join("data/state/recent.toml").exists(),
        "state file in the data dir"
    );

    // Live sessions are listed too.
    let live = env.engine.sessions_with_logs();
    assert!(
        live.iter()
            .any(|i| i.id == id && i.ended.is_none() && !i.probe)
    );

    env.engine.disconnect(id).await.unwrap();
    let info = ended_info(&env, id).await;
    assert_eq!(info.alias, "t");
    assert_eq!(
        info.outcome,
        Some(SessionOutcome::Closed {
            reason: CloseReason::UserRequested
        })
    );
    let logs = env.engine.session_logs(id).unwrap();
    assert!(logs.iter().any(|r| r.target == "auth"), "{logs:?}");
    assert!(logs.iter().all(|r| !r.to_line().contains('\n')));

    // A failed session keeps its logs and is not a recent host.
    let (failed, out) = env.connect("nokey", |_| PromptResponse::Cancel).await;
    assert!(!out.connected(), "{:?}", out.phases);
    let info = ended_info(&env, failed).await;
    assert!(info.outcome.is_some());
    assert!(!env.engine.session_logs(failed).unwrap().is_empty());
    let recent = env.engine.recent(10).await.unwrap();
    assert_eq!(recent.len(), 1, "{recent:?}");

    assert!(env.engine.forget_recent("t").await.unwrap());
    assert!(env.engine.recent(10).await.unwrap().is_empty());
    env.engine.clear_recent().await.unwrap();
}

#[tokio::test]
async fn only_the_last_ended_sessions_keep_their_logs() {
    use portico_core::engine::ENDED_SESSIONS_KEPT;

    // Nothing listens there: every session fails fast.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut env = Env::new(&target_config("down", port, ""));
    let mut ids = Vec::new();
    for _ in 0..ENDED_SESSIONS_KEPT + 3 {
        let (id, out) = env.connect("down", never).await;
        assert_eq!(out.failure_code(), Some("connect"), "{:?}", out.phases);
        ended_info(&env, id).await;
        ids.push(id);
    }
    let listed = env.engine.sessions_with_logs();
    assert_eq!(listed.len(), ENDED_SESSIONS_KEPT);
    assert_eq!(listed.first().map(|i| i.id), ids.get(3).copied());
    assert!(env.engine.session_logs(ids[0]).is_err());
    assert!(!env.engine.session_logs(ids[3]).unwrap().is_empty());
    assert!(env.engine.recent(10).await.unwrap().is_empty());
}
