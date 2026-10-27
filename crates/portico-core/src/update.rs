//! Opt-in update check: is a newer Portico release out? (0.3.0, phase 9.)
//!
//! Portico only **notifies**: it never downloads nor runs anything. The
//! UIs show `0.3.1 available` and how to update this install
//! ([`install_instruction`]).
//!
//! ## Invariants (ADR "Vérification des mises à jour", `docs/STACK.md`)
//!
//! - **No network unless opted in.** `[updates] check` in `settings.toml`
//!   is `None` until the user answers the UIs' one-time question, `false`
//!   when they declined: in both cases the check task never starts and the
//!   [`ReleaseFetcher`] is never called (tested with a fetcher that panics).
//! - **One request**, at most once per [`CHECK_INTERVAL`] (or
//!   [`RETRY_AFTER_FAILURE`] after a failure), even across restarts (the
//!   last attempt is kept in `state/update.toml`): `GET` [`RELEASES_API_URL`]
//!   (a constant) with `User-Agent: Portico/<version>` (GitHub requires one;
//!   it tells GitHub the running version) and
//!   `Accept: application/vnd.github+json`. No cookie, no other header, no
//!   identifier. Timeout [`FETCH_TIMEOUT`], body capped at
//!   [`MAX_RESPONSE_BYTES`], HTTPS only, system roots. The proxy comes from the environment
//!   (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, upper or lower case, with
//!   `NO_PROXY`): a plain-HTTP proxy only sees a `CONNECT`, TLS to GitHub
//!   stays end to end.
//! - **The answer is data.** Only `tag_name` (`vX.Y.Z`), `html_url` (must
//!   start with [`RELEASE_PAGE_PREFIX`]), `draft` and `prerelease` are read;
//!   anything else is ignored, and the cache is validated again when read
//!   (a hand-edited file cannot inject a link).
//! - **Silent failures**: a failed check is logged at debug level (no
//!   secret involved) and retried after an hour; the UIs show nothing.
//! - The blocking HTTP call runs on `spawn_blocking`, never on the runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::store;

/// The only URL the check contacts.
pub const RELEASES_API_URL: &str =
    "https://api.github.com/repos/fs0ciety7000/Portico/releases/latest";
/// A release page link must start with this (anything else is ignored).
pub const RELEASE_PAGE_PREFIX: &str = "https://github.com/fs0ciety7000/Portico/releases/";
/// Time between two successful checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Time before trying again after a failed check.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60 * 60);
/// Whole-request timeout (connect, TLS, headers, body).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest response body read.
pub const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// The running version (the workspace version).
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// `Accept` header of the request.
pub const ACCEPT: &str = "application/vnd.github+json";
/// Longest release link kept, in bytes.
const MAX_URL_BYTES: usize = 256;
/// Longest version tag accepted, in bytes.
const MAX_TAG_BYTES: usize = 32;
/// Largest `update.toml` read, in bytes.
const MAX_CACHE_FILE_BYTES: u64 = 16 * 1024;
/// Redirects followed (GitHub redirects a renamed repository, on its API host).
const MAX_REDIRECTS: u32 = 3;

/// `User-Agent` of the request: `Portico/<version>`.
pub fn user_agent() -> String {
    format!("Portico/{CURRENT_VERSION}")
}

/// A failed check. Messages never hold a secret (there is none involved).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// Network, TLS, timeout, HTTP status or oversized body.
    #[error("update check failed: {reason}")]
    Fetch { reason: String },
    /// The answer is not a usable release (and is ignored).
    #[error("ignored release data: {reason}")]
    Release { reason: &'static str },
    /// The fetcher of tests and offline builds: never any network.
    #[error("update checks have no network access here")]
    Offline,
}

impl UpdateError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Fetch { .. } => (
                "update_fetch",
                Some("Portico tries again in an hour; nothing to do."),
            ),
            Self::Release { .. } => ("update_release", None),
            Self::Offline => ("update_offline", None),
        }
    }
}

// ── fetching ────────────────────────────────────────────────────────────

