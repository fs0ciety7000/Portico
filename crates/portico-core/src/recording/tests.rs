//! Unit tests of [`crate::recording`]: writer, tap, parser, playback helpers,
//! sanitiser, file safety.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fmt::Write as _;
use std::io::Cursor;
use std::time::Duration;

use super::*;

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn at(unix: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix).unwrap()
}

const HEADER: &str = r#"{"version":2,"width":80,"height":24,"timestamp":1700000000,"title":"web","env":{"TERM":"xterm-256color"}}"#;

fn events(text: &str) -> Vec<Result<TimedEvent>> {
    CastReader::new(Cursor::new(text.as_bytes().to_vec()), "t.cast")
        .unwrap()
        .collect()
}

fn ok_events(text: &str) -> Vec<TimedEvent> {
    events(text).into_iter().map(Result::unwrap).collect()
}

fn code(err: &Error) -> String {
    err.summary().code
}

fn open_err(text: &[u8]) -> String {
    code(&CastReader::new(Cursor::new(text.to_vec()), "t.cast").unwrap_err())
}

fn last_err(text: &str) -> String {
    let all = events(text);
    match all.last() {
        Some(Err(e)) => code(e),
        other => panic!("expected an error, got {other:?}"),
    }
}

fn output(event: &TimedEvent) -> &str {
    match &event.event {
        CastEvent::Output(text) => text,
        CastEvent::Resize { .. } => panic!("not output: {event:?}"),
    }
}

/// Writes `chunks` as output events through a fresh writer, returns the
/// file text.
fn write_chunks(chunks: &[&[u8]]) -> String {
    let mut writer = CastWriter::new(Vec::new(), MAX_RECORDING_BYTES);
    writer.write_line(HEADER).unwrap();
    for (i, chunk) in chunks.iter().enumerate() {
        let at = f64::from(u32::try_from(i).unwrap()) * 0.1;
        writer
            .write_event(TapEvent::Output {
                at,
                bytes: Bytes::copy_from_slice(chunk),
            })
            .unwrap();
    }
    let (sink, done) = writer.finish();
    done.unwrap();
    String::from_utf8(sink).unwrap()
}

fn joined_output(text: &str) -> String {
    ok_events(text)
        .iter()
        .filter_map(|e| match &e.event {
            CastEvent::Output(t) => Some(t.as_str()),
            CastEvent::Resize { .. } => None,
        })
        .collect()
}

// ── names ───────────────────────────────────────────────────────────────

#[test]
fn alias_is_sanitised_for_a_file_name() {
    assert_eq!(sanitize_alias("web-1.prod"), "web-1.prod");
    // Runs of dots collapse: the old `_.._etc_passwd` failed `validate_name`.
    assert_eq!(sanitize_alias("../../etc/passwd"), "_._etc_passwd");
    assert_eq!(sanitize_alias("a b/c\\d"), "a_b_c_d");
    assert_eq!(sanitize_alias("-rf"), "rf");
    assert_eq!(sanitize_alias(".hidden"), "hidden");
    assert_eq!(sanitize_alias("…"), "_");
    assert_eq!(sanitize_alias(""), "session");
    assert_eq!(sanitize_alias("..."), "session");
    assert_eq!(sanitize_alias(&"x".repeat(200)).len(), 64);
    // Whatever goes in, the result is a valid recording name stem.
    for alias in [
        "../x",
        "a..b",
        "a\nb",
        "\u{202e}evil",
        "x".repeat(300).as_str(),
    ] {
        let name = format!("{}-{}.cast", sanitize_alias(alias), name_timestamp(at(0)));
        validate_name(&name).unwrap();
    }
}

#[test]
fn name_timestamp_is_utc_and_sortable() {
    assert_eq!(name_timestamp(at(1_700_000_000)), "20231114T221320Z");
    let offset = at(1_700_000_000).to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
    assert_eq!(name_timestamp(offset), "20231114T221320Z");
}

#[test]
fn only_plain_cast_names_are_valid() {
    for good in ["web-20231114T221320Z.cast", "a.cast", "x_y-2.cast"] {
        validate_name(good).unwrap();
    }
    for bad in [
        "",
        ".cast",
        "a.txt",
        "../a.cast",
        "..cast",
        "a/../b.cast",
        "/etc/a.cast",
        "dir/a.cast",
        "a\\b.cast",
        ".hidden.cast",
        "a..b.cast",
        "a b.cast",
        "a\0.cast",
        "é.cast",
    ] {
        let err = validate_name(bad).unwrap_err();
        assert_eq!(err.code_and_hint().0, "recording_invalid_name", "{bad:?}");
    }
    assert!(validate_name(&format!("{}.cast", "a".repeat(300))).is_err());
}

// ── writer ──────────────────────────────────────────────────────────────

#[test]
fn utf8_split_across_chunks_is_kept_whole() {
    // "€" = E2 82 AC, "😀" = F0 9F 98 80, split at every possible point.
    let text = write_chunks(&[b"a\xE2", b"\x82", b"\xACb\xF0\x9F", b"\x98", b"\x80c"]);
    assert_eq!(joined_output(&text), "a€b😀c");
    assert!(!text.contains('\u{FFFD}'), "{text}");
}

#[test]
fn invalid_utf8_is_replaced_and_a_dangling_sequence_flushed_at_the_end() {
    let text = write_chunks(&[b"ok\xFFok", b"\xC3(", b"end\xE2\x82"]);
    assert_eq!(joined_output(&text), "ok\u{FFFD}ok\u{FFFD}(end\u{FFFD}");
}

#[test]
fn utf8_stream_handles_every_split_of_mixed_text() {
    let sample = "héllo € 😀 ∑ x".as_bytes();
    for cut in 0..=sample.len() {
        let (a, b) = sample.split_at(cut);
        let mut stream = Utf8Stream::default();
        let mut out = stream.decode(a);
        out.push_str(&stream.decode(b));
        assert!(stream.finish().is_none());
        assert_eq!(out.as_bytes(), sample, "cut at {cut}");
    }
}

