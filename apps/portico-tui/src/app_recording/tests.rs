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
use crate::app_panes::NoteTone;
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
    // A requested stop says where the file went: neutral, 4 s.
    let note = app.note.clone().unwrap();
    assert_eq!(note.message, "Recording saved · web-01.cast");
    assert_eq!(note.hint, "Ctrl-Space v to open Recordings");
    assert_eq!(note.tone, NoteTone::Neutral);
    app.expire(Instant::now() + SAVED_NOTE_TTL);
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
        assert_eq!(note.tone, NoteTone::Warning);
        assert!(!note.hint.is_empty());
        let lines = screen(&mut app, 120, 24);
        assert!(lines[23].contains(message), "{}", text(&lines));
        assert!(lines[23].contains(&note.hint[..20]), "{}", text(&lines));
        // It goes away on its own.
        app.expire(Instant::now() + RECORDING_NOTE_TTL);
        assert!(app.note.is_none());
    }
    // A closed terminal says nothing (its pane says it).
    let mut app = test_app();
    let terminal = TerminalId::next();
    app.update(started(terminal, "web-01.cast"));
    app.update(stopped(terminal, RecordingStopReason::TerminalClosed));
    assert!(app.note.is_none());
}

#[tokio::test]
async fn a_requested_stop_is_a_neutral_notice() {
    let mut app = test_app();
    let terminal = TerminalId::next();
    app.update(started(terminal, "web-01.cast"));
    app.update(stopped(terminal, RecordingStopReason::Requested));
    let buf = {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();
        terminal.backend().buffer().clone()
    };
    let lines = screen(&mut app, 80, 24);
    let bar = &lines[23];
    assert!(
        bar.contains("Recording saved · web-01.cast · Ctrl-Space v to open Recordings"),
        "{}",
        text(&lines)
    );
    assert!(!bar.contains('▲'), "no warning mark");
    // No status colour: secondary text.
    let s = app.styles();
    let x = u16::try_from(bar.find("Recording").unwrap()).unwrap();
    let cell = buf.cell((x, 23)).unwrap();
    assert_eq!(cell.fg, crate::theme::c(s.theme.palette.text_secondary));
    for status in [s.theme.status.warning, s.theme.status.error] {
        assert_ne!(cell.fg, crate::theme::c(status));
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
        searching: false,
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
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("No recordings yet."), "{all}");
    assert!(all.contains("Ctrl-Space R records"), "{all}");
    // No count when there is nothing, no key line inside the panel (the
    // status bar has them).
    assert!(lines[1].contains("╭ Recordings ─"), "{all}");
    assert!(!lines[1].contains('·'), "{all}");
    assert!(!text(&lines[1..23]).contains("Esc close"), "{all}");
    assert!(lines[23].contains("Esc  close"), "{all}");
    // At 80 columns the panel takes the whole width: nothing of what is
    // behind shows in a margin.
    for row in &lines[1..23] {
        let first = row.chars().next().unwrap();
        let last = row.chars().last().unwrap();
        assert!("╭│╰".contains(first), "{all}");
        assert!("╮│╯".contains(last), "{all}");
    }

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
    // The local offset is unknown in tests: UTC, and it says so.
    assert!(
        web.contains("22 Oct 14:03 UTC") && web.contains("1:23"),
        "{all}"
    );
    assert!(web.contains("1.4 MiB"), "{all}");
    let junk = lines.iter().find(|l| l.contains("junk ")).unwrap();
    assert!(junk.contains("▲ junk") && junk.contains("—"), "{all}");
    // The selected file's name; the keys in the status bar. The file being
    // written cannot be deleted: its delete key is not offered.
    assert!(all.contains("file  web-01-20261022T140300Z.cast"), "{all}");
    assert!(lines[23].contains("RECORDINGS"), "{all}");
    assert!(lines[23].contains("↵  play"), "{all}");
    assert!(!lines[23].contains("d  delete"), "{all}");
    app.update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(
        all.contains("▲ This recording is still being written"),
        "{all}"
    );
    assert!(all.contains("Stop the recording first"), "{all}");
    assert!(!all.contains("Delete web-01"), "no question: {all}");

    // The armed delete question, on a file that is not being written.
    app.update(Action::RecordingsKey(key(KeyCode::Down)));
    assert!(screen(&mut app, 80, 24)[23].contains("d  delete"));
    app.update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    let lines = screen(&mut app, 80, 24);
    let all = text(&lines);
    assert!(all.contains("Delete db-20261021T090000Z.cast?"), "{all}");
    assert!(
        all.contains("The file is removed from the recordings folder. This cannot be undone."),
        "{all}"
    );
    assert!(
        lines[23].contains("n / Esc  keep") && lines[23].contains("y  delete"),
        "{all}"
    );
    // Not armed yet: Esc is ignored too; armed, it keeps the file.
    app.update(Action::RecordingsKey(key(KeyCode::Esc)));
    assert!(app.recordings_panel.as_ref().unwrap().confirm.is_some());
    arm_panel(&mut app);
    app.update(Action::RecordingsKey(key(KeyCode::Esc)));
    assert!(app.recordings_panel.as_ref().unwrap().confirm.is_none());
    // An invalid file, selected: a warning (not an error), and why.
    app.update(Action::RecordingsKey(key(KeyCode::Down)));
    let all = text(&screen(&mut app, 80, 24));
    assert!(
        all.contains("Not a valid asciicast v2 recording: it cannot be played,"),
        "{all}"
    );
    // A refusal from the engine shows its summary.
    app.update(Action::RecordingsKey(key(KeyCode::Up)));
    app.update(Action::RecordingsKey(key(KeyCode::Up)));
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
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
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
    // Shortened like the dialog does (long temp dirs, e.g. macOS
    // `/var/folders/…`, get a middle ellipsis).
    let folder = super::folder_label(&lab.recordings_dir(), super::FOLDER_WIDTH);
    let Some(Dialog::Confirm(dialog)) = lab.app.dialogs.front() else {
        panic!("no confirmation")
    };
    assert_eq!(
        dialog.body,
        format!(
            "Everything the terminal shows is recorded, including the commands you type (the shell echoes them) and any secret a command prints. Keystrokes themselves are not stored, so a password typed at a prompt that does not echo stays out. Saved to {folder}, readable only by you."
        )
    );
    // One pane: no subject line.
    assert_eq!(dialog.subject, None);
    // Shown in full (word-wrapped, nothing cut).
    let shown: String = lines
        .iter()
        .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    for words in [
        "Everything the terminal shows is recorded, including the commands you type",
        "Keystrokes themselves are not stored,",
        "readable only by you.",
    ] {
        assert!(shown.contains(words), "{words}\n{all}");
    }
    assert!(
        all.contains("n / Esc  cancel") && all.contains("y  record"),
        "{all}"
    );
    assert!(lines[23].contains("CONFIRM"), "{all}");
    // Keys typed for the shell right before are ignored, `y` included.
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    assert_eq!(lab.app.dialogs.len(), 1, "not armed yet");
    // Armed: other keys are not an answer (`Y`, Enter, a letter)…
    arm_dialog(&mut lab.app);
    for ignored in [KeyCode::Char('Y'), KeyCode::Enter, KeyCode::Char('l')] {
        lab.app.update(Action::DialogKey(key(ignored)));
        assert_eq!(lab.app.dialogs.len(), 1, "{ignored:?}");
    }
    // …`n` and `Esc` cancel.
    for cancel in [KeyCode::Char('n'), KeyCode::Esc] {
        lab.app.update(Action::DialogKey(key(cancel)));
        assert!(lab.app.dialogs.is_empty(), "{cancel:?}");
        assert!(!lab.env.engine.is_recording(terminal));
        lab.app.update(Action::ToggleRecording);
        arm_dialog(&mut lab.app);
    }
    arm_dialog(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    assert!(lab.app.dialogs.is_empty());
    // The badge waits for `Started`.
    assert!(!lab.app.is_recorded(terminal));
    lab.until(|app| app.is_recorded(terminal)).await;
    let lines = screen(&mut lab.app, 80, 24);
    // One pane: the badge is on its tab, apart from the connection dot;
    // the status bar says it too, with how to stop (lower case).
    assert!(lines[0].contains("1 alpha ● · REC"), "{}", text(&lines));
    assert!(lines[23].contains("● REC"), "{}", text(&lines));
    assert!(
        lines[23].contains("Ctrl-Space R  stop recording"),
        "{}",
        text(&lines)
    );

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
    // The tab says it too, whatever pane has the focus.
    assert!(lines[0].contains("(2) ● · REC"), "{}", text(&lines));
    // Recording the other pane: the question names it.
    lab.app.update(Action::ToggleRecording);
    let Some(Dialog::Confirm(dialog)) = lab.app.dialogs.front() else {
        panic!("no confirmation")
    };
    assert_eq!(dialog.title, "Record this pane?");
    assert_eq!(dialog.subject.as_deref(), Some("alpha · pane 2/2"));
    let all = text(&screen(&mut lab.app, 80, 24));
    assert!(all.contains("Record this pane?"), "{all}");
    assert!(all.contains("alpha · pane 2/2"), "{all}");
    lab.app.update(Action::DialogKey(key(KeyCode::Esc)));
    arm_dialog(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Esc)));
    assert!(lab.app.dialogs.is_empty());
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
    let note = lab.app.note.clone().unwrap();
    assert!(
        note.message.starts_with("Recording saved · alpha-"),
        "{note:?}"
    );
    assert_eq!(note.tone, NoteTone::Neutral);
    // It goes after 4 s (it holds the hints' place meanwhile).
    lab.app.expire(Instant::now() + SAVED_NOTE_TTL);
    assert!(lab.app.note.is_none());

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
    // 80×24: one row of chrome, at the bottom: the file name, the time,
    // the state and the speed (the grid gets the title row).
    assert!(lines[22].contains(&entry.name[..12]), "{all}");
    assert!(
        lines[22].contains(" / ") && lines[22].contains("■ Ended"),
        "{all}"
    );
    assert!(!all.contains("cropped"), "{all}");
    assert!(
        lines[23].contains("PLAYBACK")
            && lines[23].contains("Space  replay")
            && lines[23].contains("q  back to list"),
        "{all}"
    );
    // The status of the session behind stays visible.
    assert!(lines[23].contains("● alpha"), "{all}");
    // Taller: a title row with the name, the state and the speed.
    let lines = screen(&mut lab.app, 120, 32);
    let all = text(&lines);
    assert!(
        lines[1].contains("Playback") && lines[1].contains(&entry.name),
        "{all}"
    );
    assert!(lines[1].contains("■ Ended"), "{all}");

    // Space replays from the start (the file reopens).
    lab.app.update(Action::PlayerKey(key(KeyCode::Char(' '))));
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() != PlayerState::Ended)
    })
    .await;
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Ended)
    })
    .await;
    assert!(text(&screen(&mut lab.app, 80, 24)).contains("hello-record"));
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
    // Refused up front: no question that would only fail.
    lab.app
        .update(Action::RecordingsKey(key(KeyCode::Char('d'))));
    let panel = lab.app.recordings_panel.as_ref().unwrap();
    assert!(panel.confirm.is_none());
    assert_eq!(panel.refused, Some(crate::recordings::STILL_WRITTEN));
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
    let lines = screen(&mut lab.app, 100, 30);
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

