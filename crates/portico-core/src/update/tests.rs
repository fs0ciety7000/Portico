use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use time::macros::datetime;

use super::*;

const URL: &str = "https://github.com/fs0ciety7000/Portico/releases/tag/v0.3.1";

fn v(text: &str) -> semver::Version {
    semver::Version::parse(text).expect("version")
}

fn body(tag: &str, url: &str) -> Vec<u8> {
    format!(
        r#"{{"tag_name":"{tag}","html_url":"{url}","draft":false,"prerelease":false,"name":"x","assets":[{{"browser_download_url":"https://evil.example/x"}}]}}"#
    )
    .into_bytes()
}

// ── versions and parsing ────────────────────────────────────────────────

#[test]
fn tags_are_plain_semver_and_compare_as_such() {
    assert_eq!(parse_tag("v0.3.1"), Some(v("0.3.1")));
    assert_eq!(parse_tag("1.10.0"), Some(v("1.10.0")));
    for bad in [
        "v0.4.0-rc.1",
        "v0.4.0+build",
        "0.4",
        "latest",
        "",
        "vv1.0.0",
        "v1.0.0 ",
        &format!("v1.0.{}", "1".repeat(40)),
    ] {
        assert_eq!(parse_tag(bad), None, "{bad:?}");
    }
    // Numeric, not lexical: 0.10.0 > 0.9.9, 1.0.0 > 0.99.0.
    assert!(parse_tag("v0.10.0") > parse_tag("v0.9.9"));
    assert!(parse_tag("v1.0.0") > parse_tag("v0.99.0"));
    assert_eq!(current_version().to_string(), CURRENT_VERSION);
}

#[test]
fn a_release_is_parsed_and_extra_fields_are_ignored() {
    assert_eq!(
        parse_release(&body("v0.3.1", URL)).expect("release"),
        Release {
            version: v("0.3.1"),
            url: URL.into()
        }
    );
}

