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

async fn connected(behaviour: Behaviour) -> Fixture {
    connected_with(behaviour, "").await
}

async fn connected_with(mut behaviour: Behaviour, extra_config: &str) -> Fixture {
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
        &format!("  IdentityFile ~/.ssh/id_test\n{extra_config}"),
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
    finish_all(env, &[id]).await.remove(&id).unwrap_or_default()
}

/// Events of each transfer, collected until all of them ended (events of
/// one are never lost while waiting for another).
async fn finish_all(
    env: &mut Env,
    ids: &[TransferId],
) -> std::collections::HashMap<TransferId, Vec<SftpEvent>> {
    let mut seen: std::collections::HashMap<TransferId, Vec<SftpEvent>> =
        std::collections::HashMap::new();
    let mut pending = ids.len();
    while pending > 0 {
        let ev = tokio::time::timeout(Duration::from_secs(30), env.events.recv())
            .await
            .expect("transfer did not end in time")
            .unwrap();
        let EngineEvent::Sftp { event, .. } = ev else {
            continue;
        };
        let transfer = match &event {
            SftpEvent::TransferStarted { transfer, .. }
            | SftpEvent::TransferProgress { transfer, .. }
            | SftpEvent::TransferFinished { transfer, .. }
            | SftpEvent::TransferSkipped { transfer }
            | SftpEvent::TransferCancelled { transfer }
            | SftpEvent::TransferFailed { transfer, .. } => *transfer,
            _ => continue,
        };
        if !ids.contains(&transfer) {
            continue;
        }
        if matches!(
            event,
            SftpEvent::TransferFinished { .. }
                | SftpEvent::TransferSkipped { .. }
                | SftpEvent::TransferCancelled { .. }
                | SftpEvent::TransferFailed { .. }
        ) {
            pending -= 1;
        }
        seen.entry(transfer).or_default().push(event);
    }
    seen
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
    let id = f
        .env
        .engine
        .start_tunnel(f.id, spec.clone(), false)
        .await
        .unwrap();
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
            false,
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
            false,
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
            false,
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
            false,
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

// ── Review regressions (phase 2) ───────────────────────────────────────────

fn download(remote: &str, local: &Path, on_conflict: OnConflict) -> TransferRequest {
    TransferRequest {
        direction: Direction::Download,
        local: local.to_owned(),
        remote: remote.into(),
        on_conflict,
    }
}

#[tokio::test]
async fn a_refused_request_does_not_cancel_running_transfers() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    std::fs::write(f.root.path().join("big.bin"), content(48 * 1024 * 1024)).unwrap();
    std::fs::create_dir(f.root.path().join("exists")).unwrap();
    std::fs::write(f.root.path().join("exists/x"), b"x").unwrap();
    let id = f
        .env
        .engine
        .transfer(
            f.id,
            download("/big.bin", &local.path().join("big.bin"), OnConflict::Ask),
        )
        .await
        .unwrap();
    // Refused by the server (SSH_FX_FAILURE): ordinary errors.
    assert!(f.env.engine.sftp_mkdir(f.id, "/exists").await.is_err());
    assert!(f.env.engine.sftp_remove(f.id, "/exists").await.is_err());
    let events = finish(&mut f.env, id).await;
    assert!(
        matches!(events.last(), Some(SftpEvent::TransferFinished { .. })),
        "{events:?}"
    );
    assert_eq!(
        std::fs::metadata(local.path().join("big.bin"))
            .unwrap()
            .len(),
        48 * 1024 * 1024
    );
}

#[tokio::test]
async fn hostile_names_never_reach_the_listing() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ok.txt"), b"ok").unwrap();
    let f = connected(Behaviour {
        sftp_root: Some(root.path().to_owned()),
        sftp_extra_names: vec![
            "../evil".into(),
            "/abs".into(),
            "a/b".into(),
            "..".into(),
            "nul\0x".into(),
        ],
        ..Behaviour::default()
    })
    .await;
    let names: Vec<_> = f
        .env
        .engine
        .sftp_list(f.id, "/")
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["ok.txt"]);
}