/// Every session of the lab measured at 12 ms.
fn measured(app: &mut App) {
    for view in app.sessions.values_mut() {
        view.latency = Some(Duration::from_millis(12));
    }
}

#[tokio::test]
async fn the_status_bar_keeps_status_and_rec_at_80_columns() {
    let mut lab = lab().await;
    let terminal = lab.open_terminal().await;
    lab.app.update(started(terminal, "alpha.cast"));
    measured(&mut lab.app);
    // Recorded + connected, 80 columns: status, latency and REC stay; the
    // stop key comes first among the hints.
    let lines = screen(&mut lab.app, 80, 24);
    let bar = &lines[23];
    assert!(bar.starts_with(" TERMINAL "), "{bar}");
    assert!(bar.contains("Ctrl-Space R  stop recording"), "{bar}");
    assert!(bar.contains("● alpha · 12 ms"), "{bar}");
    assert!(bar.ends_with("● REC "), "{bar}");

    // A long alias: shortened (12) before anything else goes; the
    // latency and REC stay.
    if let Some(view) = lab
        .app
        .tabs
        .get_mut(0)
        .and_then(Tab::focused_mut)
        .and_then(PaneSlot::view_mut)
    {
        view.alias = "production-eu-west-3-postgres-primary".into();
    }
    let lines = screen(&mut lab.app, 80, 24);
    let bar = &lines[23];
    assert!(bar.contains("● production-… · 12 ms"), "{bar}");
    assert!(bar.ends_with("● REC "), "{bar}");
    assert!(bar.contains("stop recording"), "{bar}");
    assert_eq!(bar.chars().count(), 80);

    // The player, playing and paused: the session's status stays visible,
    // and so does the way back.
    let bytes =
        crate::player::tests::cast(80, 24, &[(0.1, "o", "first"), (100.0, "o", "much later")]);
    plant(&lab.recordings_dir(), "long.cast", &bytes);
    lab.app.update(Action::OpenRecordings);
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.entries.ready().is_some_and(|e| !e.is_empty()))
    })
    .await;
    lab.app.update(Action::RecordingsKey(key(KeyCode::Enter)));
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Playing)
    })
    .await;
    // The engine's own keepalive may have reported a real loopback RTT
    // (`<1 ms`) while the events were pumped (timing-dependent: seen on
    // Windows): pin the latency again so the assertion stays about layout.
    measured(&mut lab.app);
    for state in ["▶ Playing", "❚❚ Paused"] {
        let lines = screen(&mut lab.app, 80, 24);
        let all = text(&lines);
        assert!(lines[22].contains(state), "{all}");
        let bar = &lines[23];
        assert!(bar.contains("PLAYBACK"), "{bar}");
        assert!(bar.contains("● production-… · 12 ms"), "{bar}");
        assert!(bar.contains("q  back to list"), "{bar}");
        assert!(bar.ends_with("● REC "), "{bar}");
        lab.app.update(Action::PlayerKey(key(KeyCode::Char(' '))));
    }
}

