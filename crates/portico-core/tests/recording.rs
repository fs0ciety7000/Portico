//! Terminal recording (asciicast v2) end to end, against the in-process
//! russh server (phase 6).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::time::Duration;

use bytes::Bytes;
use portico_core::engine::{PtySize, TerminalHandle};
use portico_core::events::{EngineEvent, RecordingState};
use portico_core::ids::{SessionId, TerminalId};
use portico_core::recording::{CastEvent, RecordingStopReason, TimedEvent};
use support::{Behaviour, Env, TestServer, never, new_key, target_config};

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

async fn pty_env(behaviour: impl FnOnce(Behaviour) -> Behaviour) -> (TestServer, Env, SessionId) {
    let key = new_key();
    let server = TestServer::start(behaviour(Behaviour {
        keys: vec![key.public_key().clone()],
        ..Behaviour::default()
    }))
    .await;
    let mut env = Env::new(&target_config(
        "web",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let (id, out) = env.connect("web", never).await;
    assert!(out.connected(), "{:?}", out.phases);
    (server, env, id)
}

async fn read_until(term: &mut TerminalHandle, needle: &[u8]) -> Vec<u8> {
    let mut seen = Vec::new();
    while !seen.windows(needle.len()).any(|w| w == needle) {
        let chunk = tokio::time::timeout(Duration::from_secs(5), term.output.recv())
            .await
            .expect("terminal output in time")
            .expect("terminal still open");
        seen.extend_from_slice(&chunk);
    }
    seen
}

/// Waits for the recording event of `terminal` matching `pred`.
async fn recording_event(
    env: &mut Env,
    terminal: TerminalId,
    mut pred: impl FnMut(&RecordingState) -> bool,
) -> RecordingState {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), env.events.recv())
            .await
            .expect("recording event in time")
            .expect("event stream open");
        if let EngineEvent::Recording {
            terminal: t, state, ..
        } = event
            && t == terminal
            && pred(&state)
        {
            return state;
        }
    }
}

async fn stopped(env: &mut Env, terminal: TerminalId) -> (RecordingStopReason, u64, Duration) {
    match recording_event(env, terminal, |s| {
        matches!(s, RecordingState::Stopped { .. })
    })
    .await
    {
        RecordingState::Stopped {
            reason,
            bytes,
            duration,
            ..
        } => (reason, bytes, duration),
        other => panic!("{other:?}"),
    }
}

/// Every event of the recording `name`, read through the engine.
async fn replay(env: &Env, name: &str) -> (portico_core::recording::CastHeader, Vec<TimedEvent>) {
    let reader = env.engine.open_recording(name).await.unwrap();
    tokio::task::spawn_blocking(move || {
        let header = reader.header().clone();
        let events: Vec<TimedEvent> = reader.map(Result::unwrap).collect();
        (header, events)
    })
    .await
    .unwrap()
}

fn all_output(events: &[TimedEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match &e.event {
            CastEvent::Output(text) => Some(text.as_str()),
            CastEvent::Resize { .. } => None,
        })
        .collect()
}

