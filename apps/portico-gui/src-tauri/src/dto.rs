//! Serializable views sent to the webview (camelCase for TypeScript).

use std::path::{Path, PathBuf};

use portico_core::engine::{PromptResponse, PtySize};
use portico_core::hosts::{HostEntry, HostOrigin};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostDto {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: u16,
    pub proxy_jump: Vec<String>,
    pub identity_files: Vec<String>,
    pub local_forwards: Vec<String>,
    pub remote_forwards: Vec<String>,
    /// `DynamicForward` values (`-D` SOCKS proxies), offered in Tunnels.
    pub dynamic_forwards: Vec<String>,
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    /// Free text from `hosts.toml` (validated by the core: `\n` is its only
    /// control character, no bidi or invisible character).
    pub notes: Option<String>,
    pub source: String,
    /// Which file declares the host: only `portico` hosts are editable.
    pub origin: HostOrigin,
    pub warnings: Vec<String>,
}

impl From<&HostEntry> for HostDto {
    fn from(h: &HostEntry) -> Self {
        let r = &h.resolved;
        Self {
            alias: h.alias.clone(),
            hostname: r.hostname.clone(),
            user: r.user.clone(),
            port: r.port,
            proxy_jump: r.proxy_jump.iter().map(|j| j.host.clone()).collect(),
            identity_files: r.identity_files.iter().map(|p| display_path(p)).collect(),
            local_forwards: r.local_forwards.clone(),
            remote_forwards: r.remote_forwards.clone(),
            dynamic_forwards: r.dynamic_forwards.clone(),
            tags: h.meta.tags.clone(),
            group: h.meta.group.clone(),
            favorite: h.meta.favorite,
            notes: h.meta.notes.clone(),
            source: format!("{}:{}", display_path(&h.source.file), h.source.line),
            origin: h.origin,
            warnings: r.warnings.iter().map(|w| w.message.clone()).collect(),
        }
    }
}

/// `~/…` for paths under the home directory (shorter, and does not expose
/// the account's home path in screenshots).
pub(crate) fn display_path(path: &Path) -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| {
            path.strip_prefix(home)
                .ok()
                .map(|rest| format!("~/{}", rest.display()))
        })
        .unwrap_or_else(|| path.display().to_string())
}

/// Text that may come from a server or a hand-edited file (log messages,
/// error summaries, config values), made safe to display on one line: the
/// same escapes as `LogRecord::to_line` (`\n`, `\r`, `\t`, then `\u{…}` for
/// other control characters and for bidi or invisible format characters),
/// and capped at `max_chars` characters (`…` marks a cut).
pub(crate) fn escape_display(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars.saturating_mul(4)));
    for (count, c) in text.chars().enumerate() {
        if count >= max_chars {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || is_hidden_format(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

/// Bidi controls, invisible format characters and line or paragraph
/// separators (the set `portico-core` refuses in single-line fields).
fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{00ad}'
            | '\u{200b}'
            | '\u{200c}'
            | '\u{2060}'..='\u{2064}'
            | '\u{feff}'
            | '\u{e0000}'..='\u{e007f}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// An `ErrorSummary` whose texts went through [`escape_display`].
pub(crate) fn safe_summary(e: &portico_core::ErrorSummary) -> portico_core::ErrorSummary {
    portico_core::ErrorSummary {
        code: escape_display(&e.code, 64),
        message: escape_display(&e.message, 512),
        hint: e.hint.as_deref().map(|h| escape_display(h, 512)),
    }
}

/// Unix time in milliseconds (the webview formats it in local time).
pub(crate) fn unix_ms(at: time::OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThemeDto {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

/// Terminal size from the view: cells, and pixels when known (0 otherwise).
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct PtySizeDto {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

impl From<PtySizeDto> for PtySize {
    fn from(s: PtySizeDto) -> Self {
        Self {
            cols: s.cols.max(1),
            rows: s.rows.max(1),
            pixel_width: s.width,
            pixel_height: s.height,
        }
    }
}

/// Answer to a prompt, as sent by the webview.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PromptAnswerDto {
    AcceptHostKey,
    // Deserialized straight into `SecretString`: no intermediate `String`.
    Secret { value: SecretString, remember: bool },
    Answers { values: Vec<SecretString> },
    Cancel,
}

// Manual impl: the derived one would print secrets.
impl std::fmt::Debug for PromptAnswerDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::AcceptHostKey => "accept_host_key",
            Self::Secret { .. } => "secret",
            Self::Answers { .. } => "answers",
            Self::Cancel => "cancel",
        };
        f.debug_struct("PromptAnswerDto")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

impl From<PromptAnswerDto> for PromptResponse {
    fn from(dto: PromptAnswerDto) -> Self {
        match dto {
            PromptAnswerDto::AcceptHostKey => Self::AcceptHostKey,
            PromptAnswerDto::Secret { value, remember } => Self::Secret { value, remember },
            PromptAnswerDto::Answers { values } => Self::Answers(values),
            PromptAnswerDto::Cancel => Self::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_paths_are_shortened() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            display_path(&Path::new(&home).join(".ssh/config")),
            "~/.ssh/config"
        );
        assert_eq!(
            display_path(Path::new("/etc/ssh/ssh_config")),
            "/etc/ssh/ssh_config"
        );
    }

    #[test]
    fn display_text_is_escaped_like_log_lines() {
        assert_eq!(
            escape_display("a\nb\t\u{1b}[31m\u{202e}gpj.exe", 100),
            "a\\nb\\t\\u{1b}[31m\\u{202e}gpj.exe"
        );
        assert_eq!(
            escape_display("zero\u{200b}width", 100),
            "zero\\u{200b}width"
        );
        assert_eq!(escape_display("plain text, é ü", 100), "plain text, é ü");
        assert_eq!(escape_display("abcdef", 3), "abc…");
        let summary = safe_summary(&portico_core::ErrorSummary {
            code: "auth_failed".into(),
            message: "server said\r\nfake prompt".into(),
            hint: Some("\u{2066}hint".into()),
        });
        assert_eq!(summary.message, "server said\\r\\nfake prompt");
        assert_eq!(summary.hint.as_deref(), Some("\\u{2066}hint"));
    }

    #[test]
    fn prompt_answer_dto_debug_is_redacted() {
        let dto: PromptAnswerDto =
            serde_json::from_str(r#"{"kind":"secret","value":"hunter2","remember":true}"#).unwrap();
        assert!(!format!("{dto:?}").contains("hunter2"));
        let PromptResponse::Secret { value, remember } = dto.into() else {
            panic!("expected a secret");
        };
        assert!(remember);
        assert_eq!(secrecy::ExposeSecret::expose_secret(&value), "hunter2");
    }
}
