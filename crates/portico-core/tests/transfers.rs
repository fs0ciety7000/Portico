//! SFTP and port forwarding against the in-process server (phase 2).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use portico_core::events::{EngineEvent, SftpEvent, TunnelEvent};
use portico_core::forward::{Endpoint, ForwardSpec};
use portico_core::ids::{SessionId, TransferId, TunnelId};
use portico_core::sftp::{Direction, EntryKind, OnConflict, TransferRequest};
use support::{Behaviour, Env, TestServer, never, new_key, target_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Deterministic, incompressible-ish content.
fn content(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x9e37_79b9;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .collect()
}

struct Fixture {
    env: Env,
    id: SessionId,
    root: tempfile::TempDir,
    server: TestServer,
}

async fn connected(mut behaviour: Behaviour) -> Fixture {
    let key = new_key();
    let root = tempfile::tempdir().unwrap();
    behaviour.keys = vec![key.public_key().clone()];
    if behaviour.sftp_root.is_none()
        && !behaviour.allow_direct_tcpip
        && !behaviour.allow_tcpip_forward
    {
        behaviour.sftp_root = Some(root.path().to_owned());
    }
    let server = TestServer::start(behaviour).await;
    let mut env = Env::new(&target_config(
        "t",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let (id, out) = env.connect("t", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    Fixture {
        env,
        id,
        root,
        server,
    }
}

async fn sftp_fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let mut f = connected(Behaviour {
        sftp_root: Some(root.path().to_owned()),
        ..Behaviour::default()
    })
    .await;
    f.root = root;
    f
}

/// Waits for the end of a transfer, collecting its events.
async fn finish(env: &mut Env, id: TransferId) -> Vec<SftpEvent> {
    let mut seen = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), env.events.recv())
            .await
            .expect("transfer did not end in time")
            .unwrap();
        if let EngineEvent::Sftp { event, .. } = ev {
            let transfer = match &event {
                SftpEvent::TransferStarted { transfer, .. }
                | SftpEvent::TransferProgress { transfer, .. }
                | SftpEvent::TransferFinished { transfer, .. }
                | SftpEvent::TransferSkipped { transfer }
                | SftpEvent::TransferCancelled { transfer }
                | SftpEvent::TransferFailed { transfer, .. } => *transfer,
                _ => continue,
            };
            if transfer != id {
                continue;
            }
            let done = matches!(
                event,
                SftpEvent::TransferFinished { .. }
                    | SftpEvent::TransferSkipped { .. }
                    | SftpEvent::TransferCancelled { .. }
                    | SftpEvent::TransferFailed { .. }
            );
            seen.push(event);
            if done {
                return seen;
            }
        }
    }
}

fn no_part_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part"))
}

// ── SFTP ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn sftp_browse_and_file_operations() {
    let f = sftp_fixture().await;
    std::fs::create_dir(f.root.path().join("logs")).unwrap();
    std::fs::write(f.root.path().join("b.txt"), b"bee").unwrap();
    std::fs::write(f.root.path().join("A.txt"), b"a").unwrap();
    let e = &f.env.engine;

    assert_eq!(e.sftp_home(f.id).await.unwrap(), "/");
    let list = e.sftp_list(f.id, "/").await.unwrap();
    let names: Vec<_> = list.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(
        names,
        ["logs", "A.txt", "b.txt"],
        "dirs first, then case-insensitive"
    );
    assert_eq!(list.first().unwrap().kind, EntryKind::Dir);
    assert_eq!(list.get(2).unwrap().size, 3);
    assert_eq!(list.get(2).unwrap().path, "/b.txt");

    e.sftp_mkdir(f.id, "/new").await.unwrap();
    e.sftp_rename(f.id, "/b.txt", "/new/c.txt").await.unwrap();
    assert_eq!(
        std::fs::read(f.root.path().join("new/c.txt")).unwrap(),
        b"bee"
    );
    // Rename never replaces silently.
    let err = e
        .sftp_rename(f.id, "/A.txt", "/new/c.txt")
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "destination_exists");

    e.sftp_remove(f.id, "/new/c.txt").await.unwrap();
    e.sftp_remove(f.id, "/new").await.unwrap();
    assert!(!f.root.path().join("new").exists());

    let missing = e.sftp_list(f.id, "/nope").await.unwrap_err();
    assert_eq!(missing.summary().code, "not_found");
    // Paths cannot escape the server's root (fixture) and errors are typed.
    assert_eq!(
        e.sftp_stat(f.id, "/A.txt").await.unwrap().kind,
        EntryKind::File
    );
}