/// Fetches the latest release document. **Blocking**: the engine calls it
/// on `spawn_blocking`. Tests inject their own (no network in tests).
pub trait ReleaseFetcher: Send + Sync + std::fmt::Debug {
    /// The response body of `GET` [`RELEASES_API_URL`] (at most
    /// [`MAX_RESPONSE_BYTES`]).
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError>;
}

/// The real fetcher: one HTTPS `GET` to [`RELEASES_API_URL`] (`ureq`,
/// rustls on the aws-lc-rs provider, system roots).
#[derive(Debug, Clone)]
pub struct GithubFetcher {
    url: String,
    timeout: Duration,
    /// Proxy from the environment (`ureq::Proxy::try_from_env`: `ALL_PROXY`,
    /// `HTTPS_PROXY`, `HTTP_PROXY`, upper or lower case, with `NO_PROXY`;
    /// TLS to GitHub stays end to end through it); tests pass none.
    proxy_from_env: bool,
}

impl GithubFetcher {
    pub fn new() -> Self {
        Self {
            url: RELEASES_API_URL.to_owned(),
            timeout: FETCH_TIMEOUT,
            proxy_from_env: true,
        }
    }

    /// A local test server instead of GitHub (plain HTTP allowed, no proxy).
    #[cfg(test)]
    fn local(url: String, timeout: Duration) -> Self {
        Self {
            url,
            timeout,
            proxy_from_env: false,
        }
    }

    fn agent(&self) -> ureq::Agent {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = TlsConfig::builder()
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(provider)
            .build();
        let proxy = if self.proxy_from_env {
            ureq::Proxy::try_from_env()
        } else {
            None
        };
        ureq::Agent::config_builder()
            .https_only(self.url.starts_with("https://"))
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .max_redirects(MAX_REDIRECTS)
            .save_redirect_history(false)
            .user_agent(user_agent())
            .accept(ACCEPT)
            .proxy(proxy)
            .tls_config(tls)
            .build()
            .new_agent()
    }
}

impl Default for GithubFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ReleaseFetcher for GithubFetcher {
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError> {
        let fail = |reason: String| UpdateError::Fetch { reason };
        let mut response = self
            .agent()
            .get(&self.url)
            .call()
            .map_err(|e| fail(e.to_string()))?;
        let status = response.status();
        if status != ureq::http::StatusCode::OK {
            return Err(fail(format!("GitHub answered HTTP {}", status.as_u16())));
        }
        response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|e| fail(e.to_string()))
    }
}

/// A fetcher that never reaches the network (tests, and engines built
/// without one): every call is [`UpdateError::Offline`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoNetwork;

impl ReleaseFetcher for NoNetwork {
    fn fetch_latest(&self) -> Result<Vec<u8>, UpdateError> {
        Err(UpdateError::Offline)
    }
}

/// The real fetcher, shared.
pub fn github() -> Arc<dyn ReleaseFetcher> {
    Arc::new(GithubFetcher::new())
}

/// [`NoNetwork`], shared: for `EngineConfig`s built in tests.
pub fn no_network() -> Arc<dyn ReleaseFetcher> {
    Arc::new(NoNetwork)
}

// ── parsing ─────────────────────────────────────────────────────────────

/// A published release, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Release {
    pub(crate) version: semver::Version,
    pub(crate) url: String,
}

