//! The player on in-memory recordings: timing (pause, speed, idle skip),
//! the dropped replies, the one sanitiser and the confined modes.

use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};

use super::*;

/// An asciicast v2 file: header `cols`×`rows`, then `(time, type, data)`.
pub(crate) fn cast(cols: u16, rows: u16, events: &[(f64, &str, &str)]) -> Vec<u8> {
    let mut out = serde_json::json!({
        "version": 2,
        "width": cols,
        "height": rows,
        "timestamp": 1_700_000_000,
        "title": "web-01",
    })
    .to_string();
    out.push('\n');
    for (time, kind, data) in events {
        out.push_str(&serde_json::json!([time, kind, data]).to_string());
        out.push('\n');
    }
    out.into_bytes()
}

pub(crate) fn player_of(bytes: Vec<u8>, total: Option<f64>) -> Player {
    let reader = CastReader::new(Cursor::new(bytes), "test.cast").unwrap();
    Player::new("test.cast".into(), total, spawn_reader(reader).unwrap())
}

/// Shows what is due at `now`, waiting for the reader when it has more
/// and nothing waits for its time.
async fn settle(player: &mut Player, now: Instant) {
    loop {
        player.step(now);
        if !player.wants_input() {
            return;
        }
        let msg = player.recv().await;
        player.feed(msg, now);
    }
}

/// Plays everything at once.
pub(crate) async fn play_all(player: &mut Player) {
    let far = Instant::now() + Duration::from_secs(3600);
    while player.state() != PlayerState::Ended {
        settle(player, far).await;
        player.skip_idle(far);
    }
}

