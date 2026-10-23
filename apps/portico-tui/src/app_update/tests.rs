//! The update check in the TUI: the one-time question, its three answers
//! (through a real engine on a throwaway HOME, with a fetcher that counts
//! and never reaches the network), the badge and the update view at 80×24.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::update::{
    AvailableUpdate, InstallMethod, ReleaseFetcher, UpdateError, UpdateStatus,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::app::test_app;
use crate::dialog::{ARM_DELAY, ConfirmOutcome};

const URL: &str = "https://github.com/fs0ciety7000/Portico/releases/tag/v0.3.1";

fn screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn status(check: Option<bool>, available: bool) -> UpdateStatus {
    UpdateStatus {
        check,
        current: "0.3.0".into(),
        available: available.then(|| AvailableUpdate {
            version: "0.3.1".into(),
            url: URL.into(),
        }),
        checked_at: None,
    }
}

/// The front dialog, armed long enough ago to take keys.
fn front_confirm(app: &mut App) -> &mut ConfirmDialog {
    match app.dialogs.front_mut() {
        Some(Dialog::Confirm(c)) => {
            if c.armed.is_some() {
                c.armed = Instant::now().checked_sub(ARM_DELAY * 2);
            }
            c
        }
        other => panic!("expected a confirmation, got {other:?}"),
    }
}

#[test]
fn the_question_is_asked_once_and_only_when_not_answered() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(None, false))));
    assert_eq!(app.dialogs.len(), 1);
    assert_eq!(
        front_confirm(&mut app).action,
        ConfirmAction::UpdateQuestion
    );
    // Another status during the run (event, resync): not asked again.
    app.update(Action::UpdateLoaded(Ok(status(None, false))));
    assert_eq!(app.dialogs.len(), 1);

    for answered in [Some(false), Some(true)] {
        let mut app = test_app();
        app.update(Action::UpdateLoaded(Ok(status(answered, false))));
        assert!(app.dialogs.is_empty(), "{answered:?}");
    }
    // settings.toml unreadable: nothing asked (the answer could not be saved).
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Err(ErrorSummary {
        code: "serde".into(),
        message: "bad".into(),
        hint: None,
    })));
    assert!(app.dialogs.is_empty());
}

#[test]
fn the_question_takes_y_n_and_esc_once_armed() {
    let mut dialog = question_dialog();
    // Typed just before it showed: ignored.
    assert_eq!(dialog.handle_key(key('n')), ConfirmOutcome::None);
    dialog.armed = Instant::now().checked_sub(ARM_DELAY * 2);
    assert_eq!(dialog.handle_key(key('y')), ConfirmOutcome::Confirm);
    assert_eq!(dialog.handle_key(key('n')), ConfirmOutcome::Decline);
    assert_eq!(
        dialog.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ConfirmOutcome::Cancel
    );
    assert_eq!(dialog.handle_key(key('j')), ConfirmOutcome::None);
}

#[test]
fn the_question_at_80x24() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(None, false))));
    front_confirm(&mut app);
    let text = screen(&mut app);
    for needle in [
        "Updates",
        "Check for new versions once a day?",
        "github.com and send nothing else.",
        "Esc  later",
        "n  not now",
        "y  check daily",
        "Change it later: U on the host list.",
    ] {
        assert!(text.contains(needle), "{needle:?} missing:\n{text}");
    }
    // Compact (56 columns) and quiet (no accent border), a blank line
    // before the muted note.
    let lines: Vec<&str> = text.lines().collect();
    let top = lines.iter().find(|l| l.contains("╭ Updates ")).unwrap();
    let start = top.find('╭').unwrap();
    let end = top.find('╮').unwrap();
    assert_eq!(
        top[start..end + '╮'.len_utf8()].chars().count(),
        56,
        "{top:?}"
    );
    let note = lines
        .iter()
        .position(|l| l.contains("Change it later"))
        .unwrap();
    assert!(
        lines[note - 1]
            .trim_matches(|c| c == '│' || c == ' ')
            .is_empty()
    );
}

#[test]
fn the_question_border_is_subtle_and_its_note_muted() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(None, false))));
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::render(&mut app, f)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let s = app.styles();
    let corner = buffer
        .content()
        .iter()
        .find(|c| c.symbol() == "╭" && c.bg == s.overlay().bg.unwrap())
        .unwrap();
    assert_eq!(corner.fg, s.border().fg.unwrap());
    let row = (0..24)
        .find(|y| {
            (0..80)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("Change it later")
        })
        .unwrap();
    let line: String = (0..80).map(|x| buffer[(x, row)].symbol()).collect();
    let x = u16::try_from(line.find("Change").unwrap()).unwrap();
    assert_eq!(buffer[(x, row)].fg, s.muted().fg.unwrap());
}