#[test]
fn header_is_minimal_and_events_are_o_and_r() {
    let mut writer = CastWriter::new(Vec::new(), MAX_RECORDING_BYTES);
    writer
        .write_line(&header_line("web", size(100, 30), at(1_700_000_000)))
        .unwrap();
    writer
        .write_event(TapEvent::Output {
            at: 0.5,
            bytes: Bytes::from_static(b"\x1b[31mred\x1b[0m\r\n"),
        })
        .unwrap();
    writer
        .write_event(TapEvent::Resize {
            at: 0.25, // earlier than the last event: clamped, never decreasing
            cols: 120,
            rows: 40,
        })
        .unwrap();
    let (sink, done) = writer.finish();
    done.unwrap();
    let text = String::from_utf8(sink).unwrap();
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let keys: Vec<&str> = header
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys.len(),
        6,
        "only version, width, height, timestamp, title, env: {keys:?}"
    );
    assert_eq!(header["version"], 2);
    assert_eq!(header["width"], 100);
    assert_eq!(header["height"], 30);
    assert_eq!(header["timestamp"], 1_700_000_000);
    assert_eq!(header["title"], "web");
    assert_eq!(
        header["env"],
        serde_json::json!({ "TERM": "xterm-256color" })
    );
    assert_eq!(
        lines.next(),
        Some(r#"[0.5,"o","\u001b[31mred\u001b[0m\r\n"]"#)
    );
    assert_eq!(lines.next(), Some(r#"[0.5,"r","120x40"]"#));
    assert_eq!(lines.next(), None);

    let parsed = ok_events(&text);
    assert_eq!(
        parsed.get(1).map(|e| &e.event),
        Some(&CastEvent::Resize {
            cols: 120,
            rows: 40
        })
    );
}

#[test]
fn large_output_is_split_into_lines_the_reader_accepts() {
    // Worst case for JSON escaping: control bytes become `\u001b` (6 bytes).
    let chunk = vec![0x1b_u8; 3 * MAX_OUTPUT_PER_EVENT];
    let text = write_chunks(&[&chunk]);
    assert!(text.lines().all(|l| l.len() <= MAX_LINE_BYTES));
    assert!(text.lines().count() >= 4);
    assert_eq!(joined_output(&text).len(), chunk.len());
}

#[test]
fn the_size_cap_stops_cleanly_on_a_line_boundary() {
    let max = 400;
    let mut writer = CastWriter::new(Vec::new(), max);
    writer.write_line(HEADER).unwrap();
    let mut stopped = None;
    for i in 0..100_u32 {
        let result = writer.write_event(TapEvent::Output {
            at: f64::from(i),
            bytes: Bytes::from_static(b"0123456789"),
        });
        if let Err(stop) = result {
            stopped = Some(stop.reason());
            break;
        }
    }
    assert_eq!(stopped, Some(RecordingStopReason::SizeLimit));
    let (sink, _) = writer.finish();
    assert!(sink.len() <= 400, "{}", sink.len());
    assert_eq!(sink.last(), Some(&b'\n'));
    let text = String::from_utf8(sink).unwrap();
    assert!(ok_events(&text).len() > 5);
}

#[test]
fn run_writer_stops_at_the_cap_and_closes_the_queue() {
    let (tx, mut rx) = mpsc::channel(64);
    for i in 0..32_u32 {
        tx.try_send(TapEvent::Output {
            at: f64::from(i),
            bytes: Bytes::from_static(b"payload-payload"),
        })
        .unwrap();
    }
    let mut writer = CastWriter::new(Vec::new(), 200);
    let stop = run_writer(&mut rx, &mut writer);
    assert!(matches!(stop, Some(WriteStop::SizeLimit)), "{stop:?}");
    // The tap sees a closed queue on its next push.
    assert!(tx.is_closed());
}

struct FailingSink;

impl Write for FailingSink {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("disk full"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("disk full"))
    }
}

#[test]
fn a_failed_write_stops_the_recording() {
    let (tx, mut rx) = mpsc::channel(4);
    tx.try_send(TapEvent::Output {
        at: 0.0,
        bytes: Bytes::from_static(b"x"),
    })
    .unwrap();
    let mut writer = CastWriter::new(FailingSink, MAX_RECORDING_BYTES);
    let stop = run_writer(&mut rx, &mut writer).unwrap();
    assert_eq!(stop.reason(), RecordingStopReason::WriteFailed);
    assert!(tx.is_closed());
}

// ── tap ─────────────────────────────────────────────────────────────────

#[test]
fn tap_is_inert_until_activated_and_refuses_a_second_start() {
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let id = TerminalId::next();
    tap.output(&Bytes::from_static(b"ignored"));
    assert!(!tap.is_recording());
    assert!(!tap.stop(RecordingStopReason::Requested));

    let reservation = tap.begin(id).unwrap();
    assert_eq!(reservation.size(), size(80, 24));
    // Reserved while the file is being created.
    assert!(matches!(
        tap.begin(id),
        Err(RecordingError::AlreadyRecording(_))
    ));
    let (tx, mut rx) = mpsc::channel(8);
    let stop = Arc::new(StopCell::default());
    assert!(reservation.activate(tx, Arc::clone(&stop)));
    assert!(tap.is_recording());
    assert!(matches!(
        tap.begin(id),
        Err(RecordingError::AlreadyRecording(_))
    ));

    tap.output(&Bytes::from_static(b"one"));
    tap.resize(size(100, 50));
    tap.output(&Bytes::from_static(b"two"));
    let mut seen = Vec::new();
    let mut last = 0.0;
    while let Ok(event) = rx.try_recv() {
        let t = match &event {
            TapEvent::Output { at, .. } | TapEvent::Resize { at, .. } => *at,
        };
        assert!(t >= last, "times never decrease");
        last = t;
        seen.push(format!("{event:?}").split('(').next().unwrap().to_owned());
    }
    assert_eq!(seen, ["Output", "Resize", "Output"]);

    assert!(tap.stop(RecordingStopReason::Requested));
    assert_eq!(stop.get(), Some(RecordingStopReason::Requested));
    assert!(rx.try_recv().is_err(), "queue closed once drained");
    assert!(!tap.is_recording());
    // Free again.
    tap.begin(id).unwrap();
}

#[test]
fn a_resize_during_start_is_recorded_at_time_zero() {
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let reservation = tap.begin(TerminalId::next()).unwrap();
    tap.resize(size(132, 43));
    let (tx, mut rx) = mpsc::channel(8);
    assert!(reservation.activate(tx, Arc::new(StopCell::default())));
    match rx.try_recv().unwrap() {
        TapEvent::Resize { cols, rows, .. } => assert_eq!((cols, rows), (132, 43)),
        other @ TapEvent::Output { .. } => panic!("{other:?}"),
    }
}

#[test]
fn closing_the_terminal_stops_and_refuses_recordings() {
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let id = TerminalId::next();
    let reservation = tap.begin(id).unwrap();
    let (tx, rx) = mpsc::channel(8);
    let stop = Arc::new(StopCell::default());
    reservation.activate(tx, Arc::clone(&stop));
    tap.close();
    assert_eq!(stop.get(), Some(RecordingStopReason::TerminalClosed));
    assert!(rx.is_closed() || rx.is_empty());
    assert!(matches!(
        tap.begin(id),
        Err(RecordingError::UnknownTerminal(_))
    ));

    // Closed between `begin` and `activate`.
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let reservation = tap.begin(id).unwrap();
    tap.close();
    let (tx, _rx) = mpsc::channel(8);
    let stop = Arc::new(StopCell::default());
    assert!(!reservation.activate(tx, Arc::clone(&stop)));
    assert_eq!(stop.get(), Some(RecordingStopReason::TerminalClosed));
}

#[test]
fn a_full_queue_stops_the_recording_without_waiting() {
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let reservation = tap.begin(TerminalId::next()).unwrap();
    let (tx, rx) = mpsc::channel(4); // never drained: a stuck writer
    let stop = Arc::new(StopCell::default());
    reservation.activate(tx, Arc::clone(&stop));
    let started = Instant::now();
    for _ in 0..10_000 {
        tap.output(&Bytes::from_static(b"flood"));
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the tap never blocks"
    );
    assert_eq!(stop.get(), Some(RecordingStopReason::QueueFull));
    assert!(!tap.is_recording());
    assert_eq!(rx.len(), 4, "what was queued is still written");
}

/// A sink that takes 2 ms per write: a disk much slower than the PTY.
struct SlowSink(Vec<u8>);

impl Write for SlowSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(2));
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_slow_writer_stops_the_recording_and_never_stalls_the_terminal() {
    let tap = Arc::new(TerminalTap::new(size(80, 24)));
    let reservation = tap.begin(TerminalId::next()).unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    let stop = Arc::new(StopCell::default());
    reservation.activate(tx, Arc::clone(&stop));
    let writer = std::thread::spawn(move || {
        let mut writer = CastWriter::new(SlowSink(Vec::new()), MAX_RECORDING_BYTES);
        writer.write_line(HEADER).unwrap();
        let failed = run_writer(&mut rx, &mut writer);
        let (sink, done) = writer.finish();
        done.unwrap();
        (failed.is_none(), sink.0)
    });
    // The terminal side: 5000 chunks as fast as it can.
    let mut slowest = Duration::ZERO;
    for _ in 0..5000 {
        let t = Instant::now();
        tap.output(&Bytes::from_static(b"0123456789abcdef"));
        slowest = slowest.max(t.elapsed());
    }
    assert!(slowest < Duration::from_millis(200), "{slowest:?}");
    assert_eq!(stop.get(), Some(RecordingStopReason::QueueFull));
    let (clean_end, bytes) = writer.join().unwrap();
    assert!(clean_end, "the writer drains what was queued, then ends");
    // What was written is a valid recording (no hole: it just ends early).
    let text = String::from_utf8(bytes).unwrap();
    let parsed = ok_events(&text);
    assert!(!parsed.is_empty() && parsed.len() < 5000);
}