#[test]
fn unusable_answers_are_refused() {
    let release = |json: &str| parse_release(json.as_bytes());
    for (json, why) in [
        ("not json", "garbage"),
        ("{}", "no fields"),
        (
            r#"{"html_url":"https://github.com/fs0ciety7000/Portico/releases/tag/v1.0.0"}"#,
            "no tag",
        ),
        (r#"{"tag_name":"v1.0.0"}"#, "no link"),
        (r#"{"tag_name":"v1.0.0","html_url":null}"#, "null link"),
        (r#"{"tag_name":1,"html_url":"x"}"#, "wrong type"),
    ] {
        assert!(
            matches!(release(json), Err(UpdateError::Release { .. })),
            "{why}"
        );
    }
    let draft = format!(r#"{{"tag_name":"v1.0.0","html_url":"{URL}","draft":true}}"#);
    let pre = format!(r#"{{"tag_name":"v1.0.0","html_url":"{URL}","prerelease":true}}"#);
    for json in [draft, pre] {
        assert!(release(&json).is_err(), "{json}");
    }
    assert!(parse_release(&body("v1.0.0-beta.1", URL)).is_err());
}

#[test]
fn only_portico_release_pages_are_kept() {
    assert!(valid_release_url(URL));
    assert!(valid_release_url(
        "https://github.com/fs0ciety7000/Portico/releases/latest"
    ));
    for bad in [
        "http://github.com/fs0ciety7000/Portico/releases/tag/v1.0.0",
        "https://github.com/evil/Portico/releases/tag/v1.0.0",
        "https://github.com/fs0ciety7000/Portico/releases/",
        "https://github.com/fs0ciety7000/Portico/releases/../../evil",
        "https://github.com/fs0ciety7000/Portico/releases/tag/v1?x=1",
        "https://github.com/fs0ciety7000/Portico/releases/tag/v1#x",
        "https://github.com/fs0ciety7000/Portico/releases/tag/v1 x",
        "https://github.com/fs0ciety7000/Portico/releases/tag/v1\u{202e}",
        "https://github.com.evil.example/fs0ciety7000/Portico/releases/tag/v1",
        "https://user@github.com/fs0ciety7000/Portico/releases/tag/v1",
        "https://github.com/fs0ciety7000/Portico/releases/x@evil.com",
        "https://github.com/fs0ciety7000/Portico/releases/%2e%2e/%2e%2e/evil",
        "https://github.com/fs0ciety7000/Portico/releases/x\\@evil.com",
        "https://github.com/fs0ciety7000/Portico/releases/x:1",
        "javascript:alert(1)//https://github.com/fs0ciety7000/Portico/releases/x",
        &format!("{RELEASE_PAGE_PREFIX}{}", "a".repeat(300)),
    ] {
        assert!(!valid_release_url(bad), "{bad:?}");
        assert!(parse_release(&body("v9.0.0", bad)).is_err(), "{bad:?}");
    }
}

// ── recording a check, status ───────────────────────────────────────────

const NOW: OffsetDateTime = datetime!(2026-10-23 12:00 UTC);

#[test]
fn a_newer_release_becomes_available_once() {
    let current = v("0.3.0");
    let mut cache = UpdateCache::default();
    assert!(record(&mut cache, Ok(body("v0.3.1", URL)), NOW, &current));
    assert_eq!(cache.checked_at, Some(NOW));
    assert_eq!(cache.latest.as_deref(), Some("0.3.1"));
    let shown = status(Some(true), &cache, &current);
    assert_eq!(
        shown.available,
        Some(AvailableUpdate {
            version: "0.3.1".into(),
            url: URL.into()
        })
    );
    assert_eq!(shown.current, "0.3.0");
    // The same answer again: nothing new to announce.
    assert!(!record(&mut cache, Ok(body("v0.3.1", URL)), NOW, &current));
    // Once updated to it, nothing is available any more.
    assert_eq!(status(Some(true), &cache, &v("0.3.1")).available, None);
}

#[test]
fn same_or_older_releases_are_not_offered() {
    let current = v("0.3.0");
    for tag in ["v0.3.0", "v0.2.9", "v0.1.0"] {
        let mut cache = UpdateCache::default();
        assert!(!record(&mut cache, Ok(body(tag, URL)), NOW, &current));
        assert_eq!(
            status(Some(true), &cache, &current).available,
            None,
            "{tag}"
        );
        assert_eq!(cache.checked_at, Some(NOW), "a check all the same");
    }
}

#[test]
fn failures_keep_what_was_known_and_ignored_answers_count_as_checked() {
    let current = v("0.3.0");
    let mut cache = UpdateCache::default();
    record(&mut cache, Ok(body("v0.3.1", URL)), NOW, &current);
    let later = NOW + time::Duration::hours(30);
    let failure = Err(UpdateError::Fetch {
        reason: "timeout".into(),
    });
    assert!(!record(&mut cache, failure, later, &current));
    assert_eq!(cache.failed_at, Some(later));
    assert_eq!(cache.checked_at, Some(NOW));
    assert!(status(None, &cache, &current).available.is_some());
    // A foreign link is ignored, as a check that found nothing new.
    let foreign = body("v9.0.0", "https://evil.example/portico");
    assert!(!record(&mut cache, Ok(foreign), later, &current));
    assert_eq!(cache.failed_at, None);
    assert_eq!(cache.checked_at, Some(later));
    assert_eq!(cache.latest.as_deref(), Some("0.3.1"));
}

#[test]
fn the_next_check_is_due_after_a_day_or_an_hour_after_a_failure() {
    let hour = time::Duration::hours(1);
    let day = Duration::from_secs(24 * 3600);
    let cache = |checked: Option<OffsetDateTime>, failed: Option<OffsetDateTime>| UpdateCache {
        checked_at: checked,
        failed_at: failed,
        ..UpdateCache::default()
    };
    let cases = [
        (cache(None, None), Duration::ZERO, "never checked: now"),
        (cache(Some(NOW), None), day, "just checked"),
        (
            cache(Some(NOW - hour * 23), None),
            Duration::from_secs(3600),
            "checked 23 h ago",
        ),
        (
            cache(Some(NOW - hour * 25), None),
            Duration::ZERO,
            "overdue",
        ),
        (
            cache(None, Some(NOW)),
            Duration::from_secs(3600),
            "failed now, never checked",
        ),
        (
            cache(Some(NOW - hour * 48), Some(NOW - hour / 2)),
            Duration::from_secs(1800),
            "failed 30 min ago",
        ),
        (
            cache(Some(NOW - hour), Some(NOW - hour * 2)),
            Duration::from_secs(23 * 3600),
            "an older failure does not count",
        ),
        (
            cache(Some(NOW + hour * 1000), None),
            day,
            "a date in the future waits a day at most",
        ),
    ];
    for (cache, expected, why) in cases {
        assert_eq!(next_delay(&cache, NOW), expected, "{why}");
    }
}

// ── the loop, in paused time ────────────────────────────────────────────

/// A host whose clock follows Tokio's (paused) clock and whose fetch is a
/// canned answer: no thread, no network.
#[derive(Clone)]
struct FakeHost {
    start: tokio::time::Instant,
    calls: Arc<AtomicUsize>,
    answer: Arc<dyn Fn() -> Result<Vec<u8>, UpdateError> + Send + Sync>,
    checked: Arc<Mutex<Vec<(UpdateCache, bool)>>>,
    enabled: Arc<AtomicBool>,
}

impl FakeHost {
    fn new(answer: impl Fn() -> Result<Vec<u8>, UpdateError> + Send + Sync + 'static) -> Self {
        Self {
            start: tokio::time::Instant::now(),
            calls: Arc::default(),
            answer: Arc::new(answer),
            checked: Arc::default(),
            enabled: Arc::new(AtomicBool::new(true)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CheckHost for FakeHost {
    fn now(&self) -> OffsetDateTime {
        NOW + from_std(self.start.elapsed())
    }

    async fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    async fn fetch(&self) -> Result<Vec<u8>, UpdateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.answer)()
    }

    async fn checked(&self, cache: &UpdateCache, changed: bool) {
        self.checked
            .lock()
            .expect("lock")
            .push((cache.clone(), changed));
    }
}

async fn hours(n: u64) {
    tokio::time::sleep(Duration::from_secs(n * 3600)).await;
}

#[tokio::test(start_paused = true)]
async fn the_loop_stops_when_the_setting_is_off_before_a_request() {
    let host = FakeHost::new(|| Ok(body("v0.3.1", URL)));
    let cancel = CancellationToken::new();
    let task = tokio::spawn(check_loop(
        host.clone(),
        UpdateCache::default(),
        v("0.3.0"),
        cancel.clone(),
    ));
    hours(1).await;
    assert_eq!(host.calls(), 1);
    host.enabled.store(false, Ordering::SeqCst);
    hours(72).await;
    assert_eq!(host.calls(), 1, "no request once turned off");
    assert!(task.is_finished(), "the loop ended by itself");
    assert!(!cancel.is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn the_loop_checks_at_start_then_daily() {
    let host = FakeHost::new(|| Ok(body("v0.3.1", URL)));
    let cancel = CancellationToken::new();
    let task = tokio::spawn(check_loop(
        host.clone(),
        UpdateCache::default(),
        v("0.3.0"),
        cancel.clone(),
    ));
    hours(1).await;
    assert_eq!(host.calls(), 1, "never checked: at once");
    hours(22).await;
    assert_eq!(host.calls(), 1, "not again within 24 h");
    hours(2).await;
    assert_eq!(host.calls(), 2, "once a day");
    hours(24).await;
    assert_eq!(host.calls(), 3);
    {
        let checked = host.checked.lock().expect("lock");
        let changes: Vec<bool> = checked.iter().map(|(_, c)| *c).collect();
        assert_eq!(changes, [true, false, false], "announced once");
    }
    cancel.cancel();
    task.await.expect("loop ends");
    hours(100).await;
    assert_eq!(host.calls(), 3, "nothing after cancel");
}

#[tokio::test(start_paused = true)]
async fn a_recent_check_from_the_cache_is_not_repeated() {
    let host = FakeHost::new(|| Ok(body("v0.3.1", URL)));
    let cache = UpdateCache {
        checked_at: Some(NOW - time::Duration::hours(20)),
        latest: Some("0.3.1".into()),
        url: Some(URL.into()),
        ..UpdateCache::default()
    };
    let cancel = CancellationToken::new();
    tokio::spawn(check_loop(host.clone(), cache, v("0.3.0"), cancel.clone()));
    hours(3).await;
    assert_eq!(host.calls(), 0, "the restart did not query again");
    hours(2).await;
    assert_eq!(host.calls(), 1);
    cancel.cancel();
}

#[tokio::test(start_paused = true)]
async fn a_failure_is_retried_after_an_hour_never_in_a_storm() {
    let host = FakeHost::new(|| {
        Err(UpdateError::Fetch {
            reason: "offline".into(),
        })
    });
    let cancel = CancellationToken::new();
    tokio::spawn(check_loop(
        host.clone(),
        UpdateCache::default(),
        v("0.3.0"),
        cancel.clone(),
    ));
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(host.calls(), 1);
    tokio::time::sleep(Duration::from_secs(58 * 60)).await;
    assert_eq!(host.calls(), 1, "not before an hour");
    tokio::time::sleep(Duration::from_secs(2 * 60)).await;
    assert_eq!(host.calls(), 2);
    hours(10).await;
    assert_eq!(host.calls(), 12, "one an hour");
    cancel.cancel();
}

// ── cache file ──────────────────────────────────────────────────────────

#[test]
fn the_cache_roundtrips_privately_and_is_validated_on_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("update.toml");
    assert_eq!(load_cache(&path), UpdateCache::default());
    let cache = UpdateCache {
        checked_at: Some(NOW),
        failed_at: None,
        latest: Some("0.3.1".into()),
        url: Some(URL.into()),
    };
    save_cache(&path, &cache).expect("save");
    assert_eq!(load_cache(&path), cache);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A hand-edited link or version is never shown.
    for (latest, url) in [
        ("9.9.9", "https://evil.example/portico"),
        ("9.9.9-rc.1", URL),
        ("nine", URL),
    ] {
        let edited = UpdateCache {
            latest: Some(latest.into()),
            url: Some(url.into()),
            ..cache.clone()
        };
        save_cache(&path, &edited).expect("save");
        assert_eq!(
            status(Some(true), &load_cache(&path), &v("0.3.0")).available,
            None,
            "{latest} {url}"
        );
    }
    // An unreadable file is an empty cache (one check sooner, no error).
    std::fs::write(&path, "checked_at = 12\n").expect("write");
    assert_eq!(load_cache(&path), UpdateCache::default());
}

// ── install method ──────────────────────────────────────────────────────

fn probe(exe: &str) -> InstallProbe {
    InstallProbe {
        exe: PathBuf::from(exe),
        ..InstallProbe::default()
    }
}

#[test]
fn the_install_method_comes_from_the_executable_and_the_system() {
    use Frontend::{Gui, Tui};
    use InstallMethod::{AppImage, Deb, Homebrew, MacApp, Other, Pacman};
    let pacman = |exe: &str, package: &str| InstallProbe {
        pacman_packages: vec![package.into()],
        ..probe(exe)
    };
    let cases: Vec<(InstallProbe, Frontend, InstallMethod)> = vec![
        (
            probe("/opt/homebrew/Cellar/portico/0.3.0/bin/portico"),
            Tui,
            Homebrew,
        ),
        (
            probe("/usr/local/Cellar/portico/0.3.0/bin/portico"),
            Tui,
            Homebrew,
        ),
        (
            probe("/home/linuxbrew/.linuxbrew/bin/portico"),
            Tui,
            Homebrew,
        ),
        (
            probe("/opt/homebrew/Caskroom/portico/0.3.0/Portico.app/Contents/MacOS/portico-gui"),
            Gui,
            Homebrew,
        ),
        (
            InstallProbe {
                homebrew_cask: true,
                ..probe("/Applications/Portico.app/Contents/MacOS/portico-gui")
            },
            Gui,
            Homebrew,
        ),
        (
            probe("/Applications/Portico.app/Contents/MacOS/portico-gui"),
            Gui,
            MacApp,
        ),
        (
            InstallProbe {
                appimage: true,
                ..probe("/tmp/.mount_PorticXYZ/usr/bin/portico-gui")
            },
            Gui,
            AppImage,
        ),
        (
            pacman("/usr/bin/portico", "portico-bin-0.3.0-1"),
            Tui,
            Pacman,
        ),
        (
            pacman("/usr/bin/portico-gui", "portico-gui-bin-0.3.0-1"),
            Gui,
            Pacman,
        ),
        // The other UI's package does not count, nor a binary elsewhere.
        (
            pacman("/usr/bin/portico-gui", "portico-bin-0.3.0-1"),
            Gui,
            Other,
        ),
        (
            pacman("/home/me/.cargo/bin/portico", "portico-bin-0.3.0-1"),
            Tui,
            Other,
        ),
        (
            InstallProbe {
                dpkg_lists_exe: true,
                ..probe("/usr/bin/portico-gui")
            },
            Gui,
            Deb,
        ),
        (probe("/home/me/.cargo/bin/portico"), Tui, Other),
        (
            probe("/home/me/src/Portico/target/release/portico"),
            Tui,
            Other,
        ),
        (probe(""), Gui, Other),
    ];
    for (probe, frontend, expected) in cases {
        assert_eq!(install_method(&probe, frontend), expected, "{probe:?}");
    }
}

#[test]
fn each_method_says_how_to_update() {
    use Frontend::{Gui, Tui};
    let brew = instruction(InstallMethod::Homebrew, Tui, "0.3.1");
    assert_eq!(
        brew.command.as_deref(),
        Some("brew upgrade fs0ciety7000/portico/portico")
    );
    assert_eq!(
        instruction(InstallMethod::Homebrew, Gui, "0.3.1")
            .command
            .as_deref(),
        Some("brew upgrade --cask fs0ciety7000/portico/portico")
    );
    let texts = [
        (
            InstallMethod::AppImage,
            "Download the new AppImage from the release page.",
        ),
        (
            InstallMethod::Pacman,
            "Rebuild the package: render.sh 0.3.1 then makepkg -si (see docs/RELEASE.md).",
        ),
        (
            InstallMethod::Deb,
            "Download the new .deb from the release page and install it.",
        ),
        (
            InstallMethod::MacApp,
            "Download the new .dmg from the release page.",
        ),
        (InstallMethod::Other, "Download it from the release page."),
    ];
    for (method, text) in texts {
        let shown = instruction(method, Gui, "0.3.1");
        assert_eq!(shown.text, text);
        assert_eq!(shown.command, None);
    }
}

// ── the real fetcher, against a local server ────────────────────────────

/// Serves one connection with `response`; returns the request received.
fn serve_once(response: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://{}/repos/fs0ciety7000/Portico/releases/latest",
        listener.local_addr().expect("addr")
    );
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).expect("read");
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        let _ = stream.write_all(&response);
        String::from_utf8(request).expect("utf-8")
    });
    (url, server)
}

fn http_response(status: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

#[test]
fn the_request_carries_only_the_user_agent_and_accept_headers() {
    let (url, server) = serve_once(http_response("200 OK", &body("v0.3.1", URL)));
    let fetched = GithubFetcher::local(url, Duration::from_secs(5))
        .fetch_latest()
        .expect("fetch");
    assert_eq!(
        parse_release(&fetched).expect("release").version,
        v("0.3.1")
    );
    let request = server.join().expect("server");
    let mut lines = request.split("\r\n").filter(|l| !l.is_empty());
    assert_eq!(
        lines.next(),
        Some("GET /repos/fs0ciety7000/Portico/releases/latest HTTP/1.1")
    );
    let mut headers: Vec<String> = lines
        .map(|l| {
            let (name, value) = l.split_once(':').expect("header");
            format!("{}: {}", name.to_ascii_lowercase(), value.trim())
        })
        .filter(|h| !h.starts_with("host: "))
        .collect();
    headers.sort();
    assert_eq!(
        headers,
        [
            "accept: application/vnd.github+json".to_owned(),
            format!("user-agent: Portico/{CURRENT_VERSION}"),
        ]
    );
}

#[test]
fn an_http_error_or_an_oversized_body_fails() {
    let (url, server) = serve_once(http_response("404 Not Found", b"{}"));
    let err = GithubFetcher::local(url, Duration::from_secs(5))
        .fetch_latest()
        .expect_err("404");
    assert_eq!(
        err,
        UpdateError::Fetch {
            reason: "GitHub answered HTTP 404".into()
        }
    );
    server.join().expect("server");

    let huge = vec![b' '; usize::try_from(MAX_RESPONSE_BYTES).expect("usize") + 1];
    let (url, server) = serve_once(http_response("200 OK", &huge));
    assert!(matches!(
        GithubFetcher::local(url, Duration::from_secs(5)).fetch_latest(),
        Err(UpdateError::Fetch { .. })
    ));
    server.join().expect("server");
}

#[test]
fn a_silent_server_times_out() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("addr"));
    let started = std::time::Instant::now();
    let err = GithubFetcher::local(url, Duration::from_millis(300)).fetch_latest();
    assert!(matches!(err, Err(UpdateError::Fetch { .. })), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(listener);
}

#[test]
fn the_real_fetcher_only_ever_targets_github_over_https() {
    let fetcher = GithubFetcher::new();
    assert_eq!(fetcher.url, RELEASES_API_URL);
    assert!(RELEASES_API_URL.starts_with("https://api.github.com/"));
    assert_eq!(fetcher.timeout, FETCH_TIMEOUT);
    assert!(fetcher.proxy_from_env);
    assert_eq!(NoNetwork.fetch_latest(), Err(UpdateError::Offline));
}

// ── opt-in, through the engine ──────────────────────────────────────────

/// Counts calls, then panics: the network must not be used.
#[derive(Debug, Default)]
struct MustNotFetch {
    calls: AtomicUsize,
}

impl ReleaseFetcher for MustNotFetch {
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("the update check reached the network without opt-in");
    }
}

/// Answers a newer release and reports each call.
#[derive(Debug)]
struct Canned {
    calls: AtomicUsize,
    tx: std::sync::mpsc::Sender<()>,
}

impl ReleaseFetcher for Canned {
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(());
        Ok(body("v99.0.0", URL))
    }
}

fn engine(root: &Path, fetcher: Arc<dyn ReleaseFetcher>) -> crate::Engine {
    crate::Engine::new(crate::EngineConfig {
        paths: crate::store::Paths::rooted(root),
        local_user: "alice".into(),
        event_capacity: 16,
        secrets: Arc::new(crate::secrets::MemoryStore::default()),
        agent_socket: None,
        connect_timeout: crate::engine::DEFAULT_CONNECT_TIMEOUT,
        keepalive: crate::engine::DEFAULT_KEEPALIVE,
        update_fetcher: fetcher,
    })
}

#[tokio::test(start_paused = true)]
async fn no_request_while_the_setting_is_absent_or_false() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fetcher = Arc::new(MustNotFetch::default());
    let engine = engine(dir.path(), fetcher.clone());

    // Absent: not asked yet.
    let status = engine.start_update_checks().await.expect("status");
    assert_eq!(status.check, None);
    assert!(!engine.update_checks_running());
    hours(72).await;

    // Declined ("Not now"): written false, still nothing.
    let status = engine.set_update_check(false).await.expect("off");
    assert_eq!(status.check, Some(false));
    let again = engine.start_update_checks().await.expect("status");
    assert_eq!(again.check, Some(false));
    assert!(!engine.update_checks_running());
    hours(72).await;

    // Unrelated settings changes do not turn it on.
    engine
        .set_theme(Some("basalt".into()), None)
        .await
        .expect("theme");
    engine.start_update_checks().await.expect("status");
    hours(72).await;

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("data/state/update.toml").exists());
}