// One scenario, start to delete: splitting it would repeat the setup.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn a_recorded_pty_session_is_a_valid_asciicast_v2_file() {
    let (_server, mut env, session) = pty_env(|b| Behaviour {
        pty_echo: true,
        ..b
    })
    .await;
    let mut term = env
        .engine
        .open_terminal(session, size(100, 30))
        .await
        .unwrap();
    read_until(&mut term, b"welcome").await;

    let info = env.engine.start_recording(term.id).await.unwrap();
    assert!(env.engine.is_recording(term.id));
    assert_eq!(info.session, session);
    assert!(
        info.name.starts_with("web-")
            && std::path::Path::new(&info.name).extension() == Some("cast".as_ref())
    );
    assert_eq!(info.path, env.engine.recordings_dir().join(&info.name));
    let started = recording_event(&mut env, term.id, |s| {
        matches!(s, RecordingState::Started { .. })
    })
    .await;
    assert!(matches!(started, RecordingState::Started { ref name, .. } if *name == info.name));

    term.input.send(Bytes::from_static(b"hello")).await.unwrap();
    read_until(&mut term, b"hello").await;
    term.input.resize(size(120, 40));
    // The resize reaches the writer task before this echo comes back.
    tokio::time::sleep(Duration::from_millis(50)).await;
    term.input
        .send(Bytes::from_static(b" world"))
        .await
        .unwrap();
    read_until(&mut term, b"world").await;

    env.engine.stop_recording(term.id).unwrap();
    let (reason, bytes, duration) = stopped(&mut env, term.id).await;
    assert_eq!(reason, RecordingStopReason::Requested);
    assert_eq!(
        bytes,
        std::fs::metadata(&info.path).unwrap().len(),
        "the file is complete when Stopped is emitted"
    );
    assert!(duration > Duration::ZERO);
    assert!(!env.engine.is_recording(term.id));

    // Raw format: header with exactly the ADR fields, then `o` / `r` lines.
    let text = std::fs::read_to_string(&info.path).unwrap();
    let mut lines = text.lines();
    let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut keys: Vec<&str> = header
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["env", "height", "timestamp", "title", "version", "width"]
    );
    assert_eq!(header["version"], 2);
    assert_eq!(
        (header["width"].as_u64(), header["height"].as_u64()),
        (Some(100), Some(30))
    );
    assert_eq!(header["title"], "web");
    assert_eq!(
        header["env"],
        serde_json::json!({ "TERM": "xterm-256color" })
    );
    let mut last = 0.0;
    for line in lines {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        let time = event[0].as_f64().unwrap();
        assert!(time >= last, "times never decrease");
        last = time;
        assert!(matches!(event[1].as_str(), Some("o" | "r")), "{line}");
    }

    // Parsed back through the engine.
    let (parsed_header, events) = replay(&env, &info.name).await;
    assert_eq!((parsed_header.width, parsed_header.height), (100, 30));
    assert_eq!(parsed_header.title.as_deref(), Some("web"));
    assert_eq!(parsed_header.term.as_deref(), Some("xterm-256color"));
    let output = all_output(&events);
    assert!(
        output.contains("hello") && output.contains(" world"),
        "{output:?}"
    );
    assert!(!output.contains("welcome"), "only what follows the start");
    let resize_at = events
        .iter()
        .position(|e| {
            e.event
                == CastEvent::Resize {
                    cols: 120,
                    rows: 40,
                }
        })
        .expect("resize recorded");
    let world_at = events
        .iter()
        .position(|e| matches!(&e.event, CastEvent::Output(t) if t.contains("world")))
        .unwrap();
    assert!(resize_at < world_at);

    // Listed, with permissions of a private file.
    let listed = env.engine.list_recordings().await.unwrap();
    let entry = listed.iter().find(|e| e.name == info.name).unwrap();
    assert!(entry.valid);
    assert_eq!(entry.alias, "web");
    assert_eq!(entry.size, bytes);
    assert!(entry.duration.unwrap() > 0.0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&info.path), 0o600);
        assert_eq!(mode(&env.engine.recordings_dir()), 0o700);
    }

    // The debug log names the file and its size, never its content.
    let logs = env.engine.session_logs(session).unwrap();
    let log_text: String = logs.iter().map(|r| r.message.clone()).collect();
    assert!(log_text.contains(&info.name));
    assert!(!log_text.contains("hello"));

    // A second recording of the same terminal is allowed after the stop.
    let again = env.engine.start_recording(term.id).await.unwrap();
    assert_ne!(again.name, info.name);
    env.engine.delete_recording(&info.name).await.unwrap();
    assert!(!info.path.exists());
}

#[tokio::test]
async fn typed_input_never_reaches_the_file() {
    let (server, mut env, session) = pty_env(|b| Behaviour {
        pty_no_echo: true,
        ..b
    })
    .await;
    let mut term = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    read_until(&mut term, b"welcome").await;
    let info = env.engine.start_recording(term.id).await.unwrap();

    term.input
        .send(Bytes::from_static(b"INPUT-MARKER s3cr3t\r"))
        .await
        .unwrap();
    let seen = read_until(&mut term, b"OUTPUT-MARKER").await;
    assert!(!seen.windows(12).any(|w| w == b"INPUT-MARKER"), "no echo");
    // The server did get the input.
    let received = server.behaviour.received.lock().unwrap().clone();
    assert!(received.windows(12).any(|w| w == b"INPUT-MARKER"));

    env.engine.stop_recording(term.id).unwrap();
    stopped(&mut env, term.id).await;
    let raw = std::fs::read_to_string(&info.path).unwrap();
    assert!(raw.contains("OUTPUT-MARKER"), "{raw}");
    assert!(!raw.contains("INPUT-MARKER"), "{raw}");
    assert!(!raw.contains("s3cr3t"), "{raw}");
}

#[tokio::test]
async fn closing_the_terminal_flushes_and_closes_the_recording() {
    let (_server, mut env, session) = pty_env(|b| Behaviour {
        pty_echo: true,
        ..b
    })
    .await;
    let mut term = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    read_until(&mut term, b"welcome").await;
    let info = env.engine.start_recording(term.id).await.unwrap();
    term.input.send(Bytes::from_static(b"bye\r")).await.unwrap();
    read_until(&mut term, b"bye").await;
    term.input
        .send(Bytes::from_static(b"exit\r"))
        .await
        .unwrap();

    let (reason, bytes, _) = stopped(&mut env, term.id).await;
    assert_eq!(reason, RecordingStopReason::TerminalClosed);
    assert_eq!(bytes, std::fs::metadata(&info.path).unwrap().len());
    let (_, events) = replay(&env, &info.name).await;
    assert!(all_output(&events).contains("bye"));
    // The terminal is gone: nothing more to record or stop.
    let err = env.engine.start_recording(term.id).await.unwrap_err();
    assert_eq!(err.summary().code, "recording_unknown_terminal");
    assert!(err.summary().hint.is_some());
    let err = env.engine.stop_recording(term.id).unwrap_err();
    assert_eq!(err.summary().code, "recording_unknown_terminal");
}