// ── files ───────────────────────────────────────────────────────────────

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[cfg(unix)]
#[test]
fn files_are_0600_in_a_0700_folder_and_names_never_collide() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("data/recordings");
    let first = create_recording(&dir, "web", at(1_700_000_000), size(80, 24), 1024).unwrap();
    assert_eq!(first.name, "web-20231114T221320Z.cast");
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&root.path().join("data")), 0o700);
    assert_eq!(mode(&first.path), 0o600);
    drop(first);
    let second = create_recording(&dir, "web", at(1_700_000_000), size(80, 24), 1024).unwrap();
    assert_eq!(second.name, "web-20231114T221320Z-2.cast");

    // A pre-existing folder with loose permissions is tightened.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    create_recording(&dir, "db", at(1_700_000_000), size(80, 24), 1024).unwrap();
    assert_eq!(mode(&dir), 0o700);
}

#[cfg(unix)]
#[test]
fn a_planted_symlink_is_never_followed() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    std::fs::create_dir(&dir).unwrap();
    let victim = root.path().join("victim");
    std::fs::write(&victim, b"precious").unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("web-20231114T221320Z.cast")).unwrap();
    let created = create_recording(&dir, "web", at(1_700_000_000), size(80, 24), 1024).unwrap();
    assert_eq!(created.name, "web-20231114T221320Z-2.cast");
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
}

#[cfg(unix)]
#[test]
fn a_symlinked_recordings_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let dir = root.path().join("recordings");
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let err = create_recording(&dir, "web", at(0), size(80, 24), 1024).unwrap_err();
    assert_eq!(code(&err), "recording_unsafe_path");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert_eq!(code(&list(&dir).unwrap_err()), "recording_unsafe_path");
    assert_eq!(
        code(&open(&dir, "a.cast").unwrap_err()),
        "recording_unsafe_path"
    );
    assert_eq!(
        code(&delete(&dir, "a.cast").unwrap_err()),
        "recording_unsafe_path"
    );
}

#[cfg(unix)]
fn write_file(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

#[cfg(unix)]
#[test]
fn delete_stays_inside_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    write_file(&dir, "keep.cast", HEADER);
    let outside = root.path().join("outside.cast");
    std::fs::write(&outside, HEADER).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link.cast")).unwrap();
    std::fs::create_dir(dir.join("sub.cast")).unwrap();

    for name in [
        "../outside.cast",
        "/etc/passwd",
        "..",
        ".",
        "",
        "sub/../keep.cast",
        "keep.cast/",
        "keep",
    ] {
        let err = delete(&dir, name).unwrap_err();
        assert_eq!(code(&err), "recording_invalid_name", "{name:?}");
    }
    assert_eq!(
        code(&delete(&dir, "link.cast").unwrap_err()),
        "recording_unsafe_path"
    );
    assert_eq!(
        code(&delete(&dir, "sub.cast").unwrap_err()),
        "recording_unsafe_path"
    );
    assert_eq!(
        code(&delete(&dir, "missing.cast").unwrap_err()),
        "recording_not_found"
    );
    assert!(outside.exists());
    assert!(dir.join("link.cast").exists() || dir.join("link.cast").is_symlink());

    delete(&dir, "keep.cast").unwrap();
    assert!(!dir.join("keep.cast").exists());
    assert_eq!(
        code(&delete(&dir, "keep.cast").unwrap_err()),
        "recording_not_found"
    );
}

#[cfg(unix)]
#[test]
fn open_refuses_symlinks_and_traversal() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    write_file(&dir, "ok.cast", &format!("{HEADER}\n[0.1,\"o\",\"hi\"]\n"));
    let outside = root.path().join("outside.cast");
    std::fs::write(&outside, HEADER).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("link.cast")).unwrap();

    let reader = open(&dir, "ok.cast").unwrap();
    assert_eq!(reader.header().title.as_deref(), Some("web"));
    assert_eq!(reader.count(), 1);
    assert_eq!(
        code(&open(&dir, "link.cast").unwrap_err()),
        "recording_unsafe_path"
    );
    assert_eq!(
        code(&open(&dir, "../outside.cast").unwrap_err()),
        "recording_invalid_name"
    );
    assert_eq!(
        code(&open(&dir, "none.cast").unwrap_err()),
        "recording_not_found"
    );
    let missing = root.path().join("nope");
    assert_eq!(
        code(&open(&missing, "ok.cast").unwrap_err()),
        "recording_not_found"
    );
}

#[cfg(unix)]
#[test]
fn listing_reads_headers_sizes_and_durations() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    assert!(list(&dir).unwrap().is_empty(), "missing folder: empty list");

    let old = r#"{"version":2,"width":80,"height":24,"timestamp":1600000000,"title":"old"}"#;
    write_file(&dir, "old-1.cast", &format!("{old}\n[1.5,\"o\",\"x\"]\n"));
    // A long recording: the duration comes from the tail only.
    let mut long = format!("{HEADER}\n");
    for i in 0..20_000_u32 {
        writeln!(long, "[{i}.25,\"o\",\"line {i}\\r\\n\"]").unwrap();
    }
    write_file(&dir, "web-1.cast", &long);
    write_file(&dir, "broken.cast", "not json\n");
    write_file(&dir, "notes.txt", "ignored");
    std::os::unix::fs::symlink(dir.join("web-1.cast"), dir.join("alias.cast")).unwrap();

    let entries = list(&dir).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["web-1.cast", "old-1.cast", "broken.cast"]);
    let web = entries.first().unwrap();
    assert_eq!(web.alias, "web");
    assert!(web.valid);
    assert_eq!(web.size, u64::try_from(long.len()).unwrap());
    assert_eq!(web.duration, Some(19_999.25));
    assert_eq!((web.width, web.height), (Some(80), Some(24)));
    assert_eq!(web.started, Some(at(1_700_000_000)));
    assert_eq!(entries.get(1).unwrap().duration, Some(1.5));
    let broken = entries.get(2).unwrap();
    assert!(!broken.valid);
    assert_eq!(broken.alias, "broken");
    assert_eq!(broken.duration, None);
}

// ── parser ──────────────────────────────────────────────────────────────

#[test]
fn parses_events_and_skips_unknown_types_and_blank_lines() {
    let text = format!(
        "{HEADER}\r\n\n[0.1,\"o\",\"a\"]\n[0.2,\"i\",\"typed\"]\n[0.3,\"m\",\"\"]\n\
         [0.3,\"r\",\"100x30\"]\n[0.4,\"x\",{{\"future\":1}}]\n[0.5,\"o\",\"b\",\"extra\"]\r\n"
    );
    let mut reader = CastReader::new(Cursor::new(text.into_bytes()), "t.cast").unwrap();
    assert_eq!(
        reader.header(),
        &CastHeader {
            width: 80,
            height: 24,
            timestamp: Some(1_700_000_000),
            title: Some("web".into()),
            term: Some("xterm-256color".into()),
        }
    );
    let all: Vec<TimedEvent> = reader.by_ref().map(Result::unwrap).collect();
    assert_eq!(all.len(), 3);
    assert_eq!(output(all.first().unwrap()), "a");
    assert_eq!(
        all.get(1).unwrap().event,
        CastEvent::Resize {
            cols: 100,
            rows: 30
        }
    );
    assert_eq!(output(all.get(2).unwrap()), "b");
    assert!(reader.next().is_none(), "fused");
}