#[tokio::test]
async fn turning_it_on_checks_announces_and_turning_it_off_stops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (tx, rx) = std::sync::mpsc::channel();
    let fetcher = Arc::new(Canned {
        calls: AtomicUsize::new(0),
        tx,
    });
    let engine = engine(dir.path(), fetcher.clone());
    let mut events = engine.subscribe();

    let status = engine.set_update_check(true).await.expect("on");
    assert_eq!(status.check, Some(true));
    assert!(engine.update_checks_running());
    tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
        .await
        .expect("join")
        .expect("one request");

    // The setting change, then the newer release.
    let mut found = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while found.is_none() {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("event in time")
            .expect("event");
        if let crate::EngineEvent::Update { status } = event
            && let Some(available) = status.available
        {
            found = Some(available);
        }
    }
    assert_eq!(found.expect("found").version, "99.0.0");
    let status = engine.update_status().await.expect("status");
    assert_eq!(
        status.available.map(|a| a.version).as_deref(),
        Some("99.0.0")
    );
    assert!(status.checked_at.is_some());
    // Within 24 h, a restart does not query again (the cache says so).
    let restarted = self::engine(dir.path(), fetcher.clone());
    restarted.start_update_checks().await.expect("restart");
    assert!(restarted.update_checks_running());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);

    let off = engine.set_update_check(false).await.expect("off");
    assert_eq!(off.check, Some(false));
    assert!(!engine.update_checks_running());
    // The known release stays shown (no new check is made).
    assert!(off.available.is_some());
    restarted.shutdown();
    assert!(!restarted.update_checks_running());
}