/// The fields read from GitHub's answer (every other one is ignored).
#[derive(Debug, Deserialize)]
struct ReleaseJson {
    tag_name: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `vX.Y.Z` (or `X.Y.Z`): a plain release version, no pre-release or
/// build part.
pub(crate) fn parse_tag(tag: &str) -> Option<semver::Version> {
    if tag.len() > MAX_TAG_BYTES {
        return None;
    }
    let version = semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

/// A link under [`RELEASE_PAGE_PREFIX`], made of URL-path characters only.
pub(crate) fn valid_release_url(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES
        && url.strip_prefix(RELEASE_PAGE_PREFIX).is_some_and(|rest| {
            !rest.is_empty()
                && !rest.contains("..")
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        })
}

/// Reads GitHub's `releases/latest` answer.
pub(crate) fn parse_release(body: &[u8]) -> Result<Release, UpdateError> {
    let bad = |reason| UpdateError::Release { reason };
    let json: ReleaseJson = serde_json::from_slice(body).map_err(|_| bad("not a release"))?;
    if json.draft || json.prerelease {
        return Err(bad("draft or pre-release"));
    }
    let version = json
        .tag_name
        .as_deref()
        .and_then(parse_tag)
        .ok_or(bad("tag is not vX.Y.Z"))?;
    let url = json
        .html_url
        .filter(|u| valid_release_url(u))
        .ok_or(bad("link is not a Portico release page"))?;
    Ok(Release { version, url })
}

// ── cache (`state/update.toml`) ─────────────────────────────────────────

/// The last check, kept across restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateCache {
    /// Last successful check (RFC 3339).
    #[serde(with = "time::serde::rfc3339::option")]
    pub(crate) checked_at: Option<OffsetDateTime>,
    /// Last failed attempt, when more recent than `checked_at`.
    #[serde(with = "time::serde::rfc3339::option")]
    pub(crate) failed_at: Option<OffsetDateTime>,
    /// Latest release seen (`X.Y.Z`) and its page.
    pub(crate) latest: Option<String>,
    pub(crate) url: Option<String>,
}

impl UpdateCache {
    /// The release it holds, validated again (the file may be edited).
    pub(crate) fn release(&self) -> Option<Release> {
        let version = parse_tag(self.latest.as_deref()?)?;
        let url = self.url.clone().filter(|u| valid_release_url(u))?;
        Some(Release { version, url })
    }

    /// The release it holds, when newer than `current`.
    pub(crate) fn newer_than(&self, current: &semver::Version) -> Option<Release> {
        self.release().filter(|r| r.version > *current)
    }
}

/// Reads the cache; a missing, unreadable or invalid file is an empty one
/// (worst case: one check sooner). Blocking.
pub(crate) fn load_cache(path: &Path) -> UpdateCache {
    store::load_toml_capped(path, MAX_CACHE_FILE_BYTES).unwrap_or_else(|e| {
        tracing::debug!(error = %e, "update cache ignored");
        UpdateCache::default()
    })
}

/// Writes the cache atomically (`0600`). Blocking.
pub(crate) fn save_cache(path: &Path, cache: &UpdateCache) -> crate::Result<()> {
    let text = toml::to_string_pretty(cache).map_err(|e| crate::Error::Serde {
        what: "toml",
        message: e.to_string(),
    })?;
    store::write_atomic_capped(path, text.as_bytes(), MAX_CACHE_FILE_BYTES)
}

// ── status ──────────────────────────────────────────────────────────────

/// What the UIs show: the setting, and a newer release if one is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// `[updates] check`: `None` = not asked yet (the UIs ask once).
    pub check: Option<bool>,
    /// The running version.
    pub current: String,
    /// A newer release, known from the last check (kept while the setting
    /// is off: no new check is made, the known one stays shown).
    pub available: Option<AvailableUpdate>,
    /// Last successful check.
    #[serde(with = "time::serde::rfc3339::option")]
    pub checked_at: Option<OffsetDateTime>,
}

/// A release newer than the running version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableUpdate {
    /// `X.Y.Z`.
    pub version: String,
    /// Its release page (validated: under [`RELEASE_PAGE_PREFIX`]).
    pub url: String,
}