#[cfg(unix)]
#[tokio::test]
async fn modes_are_carried_across() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    let secret = f.root.path().join("id_secret");
    std::fs::write(&secret, b"key").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let dst = local.path().join("id_secret");
    let id = f
        .env
        .engine
        .transfer(f.id, download("/id_secret", &dst, OnConflict::Ask))
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut f.env, id).await.last(),
        Some(SftpEvent::TransferFinished { .. })
    ));
    assert_eq!(
        std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
        0o600,
        "never widened"
    );

    let script = local.path().join("run.sh");
    std::fs::write(&script, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let id = f
        .env
        .engine
        .transfer(
            f.id,
            TransferRequest {
                direction: Direction::Upload,
                local: script,
                remote: "/run.sh".into(),
                on_conflict: OnConflict::Ask,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        finish(&mut f.env, id).await.last(),
        Some(SftpEvent::TransferFinished { .. })
    ));
    assert_eq!(
        std::fs::metadata(f.root.path().join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[tokio::test]
async fn overwrite_upload_replaces_and_cancel_keeps_the_original() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    std::fs::write(f.root.path().join("config.yml"), b"old").unwrap();
    let new = local.path().join("config.yml");
    std::fs::write(&new, b"new").unwrap();
    let request = |on_conflict| TransferRequest {
        direction: Direction::Upload,
        local: new.clone(),
        remote: "/config.yml".into(),
        on_conflict,
    };
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
    assert_eq!(
        std::fs::read(f.root.path().join("config.yml")).unwrap(),
        b"new"
    );
    let leftovers: Vec<_> = std::fs::read_dir(f.root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["config.yml"], "no .part / .old left behind");

    // Cancelled while copying: the remote original is untouched.
    let big = local.path().join("big.bin");
    std::fs::write(&big, content(64 * 1024 * 1024)).unwrap();
    std::fs::write(f.root.path().join("big.bin"), b"original").unwrap();
    let id = f
        .env
        .engine
        .transfer(
            f.id,
            TransferRequest {
                direction: Direction::Upload,
                local: big,
                remote: "/big.bin".into(),
                on_conflict: OnConflict::Overwrite,
            },
        )
        .await
        .unwrap();
    loop {
        if let EngineEvent::Sftp {
            event: SftpEvent::TransferProgress { transfer, .. },
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
    assert_eq!(
        std::fs::read(f.root.path().join("big.bin")).unwrap(),
        b"original"
    );
}

#[tokio::test]
async fn concurrent_downloads_never_share_a_partial_file() {
    let mut f = sftp_fixture().await;
    let local = tempfile::tempdir().unwrap();
    std::fs::create_dir(f.root.path().join("a")).unwrap();
    std::fs::create_dir(f.root.path().join("b")).unwrap();
    let (one, two) = (content(3 * 1024 * 1024), content(2 * 1024 * 1024 + 7));
    std::fs::write(f.root.path().join("a/config.yml"), &one).unwrap();
    std::fs::write(f.root.path().join("b/config.yml"), &two).unwrap();
    // Someone else's partial download in the same folder.
    std::fs::write(local.path().join("config.yml.part"), b"foreign").unwrap();
    let dst = local.path().join("config.yml");
    let first = f
        .env
        .engine
        .transfer(f.id, download("/a/config.yml", &dst, OnConflict::Rename))
        .await
        .unwrap();
    let second = f
        .env
        .engine
        .transfer(f.id, download("/b/config.yml", &dst, OnConflict::Rename))
        .await
        .unwrap();
    let mut events = finish_all(&mut f.env, &[first, second]).await;
    let mut done = Vec::new();
    for id in [first, second] {
        let ev = events.remove(&id).unwrap_or_default();
        let Some(SftpEvent::TransferFinished { destination, .. }) = ev.last() else {
            panic!("{ev:?}");
        };
        done.push(std::fs::read(destination).unwrap());
    }
    done.sort_by_key(Vec::len);
    assert_eq!(done, [two, one], "both intact, under two names");
    assert!(local.path().join("config (1).yml").exists());
    assert_eq!(
        std::fs::read(local.path().join("config.yml.part")).unwrap(),
        b"foreign"
    );
}

#[tokio::test]
async fn refused_subsystem_is_reported_quickly() {
    // Session channels are accepted (PTY server) but there is no SFTP.
    let f = connected(Behaviour {
        pty_echo: true,
        allow_direct_tcpip: true, // keeps the fixture from adding an SFTP root
        ..Behaviour::default()
    })
    .await;
    let started = std::time::Instant::now();
    let err = f.env.engine.sftp_home(f.id).await.unwrap_err();
    assert_eq!(err.summary().code, "sftp_unavailable", "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn exposed_tunnels_need_confirmation() {
    let f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let spec = ForwardSpec::Local {
        bind: ep("0.0.0.0", 0),
        target: ep("127.0.0.1", 9),
    };
    let err = f
        .env
        .engine
        .start_tunnel(f.id, spec, false)
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "tunnel_exposed");
    assert!(f.env.engine.tunnels().is_empty());
}

/// A TCP service that sends data forever; returns its port.
async fn flood_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let block = vec![b'x'; 64 * 1024];
                while s.write_all(&block).await.is_ok() {}
            });
        }
    });
    port
}