#[test]
fn header_errors_are_typed() {
    assert_eq!(open_err(b""), "recording_invalid_header");
    assert_eq!(open_err(b"\n"), "recording_invalid_header");
    assert_eq!(open_err(b"garbage\n"), "recording_invalid_header");
    assert_eq!(open_err(b"[2]\n"), "recording_invalid_header");
    assert_eq!(
        open_err(br#"{"version":1,"width":80,"height":24}"#),
        "recording_unsupported_version"
    );
    assert_eq!(
        open_err(br#"{"version":3,"term":{"cols":80,"rows":24}}"#),
        "recording_unsupported_version"
    );
    assert_eq!(
        open_err(br#"{"version":"2","width":80,"height":24}"#),
        "recording_invalid_header"
    );
    for dims in [
        r#""height":24"#,
        r#""width":0,"height":24"#,
        r#""width":80,"height":-1"#,
        r#""width":80.5,"height":24"#,
        r#""width":65535,"height":65535"#,
        r#""width":80,"height":"24""#,
    ] {
        let header = format!("{{\"version\":2,{dims}}}\n");
        assert_eq!(
            open_err(header.as_bytes()),
            "recording_invalid_header",
            "{dims}"
        );
    }
    // Minimal valid header.
    CastReader::new(
        Cursor::new(br#"{"version":2,"width":1,"height":1}"#.to_vec()),
        "t",
    )
    .unwrap();
}

#[test]
fn header_title_is_sanitised() {
    let header =
        br#"{"version":2,"width":80,"height":24,"title":"evil\u001b]52;c;x\u0007\u202etxt"}"#;
    let reader = CastReader::new(Cursor::new(header.to_vec()), "t").unwrap();
    let title = reader.header().title.clone().unwrap();
    assert!(!title.chars().any(char::is_control), "{title:?}");
    assert!(!title.contains('\u{202e}'));
}

#[test]
fn event_errors_are_typed_and_end_the_stream() {
    let cases = [
        ("[0.1,\"o\"]", "recording_malformed"),
        ("{\"t\":1}", "recording_malformed"),
        ("[\"0.1\",\"o\",\"x\"]", "recording_malformed"),
        ("[-1,\"o\",\"x\"]", "recording_malformed"),
        ("[0.1,5,\"x\"]", "recording_malformed"),
        ("[0.1,\"o\",42]", "recording_malformed"),
        ("[0.1,\"r\",\"80x\"]", "recording_malformed"),
        ("[0.1,\"r\",\"0x24\"]", "recording_malformed"),
        ("[0.1,\"r\",\"+80x24\"]", "recording_malformed"),
        ("[0.1,\"r\",\"99999x24\"]", "recording_malformed"),
        ("[0.1,\"r\",7]", "recording_malformed"),
        ("[1e999,\"o\",\"x\"]", "recording_malformed"),
        ("not json", "recording_malformed"),
    ];
    for (line, expected) in cases {
        let text = format!("{HEADER}\n[0.05,\"o\",\"before\"]\n{line}\n[9,\"o\",\"after\"]\n");
        let all = events(&text);
        assert_eq!(
            all.len(),
            2,
            "{line}: what precedes is played, then it stops"
        );
        assert!(all.first().unwrap().is_ok());
        assert_eq!(last_err(&text), expected, "{line}");
    }
}

#[test]
fn time_must_not_decrease() {
    let text = format!("{HEADER}\n[1.0,\"o\",\"a\"]\n[1.0,\"o\",\"b\"]\n[0.5,\"i\",\"c\"]\n");
    assert_eq!(last_err(&text), "recording_time_backwards");
    // Even through a skipped event type.
    let text = format!("{HEADER}\n[2.0,\"m\",\"\"]\n[1.0,\"o\",\"x\"]\n");
    assert_eq!(last_err(&text), "recording_time_backwards");
}

#[test]
fn lines_and_files_are_bounded() {
    let long = format!("{HEADER}\n[0.1,\"o\",\"{}\"]\n", "x".repeat(MAX_LINE_BYTES));
    assert_eq!(last_err(&long), "recording_line_too_long");
    // Without a newline at all.
    let endless = format!("{HEADER}\n{}", "[".repeat(MAX_LINE_BYTES + 10));
    assert_eq!(last_err(&endless), "recording_line_too_long");
    // A header line too long.
    let header = format!(
        "{{\"version\":2,\"width\":80,\"height\":24,\"title\":\"{}\"}}\n",
        "t".repeat(MAX_LINE_BYTES)
    );
    assert_eq!(open_err(header.as_bytes()), "recording_line_too_long");

    // Exactly at the line limit is fine.
    let pad = MAX_LINE_BYTES - r#"[0.1,"o",""]"#.len();
    let exact = format!("{HEADER}\n[0.1,\"o\",\"{}\"]\n", "y".repeat(pad));
    assert_eq!(ok_events(&exact).len(), 1);

    // The whole input is capped.
    let mut text = format!("{HEADER}\n");
    for i in 0..100_u32 {
        writeln!(text, "[{i},\"o\",\"0123456789\"]").unwrap();
    }
    let reader = CastReader::with_limit(Cursor::new(text.clone().into_bytes()), "t", 1000).unwrap();
    let all: Vec<_> = reader.collect();
    match all.last() {
        Some(Err(e)) => assert_eq!(code(e), "recording_too_large"),
        other => panic!("{other:?}"),
    }
    let reader = CastReader::with_limit(
        Cursor::new(text.clone().into_bytes()),
        "t",
        u64::try_from(text.len()).unwrap(),
    )
    .unwrap();
    assert_eq!(reader.map(Result::unwrap).count(), 100);
}

#[test]
fn non_utf8_and_hostile_input_never_panics() {
    let mut text = format!("{HEADER}\n").into_bytes();
    text.extend_from_slice(b"[0.1,\"o\",\"\xff\xfe\"]\n");
    let all: Vec<_> = CastReader::new(Cursor::new(text), "t").unwrap().collect();
    assert_eq!(
        code(all.last().unwrap().as_ref().unwrap_err()),
        "recording_malformed"
    );

    // Deep nesting stays a typed error (serde_json's recursion limit).
    let deep = format!(
        "{HEADER}\n{}0.1{}\n",
        "[".repeat(10_000),
        "]".repeat(10_000)
    );
    assert_eq!(last_err(&deep), "recording_malformed");

    // Every prefix of a valid file, and pseudo-random garbage.
    let valid = write_chunks(&[b"hello \xE2\x82", b"\xAC world", b"\x1b[1mbold"]);
    for cut in 0..valid.len() {
        let prefix = valid.as_bytes().get(..cut).unwrap().to_vec();
        if let Ok(reader) = CastReader::new(Cursor::new(prefix), "t") {
            let _ = reader.count();
        }
    }
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..200 {
        let mut bytes = format!("{HEADER}\n").into_bytes();
        for _ in 0..64 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let alphabet = b"[]\",.0123456789eE-+orx\n\\u{}: \xff";
            let index = usize::try_from(seed % u64::try_from(alphabet.len()).unwrap()).unwrap();
            bytes.push(*alphabet.get(index).unwrap());
        }
        let _ = CastReader::new(Cursor::new(bytes), "t").unwrap().count();
    }
}

// ── playback ────────────────────────────────────────────────────────────

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

#[test]
fn idle_time_is_capped_and_speed_applied() {
    let mut timing = PlaybackTiming::new(1.0);
    assert_eq!(ms(timing.delay_until(0.5)), 500);
    assert_eq!(ms(timing.delay_until(1.0)), 500);
    // 60 s of silence plays as 2 s.
    assert_eq!(ms(timing.delay_until(61.0)), 2000);
    assert_eq!(ms(timing.delay_until(61.0)), 0);

    let mut fast = PlaybackTiming::new(4.0);
    assert_eq!(ms(fast.delay_until(1.0)), 250);
    assert_eq!(ms(fast.delay_until(100.0)), 500, "capped then sped up");

    let mut slow = PlaybackTiming::new(0.5);
    assert_eq!(ms(slow.delay_until(1.0)), 2000);
    assert_eq!(ms(slow.delay_until(30.0)), 4000);

    let mut custom = PlaybackTiming::new(1.0).with_idle_cap(Duration::from_millis(100));
    assert_eq!(ms(custom.delay_until(5.0)), 100);
}

#[test]
fn speed_is_clamped_and_bad_times_are_harmless() {
    assert!((PlaybackTiming::new(100.0).speed() - MAX_PLAYBACK_SPEED).abs() < f64::EPSILON);
    assert!((PlaybackTiming::new(0.01).speed() - MIN_PLAYBACK_SPEED).abs() < f64::EPSILON);
    assert!((PlaybackTiming::new(-3.0).speed() - MIN_PLAYBACK_SPEED).abs() < f64::EPSILON);
    assert!((PlaybackTiming::new(f64::NAN).speed() - 1.0).abs() < f64::EPSILON);
    assert!((PlaybackTiming::new(f64::INFINITY).speed() - 1.0).abs() < f64::EPSILON);
    let mut timing = PlaybackTiming::new(2.0);
    timing.set_speed(8.0);
    assert_eq!(ms(timing.delay_until(1.6)), 200);

    assert_eq!(playback_delay(-5.0, 1.0, PLAYBACK_IDLE_CAP), Duration::ZERO);
    assert_eq!(
        playback_delay(f64::NAN, 1.0, PLAYBACK_IDLE_CAP),
        Duration::ZERO
    );
    assert_eq!(
        playback_delay(f64::INFINITY, 1.0, PLAYBACK_IDLE_CAP),
        Duration::ZERO
    );
    let mut timing = PlaybackTiming::new(1.0);
    assert_eq!(timing.delay_until(f64::NAN), Duration::ZERO);
    assert_eq!(ms(timing.delay_until(1.0)), 1000);
    assert_eq!(timing.delay_until(0.5), Duration::ZERO, "never negative");
}

// ── sanitiser ───────────────────────────────────────────────────────────

/// Sanitises `input` fed in pieces cut at every position (and char by
/// char), checking the result never depends on the chunking.
fn sanitize_all_splits(input: &str) -> String {
    let whole = sanitize_playback(input);
    let boundaries: Vec<usize> = (0..=input.len())
        .filter(|i| input.is_char_boundary(*i))
        .collect();
    for &cut in &boundaries {
        let (a, b) = input.split_at(cut);
        let mut s = PlaybackSanitizer::new();
        let mut out = s.sanitize(a);
        out.push_str(&s.sanitize(b));
        assert_eq!(out, whole, "split at {cut} of {input:?}");
    }
    let mut s = PlaybackSanitizer::new();
    let by_char: String = input
        .chars()
        .map(|c| s.sanitize(c.encode_utf8(&mut [0; 4])))
        .collect();
    assert_eq!(by_char, whole, "char by char");
    whole
}

#[test]
fn benign_sequences_pass_unchanged() {
    for benign in [
        "plain text\r\n",
        "\x1b[31mred\x1b[0m \x1b[1;38;5;208mbold orange\x1b[m",
        "\x1b[38;2;10;20;30mtruecolor\x1b[0m",
        "\x1b[H\x1b[2J\x1b[10;20H\x1b[?25l\x1b[?1049h\x1b[K\x1b[A\x1b[3C",
        "\x1b7saved\x1b8\x1bM\x1b(B\x1b=\x1b>",
        "\x1b[?2004h bracketed \x1b[200~",
        "tab\tbell\x07 backspace\x08 é 😀",
        "\x1b]4;1;rgb:ff/00/00\x07\x1b]104\x07",
        "\x1b]11;#202020\x1b\\",
    ] {
        assert_eq!(sanitize_all_splits(benign), benign, "{benign:?}");
    }
}

#[test]
fn sequences_acting_outside_the_terminal_are_dropped() {
    let cases = [
        // OSC 52 clipboard, BEL and ST terminated.
        ("a\x1b]52;c;ZXZpbA==\x07b", "ab"),
        ("a\x1b]52;c;ZXZpbA==\x1b\\b", "ab"),
        // Titles.
        ("\x1b]0;pwned\x07x", "x"),
        ("\x1b]1;icon\x07\x1b]2;title\x1b\\x", "x"),
        // Hyperlink: the text stays, the link goes.
        (
            "\x1b]8;;https://evil.example\x1b\\click\x1b]8;;\x1b\\",
            "click",
        ),
        // Notifications.
        ("\x1b]9;you have mail\x07x", "x"),
        ("\x1b]777;notify;title;body\x07x", "x"),
        // Others with effects outside the grid.
        ("\x1b]7;file:///etc\x07x", "x"),
        ("\x1b]1337;File=name=a;inline=1:AAAA\x07x", "x"),
        ("\x1b]l;old title\x1b\\x", "x"),
        // Colour queries (would make the emulator answer).
        ("\x1b]10;?\x07\x1b]4;1;?\x07x", "x"),
        // C1 introducers and terminator.
        ("a\u{9d}52;c;ZXZpbA==\u{9c}b", "ab"),
        ("a\u{9d}0;t\x07b", "ab"),
        // DCS, SOS, PM, APC strings.
        ("a\x1bP+q544e\x1b\\b", "ab"),
        ("a\x1bPtmux;\x1b\x1b]52;c;eA==\x07\x1b\\b", "ab"),
        ("a\x1b_Gf=100;AAAA\x1b\\b", "ab"),
        ("a\x1b^pm\x1b\\b\x1bXsos\x1b\\c", "abc"),
        ("a\u{90}dcs\u{9c}b\u{9f}apc\u{9c}c", "abc"),
        // Aborted by CAN / SUB.
        ("a\x1b]0;t\x18b", "ab"),
        ("a\x1bPq\x1ab", "ab"),
    ];
    for (input, expected) in cases {
        assert_eq!(sanitize_all_splits(input), expected, "{input:?}");
    }
}

#[test]
fn stray_escapes_are_dropped() {
    assert_eq!(sanitize_all_splits("\x1b\x1b[0m"), "\x1b[0m");
    assert_eq!(sanitize_all_splits("a\x1b\\b"), "ab");
}

#[test]
fn an_escape_aborting_an_osc_starts_a_new_sequence() {
    // `ESC [` inside a title: the title is dropped, the SGR is kept.
    assert_eq!(
        sanitize_all_splits("\x1b]0;title\x1b[31mred"),
        "\x1b[31mred"
    );
    // A new OSC right after an aborted one is judged on its own.
    assert_eq!(sanitize_all_splits("\x1b]2;a\x1b]52;c;eA==\x07ok"), "ok");
    assert_eq!(sanitize_all_splits("\x1bPq\x1b[1mbold"), "\x1b[1mbold");
}

#[test]
fn state_is_kept_across_chunks() {
    let mut s = PlaybackSanitizer::new();
    let mut out = String::new();
    for piece in ["x\x1b", "]5", "2;c;", "ZXZpbA", "==\x1b", "\\y\x1b[", "0m"] {
        out.push_str(&s.sanitize(piece));
    }
    assert_eq!(out, "xy\x1b[0m");
    // An unfinished sequence at the end is dropped, and the next stream
    // starts clean.
    let mut s = PlaybackSanitizer::new();
    assert_eq!(s.sanitize("a\x1b]0;unterminated"), "a");
    s.finish();
    assert_eq!(s.sanitize("b"), "b");
}

#[test]
fn kept_osc_bodies_are_bounded() {
    let huge = format!("\x1b]4;1;{}\x07ok", "f".repeat(10_000));
    assert_eq!(sanitize_playback(&huge), "ok");
    let digits = format!("\x1b]{}0;t\x07ok", "1".repeat(50));
    assert_eq!(sanitize_playback(&digits), "ok");
}

// ── sanitiser against real parsers (security review) ────────────────────

/// What vte (the parser of `alacritty_terminal`, so of the TUI) dispatches.
#[derive(Debug, Default)]
struct VteRecorder {
    /// Parameters of every dispatched OSC.
    oscs: Vec<Vec<Vec<u8>>>,
    /// DCS hooks (`DECRQSS`, sixel, tmux passthrough…).
    hooks: usize,
}

impl vte::Perform for VteRecorder {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.oscs
            .push(params.iter().map(|param| param.to_vec()).collect());
    }

    fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {
        self.hooks += 1;
    }
}

/// The OSC numbers [`PlaybackSanitizer`] lets through.
const ALLOWED_OSC: [&str; 8] = ["4", "10", "11", "12", "104", "110", "111", "112"];

/// Feeds `output` to vte, cut into byte chunks at `cuts` (sorted), and
/// checks nothing acting outside the grid is dispatched.
fn assert_inert_in_vte(output: &str, cuts: &[usize], input: &str) {
    let mut parser = vte::Parser::new();
    let mut seen = VteRecorder::default();
    let bytes = output.as_bytes();
    let mut from = 0;
    for &cut in cuts.iter().chain(std::iter::once(&bytes.len())) {
        let cut = cut.clamp(from, bytes.len());
        parser.advance(&mut seen, &bytes[from..cut]);
        from = cut;
    }
    assert_eq!(seen.hooks, 0, "vte DCS hook for {input:?} -> {output:?}");
    for params in &seen.oscs {
        let number = params
            .first()
            .map(|p| String::from_utf8_lossy(p).into_owned());
        assert!(
            number.as_deref().is_some_and(|n| ALLOWED_OSC.contains(&n)),
            "vte OSC {number:?} for {input:?} -> {output:?}"
        );
        assert!(
            !params.iter().any(|p| p.contains(&b'?')),
            "vte OSC query for {input:?} -> {output:?}"
        );
    }
}

/// States of the xterm.js 6 parser (`EscapeSequenceParser.ts`,
/// `VT500_TRANSITION_TABLE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Xterm {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsPassthrough,
    SosPmApcString,
}

/// What the model of the xterm.js parser saw.
#[derive(Debug, Default)]
struct XtermSeen {
    /// Data of every OSC ended successfully (BEL, ST, ESC).
    oscs: Vec<String>,
    hooks: usize,
    /// DCS / SOS / PM / APC strings entered (dropped by the sanitiser, so
    /// never expected).
    strings: usize,
}

fn xterm_executable(c: u32) -> bool {
    matches!(c, 0x00..=0x17 | 0x19 | 0x1c..=0x1f)
}

/// A transcription of the xterm.js 6 transition table, by code point
/// (`code < 0xA0 ? code : NON_ASCII_PRINTABLE`), with the actions that
/// matter here: OSC start / put / end, DCS hook, string entry. Executes,
/// prints, CSI and ESC dispatches only change state.
// One arm per row of the table, as written there: easier to check against
// the source than a shorter, merged version.
#[allow(clippy::match_same_arms, clippy::too_many_lines)]
fn xterm_model(text: &str) -> XtermSeen {
    use Xterm as S;
    let mut seen = XtermSeen::default();
    let mut state = S::Ground;
    let mut osc = String::new();
    for ch in text.chars() {
        let code = u32::from(ch);
        let c = if code < 0xa0 { code } else { 0xa0 };
        // OSC_STRING and DCS_PASSTHROUGH terminators override "anywhere".
        if state == S::OscString && matches!(c, 0x9c | 0x1b | 0x18 | 0x1a | 0x07) {
            if c != 0x18 && c != 0x1a {
                seen.oscs.push(std::mem::take(&mut osc));
            }
            state = if c == 0x1b { S::Escape } else { S::Ground };
            continue;
        }
        if state == S::DcsPassthrough && matches!(c, 0x1b | 0x9c | 0x18 | 0x1a) {
            state = if c == 0x1b { S::Escape } else { S::Ground };
            continue;
        }
        // "Anywhere" rules (C1 and CAN, SUB, ESC), in every state.
        let anywhere = match c {
            0x18 | 0x1a | 0x99 | 0x9a | 0x80..=0x8f | 0x91..=0x97 | 0x9c => Some(S::Ground),
            0x1b => Some(S::Escape),
            0x9d => {
                osc.clear();
                Some(S::OscString)
            }
            0x98 | 0x9e | 0x9f => Some(S::SosPmApcString),
            0x9b => Some(S::CsiEntry),
            0x90 => Some(S::DcsEntry),
            _ => None,
        };
        if let Some(next) = anywhere {
            if matches!(next, S::SosPmApcString | S::DcsEntry) {
                seen.strings += 1;
            }
            state = next;
            continue;
        }
        let exec = xterm_executable(c);
        state = match state {
            S::Ground => S::Ground,
            S::Escape => match c {
                _ if exec => S::Escape,
                0x7f => S::Escape,
                0x20..=0x2f => S::EscapeIntermediate,
                0x5d => {
                    osc.clear();
                    S::OscString
                }
                0x58 | 0x5e | 0x5f => {
                    seen.strings += 1;
                    S::SosPmApcString
                }
                0x5b => S::CsiEntry,
                0x50 => {
                    seen.strings += 1;
                    S::DcsEntry
                }
                _ => S::Ground,
            },
            S::EscapeIntermediate => match c {
                _ if exec => S::EscapeIntermediate,
                0x7f | 0x20..=0x2f => S::EscapeIntermediate,
                _ => S::Ground,
            },
            S::CsiEntry => match c {
                _ if exec => S::CsiEntry,
                0x7f => S::CsiEntry,
                0x30..=0x3f => S::CsiParam,
                0x20..=0x2f => S::CsiIntermediate,
                _ => S::Ground,
            },
            S::CsiParam => match c {
                _ if exec => S::CsiParam,
                0x7f | 0x30..=0x3b => S::CsiParam,
                0x3c..=0x3f => S::CsiIgnore,
                0x20..=0x2f => S::CsiIntermediate,
                _ => S::Ground,
            },
            S::CsiIntermediate => match c {
                _ if exec => S::CsiIntermediate,
                0x7f | 0x20..=0x2f => S::CsiIntermediate,
                0x30..=0x3f => S::CsiIgnore,
                _ => S::Ground,
            },
            S::CsiIgnore => match c {
                0x40..=0x7e => S::Ground,
                _ => S::CsiIgnore,
            },
            S::OscString => {
                if matches!(c, 0x20..=0x7f | 0xa0) {
                    osc.push(ch);
                }
                S::OscString
            }
            S::DcsEntry | S::DcsParam | S::DcsIntermediate => match (state, c) {
                (_, 0x40..=0x7e) => {
                    seen.hooks += 1;
                    S::DcsPassthrough
                }
                (_, 0xa0) => S::Ground,
                (S::DcsIntermediate, 0x30..=0x3f) | (_, 0x3c..=0x3f) => S::DcsIgnore,
                (_, 0x20..=0x2f) => S::DcsIntermediate,
                (_, 0x30..=0x3b) => S::DcsParam,
                _ => state,
            },
            S::DcsIgnore => S::DcsIgnore,
            S::DcsPassthrough => S::DcsPassthrough,
            S::SosPmApcString => S::SosPmApcString,
        };
    }
    seen
}

fn assert_inert_in_xterm(output: &str, input: &str) {
    let seen = xterm_model(output);
    assert_eq!(
        seen.hooks, 0,
        "xterm.js DCS hook for {input:?} -> {output:?}"
    );
    assert_eq!(
        seen.strings, 0,
        "xterm.js string entered for {input:?} -> {output:?}"
    );
    for data in &seen.oscs {
        let number = data.split(';').next().unwrap_or_default();
        assert!(
            ALLOWED_OSC.contains(&number),
            "xterm.js OSC {data:?} for {input:?} -> {output:?}"
        );
        assert!(!data.contains('?'), "xterm.js OSC query for {input:?}");
    }
}

/// The structural invariant of the sanitiser's output.
fn assert_output_invariant(output: &str, input: &str) {
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        assert!(!is_c1(c), "C1 {c:?} emitted for {input:?} -> {output:?}");
        if c == '\x1b' {
            assert!(
                chars.peek().is_some_and(|n| matches!(n, ' '..='~')),
                "ESC not followed by ' '..='~' for {input:?} -> {output:?}"
            );
        }
    }
}