pub(crate) fn current_version() -> semver::Version {
    semver::Version::parse(CURRENT_VERSION).unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

pub(crate) fn status(
    check: Option<bool>,
    cache: &UpdateCache,
    current: &semver::Version,
) -> UpdateStatus {
    UpdateStatus {
        check,
        current: current.to_string(),
        available: cache.newer_than(current).map(|r| AvailableUpdate {
            version: r.version.to_string(),
            url: r.url,
        }),
        checked_at: cache.checked_at,
    }
}

// ── scheduling ──────────────────────────────────────────────────────────

fn to_std(d: time::Duration) -> Duration {
    Duration::try_from(d).unwrap_or(Duration::ZERO)
}

fn from_std(d: Duration) -> time::Duration {
    time::Duration::try_from(d).unwrap_or(time::Duration::ZERO)
}

/// How long to wait before the next check: due [`CHECK_INTERVAL`] after
/// the last success, [`RETRY_AFTER_FAILURE`] after a later failure, now
/// when never checked. Never more than [`CHECK_INTERVAL`] (a date in the
/// future, after a clock change, cannot stop the checks).
pub(crate) fn next_delay(cache: &UpdateCache, now: OffsetDateTime) -> Duration {
    let due = match (cache.checked_at, cache.failed_at) {
        (Some(checked), Some(failed)) if failed <= checked => checked + from_std(CHECK_INTERVAL),
        (_, Some(failed)) => failed + from_std(RETRY_AFTER_FAILURE),
        (Some(checked), None) => checked + from_std(CHECK_INTERVAL),
        (None, None) => now,
    };
    to_std(due - now).min(CHECK_INTERVAL)
}

/// What the check loop needs from its surroundings (the engine, or a test).
pub(crate) trait CheckHost: Send + Sync + 'static {
    fn now(&self) -> OffsetDateTime;
    /// The persisted setting is still `check = true` (read again before
    /// every request: another process, or the user by hand, may have
    /// turned it off since the loop started).
    fn enabled(&self) -> impl Future<Output = bool> + Send;
    /// One request (on `spawn_blocking` in the engine).
    fn fetch(&self) -> impl Future<Output = Result<Vec<u8>, UpdateError>> + Send;
    /// A check ended: persist `cache`; `changed`: the newer release shown
    /// is not the same as before.
    fn checked(&self, cache: &UpdateCache, changed: bool) -> impl Future<Output = ()> + Send;
}

/// Applies one fetch outcome to `cache`; returns whether the newer release
/// shown changed.
pub(crate) fn record(
    cache: &mut UpdateCache,
    outcome: Result<Vec<u8>, UpdateError>,
    now: OffsetDateTime,
    current: &semver::Version,
) -> bool {
    let before = cache.newer_than(current);
    match outcome.and_then(|body| parse_release(&body)) {
        Ok(release) => {
            cache.checked_at = Some(now);
            cache.failed_at = None;
            cache.latest = Some(release.version.to_string());
            cache.url = Some(release.url);
        }
        // An answer that is not a usable release: checked, nothing new.
        Err(e @ UpdateError::Release { .. }) => {
            tracing::debug!(error = %e, "update check: release ignored");
            cache.checked_at = Some(now);
            cache.failed_at = None;
        }
        Err(e) => {
            tracing::debug!(error = %e, "update check failed");
            cache.failed_at = Some(now);
        }
    }
    cache.newer_than(current) != before
}

/// Checks when due, then every [`CHECK_INTERVAL`] (an hour after a
/// failure), until `cancel`, or until the persisted setting is no longer
/// `check = true` (re-read right before each request).
pub(crate) async fn check_loop(
    host: impl CheckHost,
    mut cache: UpdateCache,
    current: semver::Version,
    cancel: CancellationToken,
) {
    loop {
        let wait = next_delay(&cache, host.now());
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        let enabled = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            enabled = host.enabled() => enabled,
        };
        if !enabled {
            tracing::debug!("update check turned off elsewhere: the periodic check stops");
            return;
        }
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            outcome = host.fetch() => outcome,
        };
        let changed = record(&mut cache, outcome, host.now(), &current);
        host.checked(&cache, changed).await;
    }
}

// ── how to update this install ──────────────────────────────────────────

/// Which UI asks (the Homebrew command differs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    Tui,
    Gui,
}