/// The visible text of the playback grid, one string per row.
pub(crate) fn rows(player: &Player) -> Vec<String> {
    let term = player.term();
    let grid = term.grid();
    (0..term.screen_lines())
        .map(|y| {
            let line = Line(i32::try_from(y).unwrap());
            (0..term.columns())
                .map(|x| grid[line][Column(x)].c)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn pause_speed_and_idle_skip_follow_the_clock() {
    let bytes = cast(
        20,
        5,
        &[
            (0.5, "o", "a"),
            (1.5, "o", "b"),
            // Ten seconds of silence: capped at 2 s.
            (11.5, "o", "c"),
            (12.0, "o", "d"),
        ],
    );
    let mut player = player_of(bytes, None);
    assert_eq!(player.state(), PlayerState::Loading);
    let t0 = Instant::now();
    settle(&mut player, t0).await;
    assert_eq!(player.next_due(), Some(t0 + Duration::from_millis(500)));
    // Not yet.
    player.step(t0 + Duration::from_millis(400));
    assert_eq!(rows(&player)[0], "");
    // Due: shown, and the next one waits a second.
    let t1 = t0 + Duration::from_millis(500);
    settle(&mut player, t1).await;
    assert_eq!(rows(&player)[0], "a");
    assert_eq!(player.state(), PlayerState::Playing);
    assert_eq!(player.next_due(), Some(t1 + Duration::from_secs(1)));

    // Paused: no deadline (so no wake-up, no redraw), nothing shows.
    player.toggle_pause(t1 + Duration::from_millis(200));
    assert_eq!(player.state(), PlayerState::Paused);
    assert_eq!(player.next_due(), None);
    player.step(t1 + Duration::from_secs(30));
    assert_eq!(rows(&player)[0], "a");
    // Resumed: what was left to wait (0.8 s) from now.
    let t2 = t1 + Duration::from_secs(30);
    player.toggle_pause(t2);
    assert_eq!(player.next_due(), Some(t2 + Duration::from_millis(800)));

    // ×2: the wait in progress is halved too.
    player.change_speed(true, t2);
    assert!((player.speed() - 2.0).abs() < f64::EPSILON);
    assert_eq!(player.next_due(), Some(t2 + Duration::from_millis(400)));
    let t3 = t2 + Duration::from_millis(400);
    settle(&mut player, t3).await;
    assert_eq!(rows(&player)[0], "ab");
    // The 10 s silence: capped at 2 s, at ×2 one second.
    assert_eq!(player.next_due(), Some(t3 + Duration::from_secs(1)));
    // `→` ends it now.
    player.skip_idle(t3);
    settle(&mut player, t3).await;
    assert_eq!(rows(&player)[0], "abc");
    assert!((player.position - 11.5).abs() < 1e-9);
    // Back to ×1: 0.5 s left (0.25 s at ×2, doubled).
    player.change_speed(false, t3);
    assert_eq!(player.next_due(), Some(t3 + Duration::from_millis(500)));
    settle(&mut player, t3 + Duration::from_millis(500)).await;
    assert_eq!(rows(&player)[0], "abcd");
    assert_eq!(player.state(), PlayerState::Ended);
    assert_eq!(player.total, Some(12.0));
    assert_eq!(player.next_due(), None);
    assert!(player.error.is_none());

    // Speeds stay within 0.5–8.
    for _ in 0..10 {
        player.change_speed(true, t3);
    }
    assert!((player.speed() - 8.0).abs() < f64::EPSILON);
    for _ in 0..10 {
        player.change_speed(false, t3);
    }
    assert!((player.speed() - 0.5).abs() < f64::EPSILON);
    assert_eq!(speed_label(0.5), "×0.5");
    assert_eq!(speed_label(8.0), "×8");
    assert_eq!(clock(83.9), "1:23");
    assert_eq!(clock(3_723.0), "1:02:03");
    assert_eq!(clock(f64::NAN), "0:00");
}

#[tokio::test]
async fn query_replies_are_dropped_by_the_player_listener() {
    // DSR (cursor position), DA (device attributes), DECRQM-like status:
    // a live terminal would answer the remote program with each.
    let bytes = cast(
        20,
        5,
        &[
            (0.1, "o", "x\u{1b}[6n"),
            (0.2, "o", "\u{1b}[c\u{1b}[5n"),
            (0.3, "o", "\u{1b}[>c\u{1b}[18t"),
        ],
    );
    let mut player = player_of(bytes, None);
    play_all(&mut player).await;
    // Alacritty did answer: the replies reached the listener, which
    // dropped them (it holds no input channel at all).
    let dropped = player.listener.dropped_replies.load(Ordering::Relaxed);
    assert!(dropped >= 3, "{dropped} replies");
    assert_eq!(rows(&player)[0], "x");
}

#[tokio::test]
async fn one_sanitiser_spans_the_whole_playback() {
    // An OSC 52 (clipboard) and a title split across events: with one
    // sanitiser per event, the tail would show as text.
    let bytes = cast(
        30,
        5,
        &[
            (0.1, "o", "\u{1b}]52;c;"),
            (0.2, "o", "c2VjcmV0\u{7}ok "),
            (0.3, "o", "\u{1b}]0;evil"),
            (0.4, "o", " title\u{7}done"),
            // Unfinished at the end: dropped by `finish`.
            (0.5, "o", "\u{1b}]2;never"),
        ],
    );
    let mut player = player_of(bytes, None);
    play_all(&mut player).await;
    let screen = rows(&player).join("\n");
    assert!(screen.starts_with("ok done"), "{screen:?}");
    assert!(!screen.contains("c2VjcmV0"), "{screen:?}");
    assert!(!screen.contains("evil") && !screen.contains("never"));
    assert_eq!(player.title.as_deref(), Some("web-01"));
}

#[tokio::test]
async fn modes_set_by_a_recording_stay_in_its_emulator() {
    let bytes = cast(
        20,
        5,
        &[(
            0.1,
            "o",
            "\u{1b}[?1000h\u{1b}[?1006h\u{1b}[?2004h\u{1b}[?1004h\u{1b}[?1049h\u{1b}[>1u",
        )],
    );
    let mut player = player_of(bytes, None);
    play_all(&mut player).await;
    let mode = player.mode();
    // Inside the player's own emulator only (it is drawn, never obeyed).
    assert!(mode.contains(TermMode::MOUSE_REPORT_CLICK));
    assert!(mode.contains(TermMode::BRACKETED_PASTE));
    assert!(mode.contains(TermMode::FOCUS_IN_OUT));
    assert!(mode.contains(TermMode::ALT_SCREEN));
    // The kitty keyboard protocol is off in the player's config.
    assert!(!mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
    assert!(HOST_AFFECTING_MODES.contains(TermMode::BRACKETED_PASTE));
}

#[tokio::test]
async fn resizes_follow_the_file_within_bounds() {
    let bytes = cast(20, 5, &[(0.1, "r", "40x10"), (0.2, "o", "wide")]);
    let mut player = player_of(bytes, Some(5.0));
    assert_eq!((player.size.cols, player.size.rows), (20, 5));
    play_all(&mut player).await;
    assert_eq!((player.size.cols, player.size.rows), (40, 10));
    assert_eq!(player.term().columns(), 40);
    // The listing's duration stays the total when longer.
    assert_eq!(player.total, Some(5.0));
    let clamped = clamp_size(0, 5000);
    assert_eq!((clamped.cols, clamped.rows), (1, MAX_PLAYBACK_DIMENSION));
}

#[tokio::test]
async fn a_damaged_file_plays_up_to_the_damage() {
    let mut bytes = cast(20, 5, &[(0.1, "o", "before")]);
    bytes.extend_from_slice(b"not json\n");
    let mut player = player_of(bytes, None);
    play_all(&mut player).await;
    assert_eq!(rows(&player)[0], "before");
    let error = player.error.clone().unwrap();
    assert_eq!(error.code, "recording_malformed");
    // The summary never quotes recorded content.
    assert!(!error.message.contains("before"));
}

#[tokio::test]
async fn closing_the_player_stops_its_reader() {
    // More events than the channel holds: the reader blocks on a full
    // queue, then ends once the player is dropped.
    let events: Vec<(f64, &str, &str)> = (0..PLAYBACK_QUEUE * 4)
        .map(|i| (f64::from(u32::try_from(i).unwrap()), "o", "."))
        .collect();
    let player = player_of(cast(20, 5, &events), None);
    drop(player);
    // Nothing to join: the test passes when no thread is left hanging on
    // a channel nobody reads (it would keep the test binary alive).
}