#[tokio::test]
async fn the_player_seeks_back_and_restarts() {
    let mut lab = lab().await;
    let bytes = crate::player::tests::cast(
        40,
        5,
        &[
            (0.1, "o", "one "),
            (0.2, "o", "two "),
            (30.0, "o", "three "),
            (60.0, "o", "four"),
        ],
    );
    plant(&lab.recordings_dir(), "seek.cast", &bytes);
    lab.app.update(Action::OpenRecordings);
    lab.until(|app| {
        app.recordings_panel
            .as_ref()
            .is_some_and(|p| p.entries.ready().is_some_and(|e| !e.is_empty()))
    })
    .await;
    lab.app.update(Action::RecordingsKey(key(KeyCode::Enter)));
    // To the end (each silence is capped at 2 s).
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Ended)
    })
    .await;
    let shown = |app: &App| {
        app.player
            .as_ref()
            .map(|p| crate::player::tests::rows(p).join("\n"))
            .unwrap_or_default()
    };
    assert!(shown(&lab.app).contains("one two three four"));
    assert!(screen(&mut lab.app, 80, 24)[23].contains("Space  replay"));

    // `←`: 5 s back (55 s): the screen as it was then, at once.
    lab.app.update(Action::PlayerKey(key(KeyCode::Left)));
    lab.until(|app| {
        app.player
            .as_ref()
            .is_some_and(|p| p.state() != PlayerState::Ended && p.position >= 30.0)
    })
    .await;
    assert_eq!(shown(&lab.app).trim_end(), "one two three");
    // `Home`: from the start.
    lab.app.update(Action::PlayerKey(key(KeyCode::Home)));
    lab.until(|app| app.player.as_ref().is_some_and(|p| p.position < 1.0))
        .await;
    assert!(!shown(&lab.app).contains("three"));
    // Keys never reach a shell, and `Esc` closes like `q`.
    lab.app.update(Action::PlayerKey(key(KeyCode::Esc)));
    assert!(lab.app.player.is_none());
    assert_eq!(lab.app.overlay, Some(Overlay::Recordings));
}