#[test]
fn the_xterm_js_model_reproduces_the_reviewed_bypasses() {
    // On raw (unsanitised) input, the model sees what the review saw in
    // xterm.js.
    assert_eq!(xterm_model("\x1b\n]0;PWNED\x07").oscs, ["0;PWNED"]);
    assert_eq!(xterm_model("\x1b\u{9d}0;PWNED\x07").oscs, ["0;PWNED"]);
    assert_eq!(xterm_model("\x1b\u{90}$qm\x1b\\").hooks, 1);
    assert_eq!(xterm_model("\x1b\x08P$qm\x1b\\").hooks, 1);
    assert_eq!(
        xterm_model("\x1b]4;1;\u{9d}52;c;aGk=\x07").oscs,
        ["52;c;aGk="]
    );
    assert!(xterm_model("\x1b]0;t\x18").oscs.is_empty(), "CAN aborts");
    assert!(xterm_model("\x1b[31m\x1b(Bx").oscs.is_empty());
}

#[test]
fn no_sequence_can_be_smuggled_past_the_sanitizer() {
    // Confirmed by the review: ESC + C0 / DEL, ESC + C1, C1 in a kept OSC.
    let reviewed = [
        "\x1b\n]0;PWNED\x07",
        "\x1b\r]8;;http://x/\x07",
        "\x1b\x08P$qm\x1b\\",
        "\x1b]52;c;x\x1b\n]2;T\x07",
        "\x1b\u{9d}0;PWNED\x07",
        "\x1b\u{90}$qm\x1b\\",
        "\x1b]4;1;\u{9d}52;c;aGk=\x07",
        "\x1b\x7f]0;PWNED\x07",
        "\x1bP$q\x1b\x0b\x1b]2;T\x07",
    ];
    for input in reviewed {
        let output = sanitize_all_splits(input);
        assert_output_invariant(&output, input);
        for forbidden in ["]0;", "]2;", "]8;", "]52;", "P$q"] {
            assert!(
                !output.contains(forbidden),
                "{forbidden:?} survives in {input:?} -> {output:?}"
            );
        }
        assert_inert_in_vte(&output, &[], input);
        assert_inert_in_xterm(&output, input);
    }
    // Found while checking vte: after ESC it ignores the bytes of a
    // non-ASCII character and stays in ESCAPE (xterm.js goes back to
    // ground). The ESC is dropped, so a following `]` is plain text in both.
    for input in [
        "\x1bé]0;PWNED\x07",
        "\x1b\u{a0}]52;c;eA==\x07",
        "\x1b(\u{9d}0;T\x07",
        "\x1b[\u{9d}0;T\x07",
        "\x1b]0;a\x1b\u{9c}]2;T\x07",
        "\x1bP\u{98}x\u{9c}\x1b]0;T\x07",
        "\u{9b}\x1b\u{9d}2;T\x07",
    ] {
        let output = sanitize_all_splits(input);
        assert_output_invariant(&output, input);
        assert!(!output.contains("\x1b]"), "{input:?} -> {output:?}");
        assert_inert_in_vte(&output, &[], input);
        assert_inert_in_xterm(&output, input);
    }
    // Text and the kept sequences still pass.
    assert_eq!(sanitize_all_splits("\x1b\n[1mx"), "\n\x1b[1mx");
    assert_eq!(sanitize_all_splits("\x1bé"), "é");
    assert_eq!(
        sanitize_all_splits("\x1b]4;1;\nrgb:ff/00/00\x07"),
        "\x1b]4;1;rgb:ff/00/00\x07"
    );
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
}

