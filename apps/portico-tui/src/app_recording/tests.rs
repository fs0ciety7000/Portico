//! Recording in the app: the toggle and its confirmation, the REC
//! indicator driven by events, stop notices, the lagged-bus resync, the
//! panel and the player, the last ones end to end against an in-process
//! SSH server (no network).

use std::path::Path;
use std::sync::atomic::Ordering;

use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::EngineEvent;
use portico_core::ids::SessionId;
use portico_core::recording::RecordingEntry;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::broadcast;

use super::*;
use crate::app::{SessionView, test_app};
use crate::app_panes::Tab;
use crate::dialog::ARM_DELAY;
use crate::keymap::{KeyContext, Keymap, map_key};
use crate::player::PlayerState;
use crate::ssh_support::{Behaviour, Env, TestServer, new_key, target_config};
use crate::terminal::GridSize;

fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect()
        })
        .collect()
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

/// The visible text of an emulator grid.
fn grid_text<T>(term: &alacritty_terminal::Term<T>) -> String {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    let grid = term.grid();
    (0..term.screen_lines())
        .map(|y| {
            let line = Line(i32::try_from(y).unwrap());
            (0..term.columns())
                .map(|x| grid[line][Column(x)].c)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The front dialog, armed as if opened long enough ago.
fn arm_dialog(app: &mut App) {
    if let Some(Dialog::Confirm(c)) = app.dialogs.front_mut() {
        c.armed = Some(
            Instant::now()
                .checked_sub(ARM_DELAY * 2)
                .unwrap_or_else(Instant::now),
        );
    }
}

fn arm_panel(app: &mut App) {
    if let Some(c) = app
        .recordings_panel
        .as_mut()
        .and_then(|p| p.confirm.as_mut())
    {
        c.opened_at = Instant::now()
            .checked_sub(ARM_DELAY * 2)
            .unwrap_or_else(Instant::now);
    }
}

fn started(terminal: TerminalId, name: &str) -> Action {
    Action::Engine(EngineEvent::Recording {
        terminal,
        session: SessionId::next(),
        state: RecordingState::Started {
            name: name.into(),
            path: format!("/data/recordings/{name}"),
        },
    })
}

fn stopped(terminal: TerminalId, reason: RecordingStopReason) -> Action {
    Action::Engine(EngineEvent::Recording {
        terminal,
        session: SessionId::next(),
        state: RecordingState::Stopped {
            reason,
            name: "web-01.cast".into(),
            path: "/data/recordings/web-01.cast".into(),
            bytes: 120,
            duration: Duration::from_secs(3),
        },
    })
}

// ── without a server ───────────────────────────────────────────────────────

#[test]
fn the_badge_follows_events_never_the_start_answer() {
    let mut app = test_app();
    let terminal = TerminalId::next();
    // `start_recording` succeeded, but `Started` has not arrived: no REC.
    app.update(Action::RecordingStart(Ok(())));
    assert!(app.recording.is_empty());
    assert!(!text(&screen(&mut app, 80, 24)).contains("REC"));

    app.update(started(terminal, "web-01.cast"));
    assert!(app.is_recorded(terminal));
    let lines = screen(&mut app, 80, 24);
    assert!(lines[23].ends_with("● REC "), "{}", text(&lines));
    // Two terminals: counted.
    app.update(started(TerminalId::next(), "db.cast"));
    assert!(screen(&mut app, 80, 24)[23].contains("● REC 2"));

    // Off only with `Stopped`.
    app.update(stopped(terminal, RecordingStopReason::Requested));
    assert!(!app.is_recorded(terminal));
    assert_eq!(app.recording.len(), 1);
    // A requested stop says nothing more.
    assert!(app.note.is_none());

    // A failed start is reported in the status bar, from its summary.
    app.update(Action::RecordingStart(Err(ErrorSummary {
        code: "recording_create_failed".into(),
        message: "cannot create the recording /x: disk full".into(),
        hint: Some("Check the free disk space.".into()),
    })));
    let note = app.note.clone().unwrap();
    assert_eq!(note.message, "Cannot create the recording /x: disk full");
    assert_eq!(note.hint, "Check the free disk space.");
}

#[test]
fn stops_on_their_own_show_a_notice_with_a_hint() {
    let cases = [
        (
            RecordingStopReason::QueueFull,
            "Recording stopped: the disk could not keep up",
        ),
        (
            RecordingStopReason::WriteFailed,
            "Recording stopped: the file could not be written",
        ),
        (
            RecordingStopReason::SizeLimit,
            "Recording stopped: size limit reached (512 MiB)",
        ),
    ];
    for (reason, message) in cases {
        let mut app = test_app();
        let terminal = TerminalId::next();
        app.update(started(terminal, "web-01.cast"));
        app.update(stopped(terminal, reason));
        assert!(app.recording.is_empty());
        let note = app.note.clone().unwrap();
        assert_eq!(note.message, message);
        assert!(!note.hint.is_empty());
        let lines = screen(&mut app, 120, 24);
        assert!(lines[23].contains(message), "{}", text(&lines));
        assert!(lines[23].contains(&note.hint[..20]), "{}", text(&lines));
        // It goes away on its own.
        app.expire(Instant::now() + RECORDING_NOTE_TTL);
        assert!(app.note.is_none());
    }
    for reason in [
        RecordingStopReason::Requested,
        RecordingStopReason::TerminalClosed,
    ] {
        let mut app = test_app();
        let terminal = TerminalId::next();
        app.update(started(terminal, "web-01.cast"));
        app.update(stopped(terminal, reason));
        assert!(app.note.is_none(), "{reason:?}");
    }
}

#[test]
fn a_lagged_bus_becomes_a_resync() {
    use tokio::sync::broadcast::error::RecvError;
    assert!(matches!(
        crate::app::engine_action(Err(RecvError::Lagged(12))),
        Some(Action::EngineLagged)
    ));
    assert!(crate::app::engine_action(Err(RecvError::Closed)).is_none());
    // A terminal the app believes recorded but that is gone: dropped.
    let mut app = test_app();
    app.update(started(TerminalId::next(), "gone.cast"));
    app.update(Action::EngineLagged);
    assert!(app.recording.is_empty());
}

#[test]
fn keys_open_the_panel_and_toggle_recording() {
    let keymap = Keymap::defaults();
    let cx = |focus, overlay, prefix| KeyContext {
        focus,
        overlay,
        dialog_open: false,
        prefix,
        pane_repeat: false,
        picking: false,
        keymap: &keymap,
    };
    let shift = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
    assert!(matches!(
        map_key(shift('R'), cx(Focus::Terminal, None, true)),
        Some(Action::ToggleRecording)
    ));
    // Without the prefix, `R` is the remote program's.
    assert!(matches!(
        map_key(shift('R'), cx(Focus::Terminal, None, false)),
        Some(Action::TerminalKey(_))
    ));
    assert!(matches!(
        map_key(key(KeyCode::Char('v')), cx(Focus::Terminal, None, true)),
        Some(Action::OpenRecordings)
    ));
    assert!(matches!(
        map_key(key(KeyCode::Char('v')), cx(Focus::Hosts, None, false)),
        Some(Action::OpenRecordings)
    ));
    // The panel and the player take every key (the prefix included):
    // nothing goes to a terminal through them.
    let ctrl_space = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
    for k in [key(KeyCode::Char('a')), key(KeyCode::Enter), ctrl_space] {
        assert!(matches!(
            map_key(k, cx(Focus::Terminal, Some(Overlay::Player), false)),
            Some(Action::PlayerKey(_))
        ));
        assert!(matches!(
            map_key(k, cx(Focus::Terminal, Some(Overlay::Recordings), false)),
            Some(Action::RecordingsKey(_))
        ));
    }
    // From the host list, there is no terminal to record.
    let mut app = test_app();
    app.update(Action::ToggleRecording);
    assert!(app.dialogs.is_empty());
    assert_eq!(app.note.as_ref().unwrap().message, "No terminal to record");
}

fn entry(name: &str, alias: &str, valid: bool) -> RecordingEntry {
    RecordingEntry {
        name: name.into(),
        alias: alias.into(),
        started: Some(time::macros::datetime!(2026-10-22 14:03 UTC)),
        size: 1_468_006,
        duration: Some(83.0),
        width: Some(80),
        height: Some(24),
        valid,
    }
}

#[tokio::test]
async fn the_panel_lists_marks_and_fits_80x24() {
    let mut app = test_app();
    app.update(Action::OpenRecordings);
    let lines = screen(&mut app, 80, 24);
    assert!(
        text(&lines).contains("Loading recordings…"),
        "{}",
        text(&lines)
    );
    app.update(Action::RecordingsLoaded(Ok(Vec::new())));
    let all = text(&screen(&mut app, 80, 24));
    assert!(all.contains("No recordings yet."), "{all}");
    assert!(all.contains("Ctrl-Space R records"), "{all}");

    let terminal = TerminalId::next();
    app.update(started(terminal, "web-01-20261022T140300Z.cast"));
    app.update(Action::RecordingsLoaded(Ok(vec![
        entry("web-01-20261022T140300Z.cast", "web-01", true),
        entry("db-20261021T090000Z.cast", "db-primary", true),
        entry("junk.cast", "junk", false),
    ])));
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains(" Recordings · 3 "), "{all}");
    // Being written: `●`; invalid: `▲`, no duration.
    let web = lines.iter().find(|l| l.contains("web-01 ")).unwrap();
    assert!(web.contains("▍● web-01"), "{all}");
    assert!(
        web.contains("22 Oct 14:03") && web.contains("1:23"),
        "{all}"
    );
    assert!(web.contains("1.4 MiB"), "{all}");
    let junk = lines.iter().find(|l| l.contains("junk ")).unwrap();
    assert!(junk.contains("▲ junk") && junk.contains("—"), "{all}");
    // The selected file's name, and the keys.
    assert!(all.contains("file  web-01-20261022T140300Z.cast"), "{all}");
    assert!(
        all.contains("↵  play") && all.contains("d  delete"),
        "{all}"
    );
    assert!(lines[23].contains("RECORDINGS"), "{all}");

    // The armed delete question.
    app.update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    let all = text(&screen(&mut app, 80, 24));
    assert!(
        all.contains("Delete web-01-20261022T140300Z.cast?"),
        "{all}"
    );
    assert!(
        all.contains("any key  keep") && all.contains("y  delete"),
        "{all}"
    );
    // A refusal from the engine shows its summary.
    app.update(Action::RecordingsKey(key(KeyCode::Esc)));
    app.update(Action::RecordingDeleted {
        name: "web-01-20261022T140300Z.cast".into(),
        result: Err(ErrorSummary {
            code: "recording_active".into(),
            message: "recording web-01-20261022T140300Z.cast is still being written".into(),
            hint: Some("Stop the recording first, then delete it.".into()),
        }),
    });
    let all = text(&screen(&mut app, 80, 24));
    assert!(
        all.contains("Stop the recording first, then delete it."),
        "{all}"
    );
}

// ── end to end ─────────────────────────────────────────────────────────────

struct Lab {
    env: Env,
    server: TestServer,
    events: broadcast::Receiver<EngineEvent>,
    app: App,
}

/// One host, `alpha`, an echoing SSH server; key authentication, host
/// key trusted.
async fn lab() -> Lab {
    let key = new_key();
    let server = TestServer::start(Behaviour {
        keys: vec![key.public_key().clone()],
        pty_echo: true,
        ..Behaviour::default()
    })
    .await;
    let env = Env::new(&target_config(
        "alpha",
        server.port,
        "  IdentityFile ~/.ssh/id_test\n",
    ));
    env.write_key("id_test", &key, None);
    env.trust(&server);
    let events = env.engine.subscribe();
    let mut app = App::new(env.engine.clone());
    app.clipboard = false;
    app.term_size = GridSize { cols: 80, rows: 22 };
    let inventory = env
        .engine
        .load_hosts()
        .await
        .map_err(|e| e.summary().message);
    app.update(Action::HostsLoaded(inventory));
    Lab {
        env,
        server,
        events,
        app,
    }
}

impl Lab {
    async fn until(&mut self, done: impl FnMut(&App) -> bool) {
        self.app.pump_until(&mut self.events, done).await;
    }

    async fn open_terminal(&mut self) -> TerminalId {
        self.app.update(Action::Connect);
        self.until(|app| {
            app.active_tab()
                .is_some_and(|t| t.items().iter().all(|(_, s)| s.live()))
        })
        .await;
        self.app.active_terminal().unwrap().id
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.app.update(Action::TerminalKey(key(KeyCode::Char(c))));
        }
    }

    fn received(&self) -> String {
        String::from_utf8_lossy(&self.server.behaviour.received.lock().unwrap()).into_owned()
    }

    fn recordings_dir(&self) -> std::path::PathBuf {
        self.env.engine.recordings_dir()
    }
}

/// Writes `bytes` as a recording file (private, like the engine's).
fn plant(dir: &Path, name: &str, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).unwrap();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

// One story, start to end (record, badge, stop, list, play): split up, the
// state each step relies on would be hidden.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn record_stop_list_and_play_back() {
    let mut lab = lab().await;
    let terminal = lab.open_terminal().await;

    // Ctrl-Space R: the confirmation, armed.
    lab.app.update(Action::ToggleRecording);
    let lines = screen(&mut lab.app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("Record this terminal?"), "{all}");
    // The shared wording, with the folder the file goes to.
    let folder = format!(
        "{}/",
        crate::ui::fmt::display_path(&lab.recordings_dir().display().to_string())
    );
    let Some(Dialog::Confirm(dialog)) = lab.app.dialogs.front() else {
        panic!("no confirmation")
    };
    assert_eq!(
        dialog.body,
        format!(
            "Everything shown on screen is recorded — including secrets printed by commands and anything the remote echoes. What you type is never recorded. Saved to {folder}, readable only by you."
        )
    );
    // Shown in full (word-wrapped, nothing cut).
    let shown: String = lines
        .iter()
        .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    for words in [
        "Everything shown on screen is recorded — including secrets",
        "What you type is never recorded.",
        "readable only by you.",
    ] {
        assert!(shown.contains(words), "{words}\n{all}");
    }
    assert!(
        all.contains("any key  cancel") && all.contains("y  record"),
        "{all}"
    );
    assert!(lines[23].contains("CONFIRM"), "{all}");
    // Keys typed for the shell right before are ignored, `y` included.
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    assert_eq!(lab.app.dialogs.len(), 1, "not armed yet");
    // Armed: anything but `y` cancels (`Y`, Enter, n…).
    for cancel in [KeyCode::Char('Y'), KeyCode::Enter, KeyCode::Char('n')] {
        arm_dialog(&mut lab.app);
        lab.app.update(Action::DialogKey(key(cancel)));
        assert!(lab.app.dialogs.is_empty(), "{cancel:?}");
        assert!(!lab.env.engine.is_recording(terminal));
        lab.app.update(Action::ToggleRecording);
    }
    arm_dialog(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    assert!(lab.app.dialogs.is_empty());
    // The badge waits for `Started`.
    assert!(!lab.app.is_recorded(terminal));
    lab.until(|app| app.is_recorded(terminal)).await;
    let lines = screen(&mut lab.app, 80, 24);
    // One pane: the badge is on its tab; the status bar says it too.
    assert!(lines[0].contains("1 alpha ● REC"), "{}", text(&lines));
    assert!(lines[23].contains("● REC"), "{}", text(&lines));
    assert!(lines[23].contains("Stop recording"), "{}", text(&lines));

    // Split: the recorded pane carries the badge in its title.
    lab.app
        .update(Action::SplitPane(crate::panes::Split::Vertical));
    lab.until(|app| {
        app.active_tab()
            .is_some_and(|t| t.len() == 2 && t.items().iter().all(|(_, s)| s.live()))
    })
    .await;
    let lines = screen(&mut lab.app, 80, 24);
    assert_eq!(lines[1].matches("REC").count(), 1, "{}", text(&lines));
    assert!(lines[1].contains("alpha  REC  ●"), "{}", text(&lines));
    assert!(!lines[0].contains("REC"), "{}", text(&lines));
    lab.app.update(Action::ClosePane);
    assert_eq!(lab.app.active_tab().map(Tab::len), Some(1));

    lab.typed("hello-record\r");
    lab.until(|app| {
        app.active_terminal().is_some_and(|v| {
            let term = v.term.lock().unwrap();
            grid_text(&term).contains("hello-record")
        })
    })
    .await;

    // The same key stops it, at once, without a question.
    lab.app.update(Action::ToggleRecording);
    assert!(lab.app.dialogs.is_empty());
    lab.until(|app| !app.is_recorded(terminal)).await;
    assert!(lab.app.note.is_none(), "a requested stop says nothing");

    // The list: the new file, then play it.
    lab.app.update(Action::OpenRecordings);
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.entries.ready().is_some_and(|e| e.len() == 1))
    })
    .await;
    let entry = lab
        .app
        .recordings_panel
        .as_ref()
        .unwrap()
        .selected()
        .cloned()
        .unwrap();
    assert_eq!(entry.alias, "alpha");
    assert!(entry.valid);
    assert_eq!(
        Path::new(&entry.name).extension().and_then(|e| e.to_str()),
        Some("cast")
    );
    let all = text(&screen(&mut lab.app, 80, 24));
    assert!(all.contains(&format!("file  {}", entry.name)), "{all}");

    lab.app.update(Action::RecordingsKey(key(KeyCode::Enter)));
    lab.until(|app| app.player.is_some()).await;
    assert_eq!(lab.app.overlay, Some(Overlay::Player));
    // Fast-forward: every silence skipped.
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Ended)
    })
    .await;
    let lines = screen(&mut lab.app, 80, 24);
    let all = text(&lines);
    // What the session showed once recording, not before.
    assert!(all.contains("hello-record"), "{all}");
    assert!(!all.contains("welcome"), "{all}");
    assert!(
        lines[1].contains("Playback") && lines[1].contains(&entry.name),
        "{all}"
    );
    assert!(lines[1].contains("■ Ended"), "{all}");
    assert!(lines[22].contains(" / "), "{all}");
    assert!(
        lines[23].contains("PLAYBACK") && lines[23].contains("back to list"),
        "{all}"
    );
    // `q`: back to the list.
    lab.app.update(Action::PlayerKey(key(KeyCode::Char('q'))));
    assert!(lab.app.player.is_none());
    assert_eq!(lab.app.overlay, Some(Overlay::Recordings));
}