#[tokio::test]
async fn upload_then_download_round_trips_10_mb() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    let data = content(10 * 1024 * 1024 + 123);
    let src = local.path().join("big.bin");
    std::fs::write(&src, &data).unwrap();

    let up = f
        .env
        .engine
        .transfer(
            f.id,
            TransferRequest {
                direction: Direction::Upload,
                local: src.clone(),
                remote: "/big.bin".into(),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    let events = finish(&mut f.env, up).await;
    assert!(
        matches!(events.first(), Some(SftpEvent::TransferStarted { total: Some(t), direction: Direction::Upload, .. }) if *t == data.len() as u64),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(SftpEvent::TransferFinished { destination, .. }) if destination == "/big.bin")
    );
    let last_progress = events
        .iter()
        .rev()
        .find_map(|e| match e {
            SftpEvent::TransferProgress { done, .. } => Some(*done),
            _ => None,
        })
        .unwrap();
    assert_eq!(last_progress, data.len() as u64, "final progress is exact");
    assert_eq!(std::fs::read(f.root.path().join("big.bin")).unwrap(), data);
    assert!(no_part_files(f.root.path()));

    let dst = local.path().join("copy.bin");
    let down = f
        .env
        .engine
        .transfer(
            f.id,
            TransferRequest {
                direction: Direction::Download,
                local: dst.clone(),
                remote: "/big.bin".into(),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    let events = finish(&mut f.env, down).await;
    assert!(
        matches!(events.last(), Some(SftpEvent::TransferFinished { .. })),
        "{events:?}"
    );
    assert_eq!(std::fs::read(&dst).unwrap(), data);
    assert!(no_part_files(local.path()));
}

#[tokio::test]
async fn conflicts_follow_the_policy() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    std::fs::write(f.root.path().join("notes.txt"), b"remote").unwrap();
    let dst = local.path().join("notes.txt");
    std::fs::write(&dst, b"local").unwrap();
    let request = |on_conflict| TransferRequest {
        direction: Direction::Download,
        local: dst.clone(),
        remote: "/notes.txt".into(),
        on_conflict,
    };

    let id = f
        .env
        .engine
        .transfer(f.id, request(OnConflict::Ask))
        .await
        .unwrap();
    let ev = finish(&mut f.env, id).await;
    assert!(
        matches!(ev.last(), Some(SftpEvent::TransferFailed { error, .. }) if error.code == "destination_exists"),
        "{ev:?}"
    );
    assert_eq!(std::fs::read(&dst).unwrap(), b"local", "untouched");

    let id = f
        .env
        .engine
        .transfer(f.id, request(OnConflict::Skip))
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut f.env, id).await.last(),
        Some(SftpEvent::TransferSkipped { .. })
    ));

    let id = f
        .env
        .engine
        .transfer(f.id, request(OnConflict::Rename))
        .await
        .unwrap();
    let ev = finish(&mut f.env, id).await;
    let renamed = local.path().join("notes (1).txt");
    assert!(
        matches!(ev.last(), Some(SftpEvent::TransferFinished { destination, .. }) if *destination == renamed.display().to_string()),
        "{ev:?}"
    );
    assert_eq!(std::fs::read(&renamed).unwrap(), b"remote");

    let id = f
        .env
        .engine
        .transfer(f.id, request(OnConflict::Overwrite))
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut f.env, id).await.last(),
        Some(SftpEvent::TransferFinished { .. })
    ));
    assert_eq!(std::fs::read(&dst).unwrap(), b"remote");
}

#[tokio::test]
async fn cancelled_download_leaves_nothing_behind() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    std::fs::write(f.root.path().join("huge.bin"), content(96 * 1024 * 1024)).unwrap();
    let dst = local.path().join("huge.bin");
    let id = f
        .env
        .engine
        .transfer(
            f.id,
            TransferRequest {
                direction: Direction::Download,
                local: dst.clone(),
                remote: "/huge.bin".into(),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    // Cancel once data is flowing.
    loop {
        if let EngineEvent::Sftp {
            event:
                SftpEvent::TransferProgress { transfer, .. }
                | SftpEvent::TransferStarted { transfer, .. },
            ..
        } = f.env.events.recv().await.unwrap()
            && transfer == id
        {
            break;
        }
    }
    f.env.engine.cancel_transfer(id).unwrap();
    let ev = finish(&mut f.env, id).await;
    assert!(
        matches!(ev.last(), Some(SftpEvent::TransferCancelled { .. })),
        "{ev:?}"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!dst.exists());
    assert!(no_part_files(local.path()));
    // The SFTP client survives a cancelled transfer.
    assert!(!f.env.engine.sftp_list(f.id, "/").await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_subsystem_is_reported() {
    let f = connected(Behaviour {
        allow_direct_tcpip: true, // any session, but no SFTP
        ..Behaviour::default()
    })
    .await;
    let err = f.env.engine.sftp_home(f.id).await.unwrap_err();
    assert_eq!(err.summary().code, "sftp_unavailable", "{err}");
}

// ── Tunnels ────────────────────────────────────────────────────────────────

/// A TCP echo service; returns its port.
async fn echo_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

async fn tunnel_event(env: &mut Env, id: TunnelId) -> TunnelEvent {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), env.events.recv())
            .await
            .expect("tunnel event in time")
            .unwrap();
        if let EngineEvent::Tunnel { id: t, event, .. } = ev
            && t == id
        {
            return event;
        }
    }
}