/// The fuzzing alphabet: every introducer, terminator and abort of the
/// sequences the sanitiser handles, in 7-bit, C1 and edge forms.
const FUZZ_ALPHABET: [char; 17] = [
    '\x1b', ']', 'P', '\\', '\x07', '\n', '\x7f', '\u{9d}', '\u{90}', '\u{9c}', '0', '2', '4', '5',
    ';', '?', 'a',
];

/// Sanitises `input` whole and cut in random chunks (same result), then
/// checks the output against the invariant, vte (fed in random byte chunks)
/// and the xterm.js model.
fn fuzz_one(input: &str, rng: &mut Rng) {
    let whole = sanitize_playback(input);
    let boundaries: Vec<usize> = (0..=input.len())
        .filter(|i| input.is_char_boundary(*i))
        .collect();
    let mut cuts: Vec<usize> = (0..2)
        .map(|_| boundaries[rng.below(boundaries.len())])
        .collect();
    cuts.sort_unstable();
    let mut sanitizer = PlaybackSanitizer::new();
    let mut chunked = String::new();
    let mut from = 0;
    for cut in cuts.iter().copied().chain(std::iter::once(input.len())) {
        chunked.push_str(&sanitizer.sanitize(&input[from..cut]));
        from = cut;
    }
    assert_eq!(chunked, whole, "chunking changed {input:?}");
    assert_output_invariant(&whole, input);
    let byte_cuts = [rng.below(whole.len() + 1)];
    assert_inert_in_vte(&whole, &byte_cuts, input);
    assert_inert_in_xterm(&whole, input);
}