#[tokio::test]
async fn deletion_is_refused_while_recording_and_invalid_files_go() {
    let mut lab = lab().await;
    let terminal = lab.open_terminal().await;
    lab.app.update(Action::ToggleRecording);
    arm_dialog(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    lab.until(|app| app.is_recorded(terminal)).await;
    plant(&lab.recordings_dir(), "junk.cast", b"not a header\n");

    lab.app.update(Action::OpenRecordings);
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.entries.ready().is_some_and(|e| e.len() == 2))
    })
    .await;
    // The active one first (newest): marked, and its deletion refused.
    let panel = lab.app.recordings_panel.as_ref().unwrap();
    let active = panel.selected().cloned().unwrap();
    assert!(active.valid && active.alias == "alpha", "{active:?}");
    lab.app
        .update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    arm_panel(&mut lab.app);
    lab.app
        .update(Action::RecordingsKey(key(KeyCode::Char('y'))));
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.notice.as_ref().is_some_and(|n| !n.ok))
    })
    .await;
    let notice = lab
        .app
        .recordings_panel
        .as_ref()
        .unwrap()
        .notice
        .clone()
        .unwrap();
    assert_eq!(
        notice.hint.as_deref(),
        Some("Stop the recording first, then delete it.")
    );
    assert!(lab.recordings_dir().join(&active.name).exists());

    // The invalid file: deletable.
    lab.app.update(Action::RecordingsKey(key(KeyCode::Down)));
    let junk = lab
        .app
        .recordings_panel
        .as_ref()
        .unwrap()
        .selected()
        .cloned()
        .unwrap();
    assert_eq!(junk.name, "junk.cast");
    assert!(!junk.valid);
    lab.app
        .update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    arm_panel(&mut lab.app);
    lab.app
        .update(Action::RecordingsKey(key(KeyCode::Char('y'))));
    lab.until(|app| {
        app.recordings_panel.as_ref().is_some_and(|p| {
            p.entries
                .ready()
                .is_some_and(|e| e.len() == 1 && e.iter().all(|x| x.name != "junk.cast"))
        })
    })
    .await;
    assert!(!lab.recordings_dir().join("junk.cast").exists());

    // Stopped, the recording can go too.
    lab.app.update(Action::CloseOverlay);
    lab.app.focus = Focus::Terminal;
    lab.app.update(Action::ToggleRecording);
    lab.until(|app| !app.is_recorded(terminal)).await;
    let name = active.name;
    let result = lab.env.engine.delete_recording(&name).await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn a_lagged_bus_resyncs_every_terminal() {
    let mut lab = lab().await;
    let terminal = lab.open_terminal().await;
    // Started while the app missed the events (a lagged receiver).
    lab.env.engine.start_recording(terminal).await.unwrap();
    while lab.events.try_recv().is_ok() {}
    assert!(!lab.app.is_recorded(terminal));
    lab.app.update(Action::EngineLagged);
    assert!(lab.app.is_recorded(terminal));
    assert!(screen(&mut lab.app, 80, 24)[23].contains("● REC"));

    lab.env.engine.stop_recording(terminal).unwrap();
    for _ in 0..500 {
        if !lab.env.engine.is_recording(terminal) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    while lab.events.try_recv().is_ok() {}
    lab.app.update(Action::EngineLagged);
    assert!(!lab.app.is_recorded(terminal));
}

#[tokio::test]
async fn a_hostile_recording_reaches_no_terminal() {
    let mut lab = lab().await;
    let terminal = lab.open_terminal().await;
    let live_mode = lab.app.active_terminal().unwrap().mode();
    // Queries a live terminal would answer, modes it would obey, a
    // clipboard write and a title: all in a third-party file.
    let bytes = crate::player::tests::cast(
        80,
        24,
        &[
            (0.1, "o", "shown\u{1b}[6n\u{1b}[c\u{1b}[5n\u{1b}[>c"),
            (
                0.2,
                "o",
                "\u{1b}[?1000h\u{1b}[?2004h\u{1b}[?1004h\u{1b}[?1049h\u{1b}[>1u",
            ),
            (0.3, "o", "\u{1b}]52;c;c2VjcmV0\u{7}\u{1b}]0;spoofed\u{7}"),
        ],
    );
    plant(&lab.recordings_dir(), "hostile.cast", &bytes);
    let before = lab.received();

    lab.app.update(Action::OpenRecordings);
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.entries.ready().is_some_and(|e| e.len() == 1))
    })
    .await;
    lab.app.update(Action::RecordingsKey(key(KeyCode::Enter)));
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Ended)
    })
    .await;
    let player = lab.app.player.as_ref().unwrap();
    // The emulator answered the queries; the answers were dropped.
    assert!(player.listener.dropped_replies.load(Ordering::Relaxed) >= 4);
    // Modes: in the player's emulator only.
    let set = TermMode::MOUSE_REPORT_CLICK
        | TermMode::BRACKETED_PASTE
        | TermMode::FOCUS_IN_OUT
        | TermMode::ALT_SCREEN;
    assert!(player.mode().contains(set), "{:?}", player.mode());
    let view_mode = lab.app.active_terminal().unwrap().mode();
    assert_eq!(view_mode, live_mode, "the live terminal is untouched");
    assert!(!view_mode.intersects(crate::player::HOST_AFFECTING_MODES));
    // The title row shows the file name next to the header title.
    let lines = screen(&mut lab.app, 80, 24);
    assert!(
        lines[1].contains("“web-01”") && lines[1].contains("hostile.cast"),
        "{}",
        text(&lines)
    );
    assert!(!text(&lines).contains("spoofed") && !text(&lines).contains("c2VjcmV0"));

    // Keys and pastes during playback go nowhere.
    lab.app.update(Action::PlayerKey(key(KeyCode::Char('a'))));
    lab.app.update(Action::Paste("pasted-in-player".into()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lab.received(), before, "nothing reached the live shell");
    assert!(!lab.env.engine.is_recording(terminal));
}

#[tokio::test]
async fn sessions_still_render_without_recording() {
    // A plain session: no badge anywhere.
    let mut app = test_app();
    app.sessions.insert(
        "web".into(),
        SessionView {
            id: SessionId::next(),
            phase: portico_core::events::SessionPhase::Connected,
            latency: None,
        },
    );
    assert!(!text(&screen(&mut app, 80, 24)).contains("REC"));
}