/// russh has one loop per connection: a local peer that stops reading a
/// busy tunnel must be cut before it freezes the whole session.
#[tokio::test]
async fn a_stalled_local_peer_does_not_freeze_the_session() {
    let flood = flood_server().await;
    let mut f = connected_with(
        Behaviour {
            allow_direct_tcpip: true,
            ..Behaviour::default()
        },
        // Real keepalive budget (the test default is 200 ms).
        "  ServerAliveInterval 2\n",
    )
    .await;
    let (engine, session) = (f.env.engine.clone(), f.id);
    let start = |target: u16| {
        let engine = engine.clone();
        let id = session;
        async move {
            engine
                .start_tunnel(
                    id,
                    ForwardSpec::Local {
                        bind: ep("127.0.0.1", 0),
                        target: ep("127.0.0.1", target),
                    },
                    false,
                )
                .await
                .unwrap()
        }
    };
    let busy = start(flood).await;
    let TunnelEvent::Listening { bind } = tunnel_event(&mut f.env, busy).await else {
        panic!("expected Listening");
    };
    // Connects to the flood and never reads.
    let _hog = tokio::net::TcpStream::connect(("127.0.0.1", port_of(&bind)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(8)).await;
    let logs: Vec<String> = f
        .env
        .engine
        .session_logs(f.id)
        .unwrap()
        .into_iter()
        .map(|l| l.message)
        .collect();
    assert!(
        logs.iter().any(|m| m.contains("stopped reading")),
        "{logs:#?}"
    );
    assert!(
        !logs.iter().any(|m| m.contains("closed (Timeout)")),
        "{logs:#?}"
    );
    // The session is still up (tunnels end with their session) and its
    // keepalives were answered. A round trip through another tunnel is
    // checked against OpenSSH in `openssh_interop` (scenario 11): russh's own
    // server stops sending on every channel while one holds unflushed data
    // for a channel the client closed, so this fixture cannot show it.
    assert!(f.env.engine.tunnels().iter().any(|t| t.id == busy));
}

// ── -D (SOCKS5) ────────────────────────────────────────────────────────────

/// A SOCKS5 client written for the test: no authentication, CONNECT to
/// `host` (sent as a domain name unless it parses as an IPv4 address).
/// Returns the reply code and the stream, ready to carry data on success.
async fn socks_connect(proxy: u16, host: &str, port: u16) -> (u8, tokio::net::TcpStream) {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut chosen = [0_u8; 2];
    s.read_exact(&mut chosen).await.unwrap();
    assert_eq!(chosen, [5, 0], "no authentication chosen");
    let mut request = vec![5, 1, 0];
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        request.push(1);
        request.extend(ip.octets());
    } else {
        request.push(3);
        request.push(u8::try_from(host.len()).unwrap());
        request.extend(host.as_bytes());
    }
    request.extend(port.to_be_bytes());
    s.write_all(&request).await.unwrap();
    let mut reply = [0_u8; 10];
    tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut reply))
        .await
        .expect("SOCKS reply in time")
        .unwrap();
    assert_eq!((reply[0], reply[3]), (5, 1), "VER, ATYP IPv4");
    (reply[1], s)
}

async fn echo_through(s: &mut tokio::net::TcpStream) {
    s.write_all(b"portico socks").await.unwrap();
    let mut buf = [0_u8; 13];
    tokio::time::timeout(Duration::from_secs(5), s.read_exact(&mut buf))
        .await
        .expect("echo in time")
        .unwrap();
    assert_eq!(&buf, b"portico socks");
}