#[test]
fn the_badge_shows_in_the_status_bar_at_80x24() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(Some(true), false))));
    assert!(!screen(&mut app).contains("available"));
    app.update(Action::Engine(portico_core::EngineEvent::Update {
        status: status(Some(true), true),
    }));
    let text = screen(&mut app);
    let bar = text.lines().last().unwrap();
    assert!(bar.trim_end().ends_with("0.3.1 available"), "{bar:?}");
    assert!(bar.contains("quit"), "quit keeps its room: {bar:?}");
}

#[test]
fn the_update_view_gives_the_page_and_the_command() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(Some(true), true))));
    app.update(Action::UpdateInstruction {
        version: "0.3.1".into(),
        instruction: portico_core::update::instruction(
            InstallMethod::Homebrew,
            Frontend::Tui,
            "0.3.1",
        ),
    });
    app.update(Action::ShowUpdate);
    let dialog = front_confirm(&mut app);
    assert_eq!(
        dialog.action,
        ConfirmAction::CopyUpdate("brew upgrade fs0ciety7000/portico/portico".into())
    );
    let text = screen(&mut app);
    for needle in [
        "Portico 0.3.1 is available",
        "You have 0.3.0.",
        URL,
        "Update with Homebrew:",
        "brew upgrade fs0ciety7000/portico/portico",
        "Esc  close",
        "y  copy command",
        "Portico never downloads or installs anything itself.",
    ] {
        assert!(text.contains(needle), "{needle:?} missing:\n{text}");
    }
    // `y` copies (OSC 52 is off in tests) and closes.
    app.update(Action::DialogKey(key('y')));
    assert!(app.dialogs.is_empty());
    assert_eq!(
        app.toast.as_ref().map(|t| t.message.as_str()),
        Some("Update command copied")
    );

    // Without a command (AppImage, deb…): the link is what `y` copies.
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(Some(true), true))));
    app.update(Action::ShowUpdate);
    assert_eq!(
        front_confirm(&mut app).action,
        ConfirmAction::CopyUpdate(URL.into())
    );
    assert!(screen(&mut app).contains("Download it from the release page."));
}

#[test]
fn without_a_release_the_view_shows_the_setting() {
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(Some(false), false))));
    app.update(Action::ShowUpdate);
    assert_eq!(
        front_confirm(&mut app).action,
        ConfirmAction::SetUpdateCheck(true)
    );
    let text = screen(&mut app);
    assert!(text.contains("Daily update check: off."), "{text}");
    assert!(text.contains("y  turn on"), "{text}");

    let now = time::OffsetDateTime::now_utc();
    let mut on = status(Some(true), false);
    on.checked_at = Some(now - time::Duration::hours(3));
    let state = UpdateState {
        status: Some(on),
        ..UpdateState::default()
    };
    let dialog = update_dialog(&state, now).unwrap();
    assert!(
        dialog.body.contains("on (last checked 3 h ago)"),
        "{}",
        dialog.body
    );
    assert_eq!(dialog.action, ConfirmAction::SetUpdateCheck(false));
}

#[test]
fn update_keys_are_bound_on_the_host_list() {
    use crate::keymap::{BINDINGS, Context};
    let find = |id: &str| BINDINGS.iter().find(|b| b.id == id).unwrap();
    for (id, keys) in [("hosts.update", ["u"]), ("hosts.update_check", ["U"])] {
        let binding = find(id);
        assert_eq!(binding.context, Context::Hosts);
        assert_eq!(binding.keys, keys);
    }
}