/// Every string over [`FUZZ_ALPHABET`] up to `max_len` characters, after
/// `prefix`. Returns how many were checked.
fn fuzz_exhaustive(prefix: &str, max_len: u32, rng: &mut Rng) -> usize {
    let base = FUZZ_ALPHABET.len();
    let mut count = 0;
    let mut input = String::new();
    for len in 0..=max_len {
        for mut n in 0..base.pow(len) {
            input.clear();
            input.push_str(prefix);
            for _ in 0..len {
                input.push(FUZZ_ALPHABET[n % base]);
                n /= base;
            }
            fuzz_one(&input, rng);
            count += 1;
        }
    }
    count
}

/// Differential fuzzing against vte 0.15 (the version `alacritty_terminal`
/// uses, so what the TUI runs) and the xterm.js model: no input over the
/// alphabet makes either dispatch an OSC outside the colour allowlist, an
/// OSC query, or a DCS.
///
/// Exhaustive over all strings of up to 5 characters (1.5 M inputs), and of
/// up to 4 characters after prefixes that put the sanitiser in each of its
/// states (OSC number being read, kept OSC, dropped OSC, string, pending
/// ESC). Length 8 (7·10⁹ inputs) is far beyond a unit test: 300 000 random
/// strings of 6 to 12 characters cover longer interleavings instead.
#[test]
fn sanitized_output_never_dispatches_a_forbidden_osc_in_vte() {
    let mut rng = Rng(0x5eed_0f19_0171_c0de);
    let mut count = fuzz_exhaustive("", 5, &mut rng);
    for prefix in [
        "\x1b",
        "\x1b]",
        "\x1b]1",
        "\x1b]4;",
        "\x1b]4;1;",
        "\x1b]52;",
        "\x1bP",
        "\u{9d}4;",
        "\x1b]4;1;x\x1b",
    ] {
        count += fuzz_exhaustive(prefix, 4, &mut rng);
    }
    let mut input = String::new();
    for _ in 0..300_000 {
        input.clear();
        let len = 6 + rng.below(7);
        for _ in 0..len {
            input.push(FUZZ_ALPHABET[rng.below(FUZZ_ALPHABET.len())]);
        }
        fuzz_one(&input, &mut rng);
        count += 1;
    }
    assert!(count > 2_000_000, "{count}");
}