async fn start_socks(f: &mut Fixture) -> (TunnelId, u16) {
    let spec = ForwardSpec::Dynamic {
        bind: ep("127.0.0.1", 0),
    };
    let id = f
        .env
        .engine
        .start_tunnel(f.id, spec.clone(), false)
        .await
        .unwrap();
    let TunnelEvent::Listening { bind } = tunnel_event(&mut f.env, id).await else {
        panic!("expected Listening");
    };
    assert_eq!(f.env.engine.tunnels().first().unwrap().spec, spec);
    (id, port_of(&bind))
}

#[tokio::test]
async fn dynamic_forward_by_address_and_by_name() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let (id, proxy) = start_socks(&mut f).await;

    let (code, mut by_ip) = socks_connect(proxy, "127.0.0.1", echo).await;
    assert_eq!(code, 0);
    echo_through(&mut by_ip).await;
    assert!(matches!(
        tunnel_event(&mut f.env, id).await,
        TunnelEvent::ConnectionOpened { .. }
    ));

    let (code, mut by_name) = socks_connect(proxy, "localhost", echo).await;
    assert_eq!(code, 0);
    echo_through(&mut by_name).await;
    // The name reached the server as is: resolved there, not here.
    let asked = f.server.behaviour.direct_targets.lock().unwrap().clone();
    assert_eq!(
        asked,
        [format!("127.0.0.1:{echo}"), format!("localhost:{echo}")]
    );
    drop((by_ip, by_name));
}

#[tokio::test]
async fn dynamic_forward_reports_channel_failures() {
    let closed = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let (_, proxy) = start_socks(&mut f).await;
    // sshd's ConnectFailed → "connection refused".
    let (code, _) = socks_connect(proxy, "127.0.0.1", closed).await;
    assert_eq!(code, 0x05);
    // Unsupported command: 0x07, and the tunnel keeps serving.
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    s.write_all(&[5, 1, 0, 5, 2, 0, 1, 127, 0, 0, 1, 0, 80])
        .await
        .unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, [5, 0, 5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
    let echo = echo_server().await;
    let (code, mut ok) = socks_connect(proxy, "127.0.0.1", echo).await;
    assert_eq!(code, 0);
    echo_through(&mut ok).await;
    // A name the server cannot reach: also "connection refused".
    let (code, _) = socks_connect(proxy, "localhost", closed).await;
    assert_eq!(code, 0x05);
    // Failures name their destination; a success does not (browsing
    // history stays out of the logs).
    let logs = session_messages(&f);
    for expected in [
        format!("→ 127.0.0.1:{closed}: refused or unreachable from the server"),
        format!("→ localhost:{closed}: refused or unresolvable on the server"),
    ] {
        assert!(logs.iter().any(|m| m.ends_with(&expected)), "{logs:#?}");
    }
    assert!(
        !logs.iter().any(|m| m.contains(&format!(":{echo}"))),
        "{logs:#?}"
    );

    // A server that forbids forwarding → "not allowed by ruleset".
    let mut f = connected(Behaviour {
        allow_tcpip_forward: true,
        ..Behaviour::default()
    })
    .await;
    let (_, proxy) = start_socks(&mut f).await;
    let (code, _) = socks_connect(proxy, "localhost", echo).await;
    assert_eq!(code, 0x02);
}

#[tokio::test]
async fn dynamic_forward_exposure_and_stop() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let err = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Dynamic {
                bind: ep("0.0.0.0", 0),
            },
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.summary().code, "tunnel_exposed");
    assert!(f.env.engine.tunnels().is_empty());

    let (id, proxy) = start_socks(&mut f).await;
    let (code, mut open) = socks_connect(proxy, "127.0.0.1", echo).await;
    assert_eq!(code, 0);
    echo_through(&mut open).await;
    // A client that never finishes its handshake.
    let mut idle = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    idle.write_all(&[5, 1]).await.unwrap();

    f.env.engine.stop_tunnel(id).unwrap();
    loop {
        if matches!(tunnel_event(&mut f.env, id).await, TunnelEvent::Stopped) {
            break;
        }
    }
    // Stopping cuts the connections in progress, handshakes included.
    for s in [&mut open, &mut idle] {
        let mut buf = [0_u8; 16];
        let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
            .await
            .expect("closed in time");
        assert!(matches!(n, Ok(0) | Err(_)), "{n:?}");
    }
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", proxy))
            .await
            .is_err(),
        "port released"
    );
}

/// Polls `check` for up to `within`.
async fn eventually(within: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    check()
}