#[test]
fn shift_u_opens_the_setting_and_never_turns_it_on_by_itself() {
    // Off, with a release still known from an earlier check: `U` shows
    // the setting (not the release), and nothing is saved until `y`.
    let mut app = test_app();
    app.update(Action::UpdateLoaded(Ok(status(Some(false), true))));
    app.update(Action::ShowUpdateSettings);
    let dialog = front_confirm(&mut app);
    assert_eq!(dialog.action, ConfirmAction::SetUpdateCheck(true));
    assert_eq!(dialog.title, "Updates");
    assert_eq!(
        app.update.status.as_ref().and_then(|s| s.check),
        Some(false)
    );
    let text = screen(&mut app);
    assert!(text.contains("Daily update check: off."), "{text}");
    assert!(text.contains("y  turn on"), "{text}");
    // Esc closes it, the setting untouched.
    app.update(Action::DialogKey(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    assert!(app.dialogs.is_empty());
    assert_eq!(
        app.update.status.as_ref().and_then(|s| s.check),
        Some(false)
    );
}

#[test]
fn ago_reads_naturally() {
    let now = time::OffsetDateTime::now_utc();
    let h = time::Duration::hours;
    assert_eq!(ago(now, now), "just now");
    assert_eq!(ago(now - time::Duration::minutes(5), now), "5 min ago");
    assert_eq!(ago(now - h(3), now), "3 h ago");
    assert_eq!(ago(now - h(72), now), "3 days ago");
    assert_eq!(ago(now + h(1), now), "just now");
}

/// Counts calls; never reaches the network.
#[derive(Debug, Default)]
struct Counting(AtomicUsize);

impl ReleaseFetcher for Counting {
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(UpdateError::Offline)
    }
}

fn engine_app(root: &std::path::Path, fetcher: Arc<Counting>) -> App {
    let engine = portico_core::Engine::new(portico_core::EngineConfig {
        paths: portico_core::store::Paths::rooted(root),
        local_user: "alice".into(),
        event_capacity: 64,
        secrets: Arc::new(portico_core::secrets::MemoryStore::default()),
        agent_socket: None,
        connect_timeout: Duration::from_secs(1),
        keepalive: Duration::from_secs(1),
        update_fetcher: fetcher,
    });
    let mut app = App::new(engine);
    app.clipboard = false;
    app
}

#[tokio::test]
async fn not_now_is_written_and_never_reaches_the_network() {
    let dir = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Counting::default());
    let mut app = engine_app(dir.path(), fetcher.clone());
    let mut events = app.engine.subscribe();
    app.start_update_checks();
    app.pump_until(&mut events, |a| !a.dialogs.is_empty()).await;
    front_confirm(&mut app);
    app.update(Action::DialogKey(key('n')));
    app.pump_until(&mut events, |a| {
        a.update
            .status
            .as_ref()
            .is_some_and(|s| s.check == Some(false))
    })
    .await;
    let settings = std::fs::read_to_string(dir.path().join("config/settings.toml")).unwrap();
    assert!(settings.contains("[updates]\ncheck = false"), "{settings}");
    assert!(!app.engine.update_checks_running());

    // Next launch: not asked again, still no request.
    let mut app = engine_app(dir.path(), fetcher.clone());
    let mut events = app.engine.subscribe();
    app.start_update_checks();
    app.pump_until(&mut events, |a| a.update.status.is_some())
        .await;
    assert!(app.dialogs.is_empty());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fetcher.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn later_asks_again_and_check_daily_turns_it_on() {
    let dir = tempfile::tempdir().unwrap();
    let fetcher = Arc::new(Counting::default());
    let mut app = engine_app(dir.path(), fetcher.clone());
    let mut events = app.engine.subscribe();
    app.start_update_checks();
    app.pump_until(&mut events, |a| !a.dialogs.is_empty()).await;
    front_confirm(&mut app);
    app.update(Action::DialogKey(KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    )));
    assert!(app.dialogs.is_empty());
    assert!(!dir.path().join("config/settings.toml").exists());
    assert_eq!(fetcher.0.load(Ordering::SeqCst), 0);

    let mut app = engine_app(dir.path(), fetcher.clone());
    let mut events = app.engine.subscribe();
    app.start_update_checks();
    app.pump_until(&mut events, |a| !a.dialogs.is_empty()).await;
    front_confirm(&mut app);
    app.update(Action::DialogKey(key('y')));
    app.pump_until(&mut events, |a| {
        a.update
            .status
            .as_ref()
            .is_some_and(|s| s.check == Some(true))
    })
    .await;
    assert!(app.engine.update_checks_running());
    // The first check runs at once (it fails here: no network in tests).
    let deadline = Instant::now() + Duration::from_secs(10);
    while fetcher.0.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fetcher.0.load(Ordering::SeqCst), 1);

    // `U` opens the setting, `y` there turns it off again.
    app.update(Action::ShowUpdateSettings);
    assert_eq!(
        front_confirm(&mut app).action,
        ConfirmAction::SetUpdateCheck(false)
    );
    app.update(Action::DialogKey(key('y')));
    app.pump_until(&mut events, |a| {
        a.update
            .status
            .as_ref()
            .is_some_and(|s| s.check == Some(false))
    })
    .await;
    assert!(!app.engine.update_checks_running());
    app.engine.shutdown();
}