/// How this copy of Portico was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InstallMethod {
    Homebrew,
    AppImage,
    Pacman,
    Deb,
    MacApp,
    /// Windows: an app of the Scoop bucket (`scoop\apps\portico…\`).
    Scoop,
    /// Windows: the per-user NSIS installer, also what winget installs
    /// (`%LOCALAPPDATA%\Portico\`).
    WindowsInstaller,
    /// Windows: `portico.exe` unpacked from the release `.zip` (TUI).
    WindowsZip,
    Other,
}

/// What a UI shows under "Portico X is available".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateInstruction {
    pub method: InstallMethod,
    /// A sentence, such as "Download it from the release page.".
    pub text: String,
    /// A command to copy, when there is one (Homebrew).
    pub command: Option<String>,
}

/// Facts about this install, gathered by [`InstallProbe::detect`] (pure
/// input of [`install_method`], so it is tested without a real system).
// Independent yes/no facts gathered from the system, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallProbe {
    /// The executable (canonical when possible).
    pub exe: PathBuf,
    /// `APPIMAGE` is set.
    pub appimage: bool,
    /// Entries of `/var/lib/pacman/local` starting with `portico`.
    pub pacman_packages: Vec<String>,
    /// A `/var/lib/dpkg/info/portico*.list` lists the executable.
    pub dpkg_lists_exe: bool,
    /// A Homebrew `Caskroom/portico` exists.
    pub homebrew_cask: bool,
    /// Running on Windows.
    pub windows: bool,
    /// `%LOCALAPPDATA%` (Windows only).
    pub local_app_data: Option<PathBuf>,
}

/// Homebrew prefixes (macOS Apple silicon, Intel, Linux).
const HOMEBREW_PREFIXES: &[&str] = &["/opt/homebrew/", "/home/linuxbrew/.linuxbrew/"];
/// `Caskroom` directories holding a `portico` cask.
const CASKROOMS: &[&str] = &[
    "/opt/homebrew/Caskroom/portico",
    "/usr/local/Caskroom/portico",
];
const PACMAN_LOCAL: &str = "/var/lib/pacman/local";
const DPKG_INFO: &str = "/var/lib/dpkg/info";
/// Largest dpkg file list read.
const MAX_DPKG_LIST_BYTES: u64 = 1024 * 1024;

impl InstallProbe {
    /// Looks at this process and system. Blocking (small reads).
    pub fn detect() -> Self {
        let exe = std::env::current_exe()
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .unwrap_or_default();
        let pacman_packages = std::fs::read_dir(PACMAN_LOCAL)
            .map(|dir| {
                dir.filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter(|n| n.starts_with("portico"))
                    .collect()
            })
            .unwrap_or_default();
        let dpkg_lists_exe = dpkg_lists(&exe);
        Self {
            appimage: std::env::var_os("APPIMAGE").is_some_and(|v| !v.is_empty()),
            pacman_packages,
            dpkg_lists_exe,
            homebrew_cask: CASKROOMS.iter().any(|d| Path::new(d).is_dir()),
            exe,
            windows: cfg!(windows),
            local_app_data: if cfg!(windows) {
                std::env::var_os("LOCALAPPDATA")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            } else {
                None
            },
        }
    }
}

fn dpkg_lists(exe: &Path) -> bool {
    let Some(exe) = exe.to_str().filter(|e| !e.is_empty()) else {
        return false;
    };
    let Ok(dir) = std::fs::read_dir(DPKG_INFO) else {
        return false;
    };
    dir.filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "list")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("portico"))
        })
        .any(|p| {
            store::read_capped(&p, MAX_DPKG_LIST_BYTES)
                .ok()
                .flatten()
                .is_some_and(|text| text.lines().any(|line| line == exe))
        })
}

/// A Windows path compared case-insensitively, `/` separators, no `\\?\`
/// prefix (what `canonicalize` adds).
fn windows_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").to_lowercase();
    text.strip_prefix("//?/").map(str::to_owned).unwrap_or(text)
}