#[tokio::test]
async fn a_terminal_is_recorded_once_at_a_time() {
    let (_server, mut env, session) = pty_env(|b| Behaviour {
        pty_echo: true,
        ..b
    })
    .await;
    let mut term = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    read_until(&mut term, b"welcome").await;

    let err = env.engine.stop_recording(term.id).unwrap_err();
    assert_eq!(err.summary().code, "recording_not_active");

    let first = env.engine.start_recording(term.id).await.unwrap();
    let err = env.engine.start_recording(term.id).await.unwrap_err();
    assert_eq!(err.summary().code, "recording_already_active");
    assert!(err.summary().hint.is_some());
    // Two concurrent starts: exactly one wins.
    env.engine.stop_recording(term.id).unwrap();
    stopped(&mut env, term.id).await;
    let (a, b) = tokio::join!(
        env.engine.start_recording(term.id),
        env.engine.start_recording(term.id)
    );
    assert!(a.is_ok() ^ b.is_ok(), "{a:?} {b:?}");

    // Another terminal of the same session is independent.
    let mut other = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    read_until(&mut other, b"welcome").await;
    let second = env.engine.start_recording(other.id).await.unwrap();
    assert_ne!(first.name, second.name);
    assert_eq!(
        env.engine.list_recordings().await.unwrap().len(),
        3,
        "first, the concurrent winner, second"
    );

    let unknown = TerminalId::from_raw(u64::MAX);
    let err = env.engine.start_recording(unknown).await.unwrap_err();
    assert_eq!(err.summary().code, "recording_unknown_terminal");
}

#[tokio::test]
async fn recording_a_flood_never_stalls_the_terminal() {
    let (_server, mut env, session) = pty_env(|b| Behaviour {
        pty_echo: true,
        flood: true,
        ..b
    })
    .await;
    let mut term = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    let info = env.engine.start_recording(term.id).await.unwrap();

    // The UI keeps up; the recording must never slow it down.
    let target = 32 * 1024 * 1024;
    let mut received = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while received < target {
        assert!(
            tokio::time::Instant::now() < deadline,
            "PTY stalled at {received}"
        );
        let chunk = tokio::time::timeout(Duration::from_secs(5), term.output.recv())
            .await
            .expect("output keeps flowing")
            .expect("terminal open");
        received += chunk.len();
    }
    // Whether the disk kept up or the queue overflowed, the outcome is
    // explicit and the file is a valid recording up to where it stopped.
    let _ = env.engine.stop_recording(term.id);
    let (reason, bytes, _) = stopped(&mut env, term.id).await;
    assert!(
        matches!(
            reason,
            RecordingStopReason::Requested | RecordingStopReason::QueueFull
        ),
        "{reason:?}"
    );
    assert!(bytes > 0);
    let (_, events) = replay(&env, &info.name).await;
    assert!(!events.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_recordings_folder_is_refused() {
    let (_server, env, session) = pty_env(|b| Behaviour {
        pty_echo: true,
        ..b
    })
    .await;
    let elsewhere = env.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(env.dir.path().join("data")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, env.engine.recordings_dir()).unwrap();

    let mut term = env
        .engine
        .open_terminal(session, size(80, 24))
        .await
        .unwrap();
    read_until(&mut term, b"welcome").await;
    let err = env.engine.start_recording(term.id).await.unwrap_err();
    assert_eq!(err.summary().code, "recording_unsafe_path");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(!env.engine.is_recording(term.id));
    // The failed start released the terminal: once fixed, it records.
    std::fs::remove_file(env.engine.recordings_dir()).unwrap();
    env.engine.start_recording(term.id).await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn delete_and_open_refuse_anything_outside_the_folder() {
    let (_server, env, _session) = pty_env(|b| b).await;
    let dir = env.engine.recordings_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let outside = env.dir.path().join("data/precious.cast");
    std::fs::write(&outside, b"{\"version\":2,\"width\":80,\"height\":24}\n").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link.cast")).unwrap();
    let ssh_config = env.dir.path().join(".ssh/config");

    for name in [
        "../precious.cast",
        "../../.ssh/config",
        "/etc/passwd",
        "..",
        "a/../../precious.cast",
    ] {
        let err = env.engine.delete_recording(name).await.unwrap_err();
        assert_eq!(err.summary().code, "recording_invalid_name", "{name}");
        let err = env.engine.open_recording(name).await.unwrap_err();
        assert_eq!(err.summary().code, "recording_invalid_name", "{name}");
    }
    let err = env.engine.delete_recording("link.cast").await.unwrap_err();
    assert_eq!(err.summary().code, "recording_unsafe_path");
    let err = env.engine.open_recording("link.cast").await.unwrap_err();
    assert_eq!(err.summary().code, "recording_unsafe_path");
    assert!(outside.exists());
    assert!(ssh_config.exists());
    assert!(env.engine.list_recordings().await.unwrap().is_empty());
}