fn session_messages(f: &Fixture) -> Vec<String> {
    f.env
        .engine
        .session_logs(f.id)
        .unwrap()
        .into_iter()
        .map(|l| l.message)
        .collect()
}

/// A server that confirms `direct-tcpip` after Portico stopped waiting
/// (`ConnectTimeout 1`): the late channel is closed, for -D and -L alike,
/// instead of staying open on the server forever.
#[tokio::test]
async fn late_channel_confirmations_are_closed() {
    let echo = echo_server().await;
    let mut f = connected_with(
        Behaviour {
            allow_direct_tcpip: true,
            delay_direct_tcpip: Some(Duration::from_millis(2500)),
            ..Behaviour::default()
        },
        "  ConnectTimeout 1\n",
    )
    .await;
    let b = std::sync::Arc::clone(&f.server.behaviour);
    let (_, proxy) = start_socks(&mut f).await;
    let started = std::time::Instant::now();
    let (code, _socks) = socks_connect(proxy, "127.0.0.1", echo).await;
    assert_eq!(code, 0x04, "open timed out → host unreachable");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        eventually(Duration::from_secs(10), || {
            b.direct_accepted.load(Ordering::SeqCst) == 1
                && b.closed_channels.load(Ordering::SeqCst) == 1
        })
        .await,
        "the late -D channel must be closed (accepted {}, closed {})",
        b.direct_accepted.load(Ordering::SeqCst),
        b.closed_channels.load(Ordering::SeqCst)
    );

    let local = f
        .env
        .engine
        .start_tunnel(
            f.id,
            ForwardSpec::Local {
                bind: ep("127.0.0.1", 0),
                target: ep("127.0.0.1", echo),
            },
            false,
        )
        .await
        .unwrap();
    let TunnelEvent::Listening { bind } = tunnel_event(&mut f.env, local).await else {
        panic!("expected Listening");
    };
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port_of(&bind)))
        .await
        .unwrap();
    let mut buf = [0_u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("-L gives up after ConnectTimeout");
    assert!(matches!(n, Ok(0) | Err(_)), "{n:?}");
    assert!(
        eventually(Duration::from_secs(10), || {
            b.direct_accepted.load(Ordering::SeqCst) == 2
                && b.closed_channels.load(Ordering::SeqCst) == 2
        })
        .await,
        "the late -L channel must be closed"
    );
    let logs = session_messages(&f);
    let timed_out = format!("→ 127.0.0.1:{echo}: timed out");
    assert!(
        logs.iter()
            .any(|m| m.starts_with("-D ") && m.ends_with(&timed_out)),
        "{logs:#?}"
    );
    assert!(
        logs.iter()
            .any(|m| m.starts_with("-L ") && m.ends_with(&timed_out)),
        "{logs:#?}"
    );
}

/// A SOCKS client that leaves before the server confirmed: the channel
/// opened for it is closed, not dropped bare.
#[tokio::test]
async fn a_client_gone_before_confirmation_leaves_no_channel_open() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        delay_direct_tcpip: Some(Duration::from_millis(800)),
        ..Behaviour::default()
    })
    .await;
    let b = std::sync::Arc::clone(&f.server.behaviour);
    let (_, proxy) = start_socks(&mut f).await;
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    let mut request = vec![5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1];
    request.extend(echo.to_be_bytes());
    s.write_all(&request).await.unwrap();
    let mut chosen = [0_u8; 2];
    s.read_exact(&mut chosen).await.unwrap();
    assert_eq!(chosen, [5, 0]);
    // Reset (RST), so the success reply cannot be written.
    #[allow(deprecated)] // `set_linger(0)` is exactly the reset wanted here
    s.set_linger(Some(Duration::ZERO)).unwrap();
    drop(s);
    assert!(
        eventually(Duration::from_secs(10), || {
            b.direct_accepted.load(Ordering::SeqCst) == 1
                && b.closed_channels.load(Ordering::SeqCst) == 1
        })
        .await,
        "accepted {}, closed {}",
        b.direct_accepted.load(Ordering::SeqCst),
        b.closed_channels.load(Ordering::SeqCst)
    );
}

/// A TCP service that sends `len` bytes of [`content`], then waits for the
/// client to close; returns its port.
async fn finite_source(len: usize) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = s.write_all(&content(len)).await;
                let mut sink = [0_u8; 64];
                while matches!(s.read(&mut sink).await, Ok(n) if n > 0) {}
            });
        }
    });
    port
}