// ── names, files, writer (security review, low findings) ────────────────

#[test]
fn every_created_name_is_valid() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    let seventy_dots = ".".repeat(70);
    let aliases = [
        "a..b",
        "..",
        "a.",
        "x...y",
        "é..ß",
        "-.-..",
        seventy_dots.as_str(),
    ];
    let mut created = Vec::new();
    for alias in aliases {
        let recording = create_recording(&dir, alias, at(1_700_000_000), size(80, 24), 1024)
            .unwrap_or_else(|e| panic!("{alias:?}: {e}"));
        validate_name(&recording.name).unwrap_or_else(|e| panic!("{alias:?}: {e}"));
        created.push(recording.name);
    }
    let listed: Vec<String> = list(&dir).unwrap().into_iter().map(|e| e.name).collect();
    for name in &created {
        assert!(listed.contains(name), "{name} is not listed: {listed:?}");
        delete(&dir, name).unwrap();
    }
    assert!(list(&dir).unwrap().is_empty());
    assert_eq!(sanitize_alias("a..b"), "a.b");
    assert_eq!(sanitize_alias("é..ß"), "_._");
    assert_eq!(sanitize_alias(&seventy_dots), "session");
}

/// Accepts `limit` bytes (cutting a write short), then fails like a full
/// disk.
#[derive(Debug)]
struct FullDisk {
    data: Vec<u8>,
    limit: usize,
}

impl Write for FullDisk {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let room = self.limit.saturating_sub(self.data.len());
        if room == 0 {
            return Err(std::io::Error::other("no space left on device"));
        }
        let n = room.min(buf.len());
        self.data.extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Truncate for FullDisk {
    fn truncate_to(&mut self, len: u64) -> std::io::Result<()> {
        self.data.truncate(usize::try_from(len).unwrap());
        Ok(())
    }
}

#[test]
fn a_failed_write_leaves_complete_lines_only() {
    for limit in [1, 150, 70_000, 200_001] {
        let (tx, mut rx) = mpsc::channel(4096);
        for i in 0..3000_u32 {
            tx.try_send(TapEvent::Output {
                at: f64::from(i) / 100.0,
                bytes: Bytes::from(format!("line {i} \x1b[1m{}\x1b[0m\r\n", "x".repeat(60))),
            })
            .unwrap();
        }
        drop(tx);
        let mut writer = CastWriter::new(
            FullDisk {
                data: Vec::new(),
                limit,
            },
            MAX_RECORDING_BYTES,
        );
        writer.write_line(HEADER).unwrap();
        let stop = run_writer(&mut rx, &mut writer).expect("the disk fills up");
        assert_eq!(stop.reason(), RecordingStopReason::WriteFailed);
        let written = usize::try_from(writer.written()).unwrap();
        assert!(written <= limit);
        let disk = truncate_to_committed(writer).unwrap();
        assert_eq!(disk.data.len(), written, "limit {limit}");
        if written > 0 {
            assert_eq!(disk.data.last(), Some(&b'\n'), "limit {limit}");
            let text = String::from_utf8(disk.data).unwrap();
            assert!(
                events(&text).iter().all(Result::is_ok),
                "limit {limit}: every line parses"
            );
        }
    }
}

#[test]
fn truncate_to_committed_cuts_a_file_back_to_its_complete_lines() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("t.cast");
    let file = File::create(&path).unwrap();
    let mut writer = CastWriter::new(file, MAX_RECORDING_BYTES);
    writer.write_line(HEADER).unwrap();
    writer.flush().unwrap();
    // A torn line reached the file (a partial write before ENOSPC), and a
    // complete line is still buffered.
    let mut raw = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    raw.write_all(b"[0.5, \"o\", \"tor").unwrap();
    writer.write_line("[1.0, \"o\", \"buffered\"]").unwrap();
    truncate_to_committed(writer).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, format!("{HEADER}\n"));
}

#[test]
fn recordings_of_huge_terminals_stay_playable() {
    let header = header_line("big", size(5000, 3000), at(1_700_000_000));
    let mut writer = CastWriter::new(Vec::new(), MAX_RECORDING_BYTES);
    writer.write_line(&header).unwrap();
    for (cols, rows) in [(2000, 2000), (0, 0), (1000, 1), (u16::MAX, 40)] {
        writer
            .write_event(TapEvent::Resize {
                at: 0.5,
                cols,
                rows,
            })
            .unwrap();
    }
    let (sink, done) = writer.finish();
    done.unwrap();
    let text = String::from_utf8(sink).unwrap();
    let reader = CastReader::new(Cursor::new(text.clone().into_bytes()), "big.cast").unwrap();
    assert_eq!(
        (reader.header().width, reader.header().height),
        (MAX_PLAYBACK_DIMENSION, MAX_PLAYBACK_DIMENSION)
    );
    let sizes: Vec<(u16, u16)> = ok_events(&text)
        .into_iter()
        .map(|e| match e.event {
            CastEvent::Resize { cols, rows } => (cols, rows),
            CastEvent::Output(_) => panic!("unexpected output"),
        })
        .collect();
    assert_eq!(sizes, [(1000, 1000), (1, 1), (1000, 1), (1000, 40)]);
}

#[cfg(unix)]
#[test]
fn a_recordings_folder_owned_by_someone_else_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    if !rustix::process::geteuid().is_root() {
        // Only root can give a folder away: nothing to check otherwise.
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("recordings");
    std::fs::create_dir(&dir).unwrap();
    std::os::unix::fs::chown(&dir, Some(4242), Some(4242)).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let err = create_recording(&dir, "web", at(0), size(80, 24), 1024).unwrap_err();
    assert_eq!(code(&err), "recording_foreign_owner");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing created"
    );
    assert_eq!(mode(&dir), 0o777, "someone else's folder is never chmod-ed");
}

#[test]
fn header_title_drops_format_characters() {
    let header = "{\"version\":2,\"width\":80,\"height\":24,\"title\":\"a\u{200e}b\u{200f}c\u{061c}d\u{200b}e\u{2060}f\u{feff}g\u{2028}h\u{2029}i\u{202e}j\u{2066}k\u{00ad}l\u{e0041}m\"}";
    let reader = CastReader::new(Cursor::new(header.as_bytes().to_vec()), "t").unwrap();
    assert_eq!(reader.header().title.as_deref(), Some("abcdefghijklm"));
    // Control characters are still replaced, visibly.
    let header = br#"{"version":2,"width":80,"height":24,"title":"a\u0007b"}"#;
    let reader = CastReader::new(Cursor::new(header.to_vec()), "t").unwrap();
    assert_eq!(reader.header().title.as_deref(), Some("a\u{FFFD}b"));
}