/// Windows installs (pure: see [`InstallProbe`]).
fn windows_install_method(probe: &InstallProbe) -> InstallMethod {
    let exe = windows_key(&probe.exe);
    if ["/scoop/apps/portico/", "/scoop/apps/portico-gui/"]
        .iter()
        .any(|p| exe.contains(p))
    {
        return InstallMethod::Scoop;
    }
    if let Some(local) = &probe.local_app_data {
        let prefix = format!("{}/portico/", windows_key(local).trim_end_matches('/'));
        if exe.starts_with(&prefix) {
            return InstallMethod::WindowsInstaller;
        }
    }
    if exe.ends_with("/portico.exe") {
        return InstallMethod::WindowsZip;
    }
    InstallMethod::Other
}

/// How this copy was installed (pure: see [`InstallProbe`]).
pub fn install_method(probe: &InstallProbe, frontend: Frontend) -> InstallMethod {
    if probe.windows {
        return windows_install_method(probe);
    }
    let exe = probe.exe.to_string_lossy();
    if probe.appimage {
        return InstallMethod::AppImage;
    }
    let in_app_bundle = exe.starts_with("/Applications/") && exe.contains(".app/Contents/MacOS/");
    if exe.contains("/Cellar/")
        || exe.contains("/Caskroom/")
        || HOMEBREW_PREFIXES.iter().any(|p| exe.starts_with(p))
        || (in_app_bundle && probe.homebrew_cask)
    {
        return InstallMethod::Homebrew;
    }
    let package = match frontend {
        Frontend::Tui => "portico-bin-",
        Frontend::Gui => "portico-gui-bin-",
    };
    if exe.starts_with("/usr/bin/") && probe.pacman_packages.iter().any(|p| p.starts_with(package))
    {
        return InstallMethod::Pacman;
    }
    if probe.dpkg_lists_exe {
        return InstallMethod::Deb;
    }
    if in_app_bundle {
        return InstallMethod::MacApp;
    }
    InstallMethod::Other
}

/// What to tell the user for `method`, updating to `version`.
pub fn instruction(method: InstallMethod, frontend: Frontend, version: &str) -> UpdateInstruction {
    let (text, command) = match method {
        InstallMethod::Homebrew => (
            "Update with Homebrew:".to_owned(),
            Some(match frontend {
                Frontend::Tui => "brew upgrade fs0ciety7000/portico/portico",
                Frontend::Gui => "brew upgrade --cask fs0ciety7000/portico/portico",
            }),
        ),
        InstallMethod::AppImage => (
            "Download the new AppImage from the release page.".to_owned(),
            None,
        ),
        InstallMethod::Pacman => (
            format!(
                "Rebuild the package: render.sh {version} then makepkg -si (see docs/RELEASE.md)."
            ),
            None,
        ),
        InstallMethod::Deb => (
            "Download the new .deb from the release page and install it.".to_owned(),
            None,
        ),
        InstallMethod::MacApp => (
            "Download the new .dmg from the release page.".to_owned(),
            None,
        ),
        InstallMethod::Scoop => (
            "Update with Scoop:".to_owned(),
            Some(match frontend {
                Frontend::Tui => "scoop update portico",
                Frontend::Gui => "scoop update portico-gui",
            }),
        ),
        InstallMethod::WindowsInstaller => (
            "Download the new installer from the release page, or with winget:".to_owned(),
            Some("winget upgrade fs0ciety7000.Portico"),
        ),
        InstallMethod::WindowsZip => (
            "Download the new .zip from the release page and replace portico.exe.".to_owned(),
            None,
        ),
        InstallMethod::Other => ("Download it from the release page.".to_owned(), None),
    };
    UpdateInstruction {
        method,
        text,
        command: command.map(str::to_owned),
    }
}

/// [`InstallProbe::detect`] + [`install_method`] + [`instruction`].
/// Blocking.
pub fn install_instruction(frontend: Frontend, version: &str) -> UpdateInstruction {
    instruction(
        install_method(&InstallProbe::detect(), frontend),
        frontend,
        version,
    )
}

#[cfg(test)]
mod tests;