/// Sibling of `a_stalled_local_peer_does_not_freeze_the_session`: a SOCKS
/// reader that pauses (a paused video) far longer than the stall limit
/// keeps its connection while the session's buffer budget lasts, and the
/// session keeps serving meanwhile.
#[tokio::test]
async fn a_paused_socks_reader_keeps_its_connection() {
    const LEN: usize = 16 * 1024 * 1024;
    let source = finite_source(LEN).await;
    let echo = echo_server().await;
    let mut f = connected_with(
        Behaviour {
            allow_direct_tcpip: true,
            ..Behaviour::default()
        },
        // Keepalive budget 3 s, stall limit 1.5 s: both shorter than the pause.
        "  ServerAliveInterval 1\n",
    )
    .await;
    let (id, proxy) = start_socks(&mut f).await;
    let (code, mut video) = socks_connect(proxy, "127.0.0.1", source).await;
    assert_eq!(code, 0);
    let mut received = vec![0_u8; 1024 * 1024];
    tokio::time::timeout(Duration::from_secs(10), video.read_exact(&mut received))
        .await
        .expect("first MiB in time")
        .unwrap();

    // Pause for 5 s; meanwhile the session serves another connection.
    let paused = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (code, mut other) = socks_connect(proxy, "127.0.0.1", echo).await;
    assert_eq!(code, 0);
    echo_through(&mut other).await;
    tokio::time::sleep_until(paused + Duration::from_secs(5)).await;

    let mut buf = vec![0_u8; 256 * 1024];
    tokio::time::timeout(Duration::from_secs(20), async {
        while received.len() < LEN {
            let n = video.read(&mut buf).await.unwrap();
            assert!(n > 0, "connection cut after {} bytes", received.len());
            received.extend_from_slice(&buf[..n]);
        }
    })
    .await
    .expect("the rest after the pause");
    assert!(received == content(LEN), "every byte, in order");

    let logs = session_messages(&f);
    assert!(
        !logs.iter().any(|m| m.contains("stopped reading")),
        "{logs:#?}"
    );
    assert!(
        !logs.iter().any(|m| m.contains("closed (Timeout)")),
        "{logs:#?}"
    );
    assert!(f.env.engine.tunnels().iter().any(|t| t.id == id));
    echo_through(&mut other).await;
}

/// A full -D tunnel answers SOCKS clients "general failure" (not a bare
/// close), and a burst of refusals is logged once, not once per connection.
#[tokio::test]
async fn refused_connections_are_logged_once_per_burst() {
    let echo = echo_server().await;
    let mut f = connected(Behaviour {
        allow_direct_tcpip: true,
        ..Behaviour::default()
    })
    .await;
    let (id, proxy) = start_socks(&mut f).await;
    // 256 clients that never finish their handshake fill the tunnel.
    let mut idle = Vec::new();
    for _ in 0..256 {
        idle.push(
            tokio::net::TcpStream::connect(("127.0.0.1", proxy))
                .await
                .unwrap(),
        );
    }
    // Let the tunnel take them all, so the next one is refused.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", proxy))
        .await
        .unwrap();
    let mut request = vec![5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1];
    request.extend(echo.to_be_bytes());
    s.write_all(&request).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut answer))
        .await
        .expect("answered in time")
        .unwrap();
    assert_eq!(
        answer,
        [5, 0, 5, 1, 0, 1, 0, 0, 0, 0, 0, 0],
        "general failure"
    );

    let mut refused = Vec::new();
    for _ in 0..300 {
        refused.push(
            tokio::net::TcpStream::connect(("127.0.0.1", proxy))
                .await
                .unwrap(),
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    f.env.engine.stop_tunnel(id).unwrap();
    while !matches!(tunnel_event(&mut f.env, id).await, TunnelEvent::Stopped) {}
    let logs = session_messages(&f);
    let refusals: Vec<&String> = logs
        .iter()
        .filter(|m| m.contains("too many connections"))
        .collect();
    assert!(!refusals.is_empty() && refusals.len() < 5, "{refusals:#?}");
    // The rest of the burst is counted, and reported when the tunnel stops.
    assert!(
        refusals
            .iter()
            .any(|m| m.ends_with("connections refused (-D): too many connections")),
        "{refusals:#?}"
    );
    drop((idle, refused));
}