fn port_of(bind: &str) -> u16 {
    bind.split(", ")
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

async fn round_trip(port: u16) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    s.write_all(b"portico tunnel").await.unwrap();
    let mut buf = [0_u8; 14];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .expect("echo in time")
        .unwrap();
    assert_eq!(&buf, b"portico tunnel");
}

fn ep(host: &str, port: u16) -> Endpoint {
    Endpoint {
        host: host.into(),
        port,
    }
}

#[tokio::test]
async fn local_forward_round_trip_and_stop() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let spec = ForwardSpec::Local {
        bind: ep("127.0.0.1", 0),
        target: ep("127.0.0.1", echo),
    };
    let id = f.env.engine.start_tunnel(f.id, spec.clone()).await.unwrap();
    let TunnelEvent::Listening { bind } = tunnel_event(&mut f.env, id).await else {
        panic!("expected Listening");
    };
    let port = port_of(&bind);
    round_trip(port).await;
    round_trip(port).await;
    assert_eq!(f.env.engine.tunnels().len(), 1);
    assert_eq!(f.env.engine.tunnels().first().unwrap().spec, spec);

    f.env.engine.stop_tunnel(id).unwrap();
    loop {
        if matches!(tunnel_event(&mut f.env, id).await, TunnelEvent::Stopped) {
            break;
        }
    }
    assert!(f.env.engine.tunnels().is_empty());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "port released"
    );
}

#[tokio::test]
async fn busy_local_port_fails_with_a_hint() {
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let id = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Local {
                bind: ep("127.0.0.1", port),
                target: ep("127.0.0.1", 9),
            },
        )
        .await
        .unwrap();
    let TunnelEvent::Failed { error } = tunnel_event(&mut f.env, id).await else {
        panic!("expected Failed");
    };
    assert_eq!(error.code, "port_in_use");
    assert!(error.hint.is_some());
    assert!(f.env.engine.tunnels().is_empty());
}

#[tokio::test]
async fn remote_forward_round_trip() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_tcpip_forward: true,
        ..Behaviour::default()
    })
    .await;
    let id = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Remote {
                bind: ep("localhost", 0),
                target: ep("127.0.0.1", echo),
            },
        )
        .await
        .unwrap();
    let TunnelEvent::Listening { bind } = tunnel_event(&mut f.env, id).await else {
        panic!("expected Listening");
    };
    round_trip(port_of(&bind)).await;
}

#[tokio::test]
async fn server_opened_channels_are_refused() {
    let mut f = connected(Behaviour {
        allow_tcpip_forward: true,
        rogue_channels: true,
        ..Behaviour::default()
    })
    .await;
    let id = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Remote {
                bind: ep("localhost", 0),
                target: ep("127.0.0.1", 9),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        tunnel_event(&mut f.env, id).await,
        TunnelEvent::Listening { .. }
    ));
    let b = &f.server.behaviour;
    for _ in 0..100 {
        if b.rogue_refused.load(Ordering::SeqCst) + b.rogue_accepted.load(Ordering::SeqCst) == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        b.rogue_accepted.load(Ordering::SeqCst),
        0,
        "unrequested channels must be refused"
    );
    assert_eq!(b.rogue_refused.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn tunnels_end_with_their_session() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let id = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Local {
                bind: ep("127.0.0.1", 0),
                target: ep("127.0.0.1", echo),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        tunnel_event(&mut f.env, id).await,
        TunnelEvent::Listening { .. }
    ));
    f.env.engine.disconnect(f.id).await.unwrap();
    loop {
        if matches!(tunnel_event(&mut f.env, id).await, TunnelEvent::Stopped) {
            break;
        }
    }
    assert!(f.env.engine.tunnels().is_empty());
}