/// Lets the paused clock run until `done` (spawned blocking work included).
async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached in time");
}

fn canned() -> Arc<Canned> {
    let (tx, _rx) = std::sync::mpsc::channel();
    Arc::new(Canned {
        calls: AtomicUsize::new(0),
        tx,
    })
}

#[tokio::test(start_paused = true)]
async fn a_check_turned_off_elsewhere_is_never_made() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fetcher = canned();
    let a = engine(dir.path(), fetcher.clone());
    let b_fetcher = Arc::new(MustNotFetch::default());
    let b = engine(dir.path(), b_fetcher.clone());

    a.set_update_check(true).await.expect("on in A");
    until(|| fetcher.calls.load(Ordering::SeqCst) == 1).await;
    // Another Portico (B, same config) turns it off: A is not told.
    b.set_update_check(false).await.expect("off in B");
    assert!(!b.update_checks_running());
    hours(72).await;
    until(|| !a.update_checks_running()).await;
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "no request after off"
    );
    assert_eq!(b_fetcher.calls.load(Ordering::SeqCst), 0);
    // Turning it on again in A starts a fresh task.
    a.set_update_check(true).await.expect("on again");
    assert!(a.update_checks_running());
    a.shutdown();
}

#[tokio::test(start_paused = true)]
async fn a_check_turned_off_by_hand_is_never_made() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fetcher = canned();
    let a = engine(dir.path(), fetcher.clone());

    a.set_update_check(true).await.expect("on");
    until(|| fetcher.calls.load(Ordering::SeqCst) == 1).await;
    let settings = crate::store::Paths::rooted(dir.path()).settings();
    std::fs::write(&settings, "[updates]\ncheck = false\n").expect("hand edit");
    hours(72).await;
    until(|| !a.update_checks_running()).await;
    assert_eq!(
        fetcher.calls.load(Ordering::SeqCst),
        1,
        "no request after off"
    );
}