#[test]
fn the_record_key_turns_accent_once_armed_with_one_wake_up() {
    let mut app = test_app();
    let opened = Instant::now();
    app.dialogs.push_back(Dialog::Confirm(ConfirmDialog {
        title: CONFIRM_TITLE.into(),
        subject: None,
        body: confirm_body("~/r/"),
        confirm: "record",
        action: ConfirmAction::StartRecording(TerminalId::next()),
        armed: Some(opened),
        answers: crate::dialog::Answers::YesNo,
        note: None,
    }));
    // One wake-up, at the instant `y` starts to count (no ticking).
    assert_eq!(app.next_deadline(), Some(opened + ARM_DELAY));
    let accent = crate::theme::c(app.styles().theme.palette.accent);
    let y_cell = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui::render(app, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = screen(app, 80, 24);
        let (y, line) = lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(" y  record"))
            .unwrap();
        let byte = line.find(" y  record").unwrap();
        let x = line[..byte].chars().count() + 1;
        buffer
            .cell((u16::try_from(x).unwrap(), u16::try_from(y).unwrap()))
            .unwrap()
            .clone()
    };
    // Not armed: muted, like any other key.
    let cell = y_cell(&mut app);
    assert_eq!(cell.symbol(), "y");
    assert_ne!(cell.bg, accent);
    // Armed: the primary key, in the accent; nothing more to wake for.
    arm_dialog(&mut app);
    app.expire(Instant::now());
    let cell = y_cell(&mut app);
    assert_eq!(cell.bg, accent);
    assert_eq!(app.next_deadline(), None);
}
