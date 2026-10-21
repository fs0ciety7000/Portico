//! Debug panel (phase 4): session logs and effective configuration.
//!
//! Log messages, error summaries and config values can carry text chosen by
//! a server or written in a config file. Everything shown here goes through
//! [`escape_display`] (the escapes of `LogRecord::to_line`): no control,
//! bidi or invisible character reaches the webview, and each field is one
//! line. "Copy all" is built here too, from `to_line`, so the copied text is
//! exactly the stable form used in bug reports.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use portico_core::effective::{EffectiveConfig, OptionOrigin};
use portico_core::events::CloseReason;
use portico_core::ids::SessionId;
use portico_core::logs::{LogLevel, LogRecord, SessionLogInfo, SessionOutcome};
use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::State;

use crate::dto::{display_path, escape_display, safe_summary, unix_ms};

/// Longest log message or config value shown (the full text is in "Copy all").
const DISPLAY_MESSAGE: usize = 2000;
/// Longest log target, alias or config keyword shown.
const DISPLAY_SHORT: usize = 128;

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

/// A session whose logs can be read (live, or among the last ended ones).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionLogDto {
    pub id: u64,
    pub alias: String,
    pub probe: bool,
    /// Unix milliseconds.
    pub started: i64,
    pub ended: Option<i64>,
    pub outcome: Option<OutcomeDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OutcomeDto {
    Closed { reason: CloseReason },
    Failed { error: ErrorSummary },
}

impl From<&SessionLogInfo> for SessionLogDto {
    fn from(info: &SessionLogInfo) -> Self {
        Self {
            id: info.id.get(),
            alias: escape_display(&info.alias, DISPLAY_SHORT),
            probe: info.probe,
            started: unix_ms(info.started),
            ended: info.ended.map(unix_ms),
            outcome: info.outcome.as_ref().map(|o| match o {
                SessionOutcome::Closed { reason } => OutcomeDto::Closed {
                    reason: reason.clone(),
                },
                SessionOutcome::Failed { error } => OutcomeDto::Failed {
                    error: safe_summary(error),
                },
            }),
        }
    }
}

/// One log record, ready to display.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogLineDto {
    /// Unix milliseconds.
    pub at: i64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

impl From<&LogRecord> for LogLineDto {
    fn from(r: &LogRecord) -> Self {
        Self {
            at: unix_ms(r.at),
            level: r.level,
            target: escape_display(&r.target, DISPLAY_SHORT),
            message: escape_display(&r.message, DISPLAY_MESSAGE),
        }
    }
}

/// Live sessions and the last ended ones, oldest first.
#[tauri::command]
pub(crate) fn sessions_with_logs(engine: State<'_, Engine>) -> Vec<SessionLogDto> {
    engine
        .sessions_with_logs()
        .iter()
        .map(SessionLogDto::from)
        .collect()
}

#[tauri::command]
pub(crate) fn session_logs(
    engine: State<'_, Engine>,
    session: u64,
) -> Result<Vec<LogLineDto>, ErrorSummary> {
    let records = engine
        .session_logs(SessionId::from_raw(session))
        .map_err(summary)?;
    Ok(records.iter().map(LogLineDto::from).collect())
}

/// Every record of the session as text (`LogRecord::to_line`, one per line),
/// for "Copy all".
#[tauri::command]
pub(crate) fn session_logs_text(
    engine: State<'_, Engine>,
    session: u64,
) -> Result<String, ErrorSummary> {
    let records = engine
        .session_logs(SessionId::from_raw(session))
        .map_err(summary)?;
    Ok(logs_text(&records))
}

fn logs_text(records: &[LogRecord]) -> String {
    let mut text = String::new();
    for record in records {
        text.push_str(&record.to_line());
        text.push('\n');
    }
    text
}

/// The `ssh -G` equivalent, display-safe.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EffectiveConfigDto {
    pub alias: String,
    pub options: Vec<EffectiveOptionDto>,
    pub route: Vec<RouteHopDto>,
    pub route_error: Option<ErrorSummary>,
    pub warnings: Vec<EffectiveWarningDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EffectiveOptionDto {
    pub key: String,
    pub values: Vec<String>,
    /// `~/.ssh/config:12`, or `None` for Portico's default.
    pub origin: Option<String>,
    /// Given at connect time (`-l` user override…).
    pub command: bool,
    /// A keyword that runs local or remote commands (`ProxyCommand`,
    /// `LocalCommand`, `RemoteCommand`, `KnownHostsCommand`): its values are
    /// never sent to the webview (`values` is empty), so they cannot be
    /// shown by accident nor end up in a copied bug report.
    pub sensitive: bool,
}

/// Keywords whose values are withheld (same list as the core's
/// `EffectiveOption::sensitive`, checked here too so the GUI never relies
/// on a field it cannot see in older cores).
const SENSITIVE_KEYS: [&str; 4] = [
    "proxycommand",
    "localcommand",
    "remotecommand",
    "knownhostscommand",
];

fn is_sensitive(key: &str) -> bool {
    SENSITIVE_KEYS.iter().any(|k| key.eq_ignore_ascii_case(k))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteHopDto {
    pub alias: String,
    pub user: String,
    pub hostname: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EffectiveWarningDto {
    /// `~/.ssh/config:12`.
    pub origin: String,
    pub message: String,
}

fn file_line(file: &str, line: usize) -> String {
    format!(
        "{}:{line}",
        escape_display(&display_path(std::path::Path::new(file)), DISPLAY_MESSAGE)
    )
}

/// Built from the fields (not the core struct) so the core can add fields
/// to `EffectiveOption` without breaking this crate.
fn option_dto(key: &str, values: &[String], origin: &OptionOrigin) -> EffectiveOptionDto {
    let sensitive = is_sensitive(key);
    EffectiveOptionDto {
        key: escape_display(key, DISPLAY_SHORT),
        values: if sensitive {
            Vec::new()
        } else {
            values
                .iter()
                .map(|v| escape_display(v, DISPLAY_MESSAGE))
                .collect()
        },
        origin: match origin {
            OptionOrigin::Config { file, line } => Some(file_line(file, *line)),
            _ => None,
        },
        command: matches!(origin, OptionOrigin::Command),
        sensitive,
    }
}

fn warning_dto(file: &str, line: usize, message: &str) -> EffectiveWarningDto {
    EffectiveWarningDto {
        origin: file_line(file, line),
        message: escape_display(message, DISPLAY_MESSAGE),
    }
}

impl From<&EffectiveConfig> for EffectiveConfigDto {
    fn from(c: &EffectiveConfig) -> Self {
        Self {
            alias: escape_display(&c.alias, DISPLAY_SHORT),
            options: c
                .options
                .iter()
                .map(|o| option_dto(&o.key, &o.values, &o.origin))
                .collect(),
            route: c
                .route
                .iter()
                .map(|h| RouteHopDto {
                    alias: escape_display(&h.alias, DISPLAY_SHORT),
                    user: escape_display(&h.user, DISPLAY_SHORT),
                    hostname: escape_display(&h.hostname, DISPLAY_SHORT),
                    port: h.port,
                })
                .collect(),
            route_error: c.route_error.as_ref().map(safe_summary),
            warnings: c
                .warnings
                .iter()
                .map(|w| warning_dto(&w.file, w.line, &w.message))
                .collect(),
        }
    }
}

#[tauri::command]
pub(crate) async fn effective_config(
    engine: State<'_, Engine>,
    alias: String,
) -> Result<EffectiveConfigDto, ErrorSummary> {
    let config = engine.effective_config(&alias).await.map_err(summary)?;
    Ok(EffectiveConfigDto::from(&config))
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn record(message: &str) -> LogRecord {
        LogRecord {
            at: OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(42),
            level: LogLevel::Warn,
            session: None,
            target: "auth\u{202e}".into(),
            message: message.into(),
        }
    }

    #[test]
    fn log_lines_are_display_safe() {
        let dto = LogLineDto::from(&record("banner: line1\r\n\u{1b}[2Jfake\u{200b}"));
        assert_eq!(dto.at, 42);
        assert_eq!(dto.target, "auth\\u{202e}");
        assert_eq!(dto.message, "banner: line1\\r\\n\\u{1b}[2Jfake\\u{200b}");
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["level"], "warn");
    }

    #[test]
    fn copy_all_is_one_to_line_per_record() {
        let records = [record("one"), record("two\nlines")];
        let text = logs_text(&records);
        assert_eq!(
            text,
            format!("{}\n{}\n", records[0].to_line(), records[1].to_line())
        );
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn failed_outcomes_are_sanitized() {
        let info = SessionLogInfo {
            id: SessionId::from_raw(7),
            alias: "web".into(),
            probe: true,
            started: OffsetDateTime::UNIX_EPOCH,
            ended: Some(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1)),
            outcome: Some(SessionOutcome::Failed {
                error: ErrorSummary {
                    code: "auth_failed".into(),
                    message: "denied\n\u{2067}x".into(),
                    hint: None,
                },
            }),
        };
        let json = serde_json::to_value(SessionLogDto::from(&info)).unwrap();
        assert_eq!(json["id"], 7);
        assert_eq!(json["ended"], 1000);
        assert_eq!(json["outcome"]["kind"], "failed");
        assert_eq!(json["outcome"]["error"]["message"], "denied\\n\\u{2067}x");
        let closed = SessionLogInfo {
            outcome: Some(SessionOutcome::Closed {
                reason: CloseReason::RemoteClosed,
            }),
            ..info
        };
        let json = serde_json::to_value(SessionLogDto::from(&closed)).unwrap();
        assert_eq!(
            json["outcome"],
            serde_json::json!({ "kind": "closed", "reason": "remote_closed" })
        );
    }

    fn config_origin(line: usize) -> OptionOrigin {
        OptionOrigin::Config {
            file: "/etc/ssh/ssh_config".into(),
            line,
        }
    }

    #[test]
    fn effective_options_keep_origins_and_escape_values() {
        let first = option_dto(
            "identityfile",
            &["~/.ssh/id\u{202e}gpj".into()],
            &config_origin(3),
        );
        assert_eq!(first.values, ["~/.ssh/id\\u{202e}gpj"]);
        assert!(!first.sensitive);
        assert!(!first.command);
        assert_eq!(first.origin.as_deref(), Some("/etc/ssh/ssh_config:3"));
        let default = option_dto("port", &["22".into()], &OptionOrigin::Default);
        assert_eq!(default.origin, None);
        assert!(option_dto("user", &["root".into()], &OptionOrigin::Command).command);
        let warning = warning_dto("/etc/ssh/ssh_config", 9, "unsupported Match\u{1b}");
        assert_eq!(warning.origin, "/etc/ssh/ssh_config:9");
        assert_eq!(warning.message, "unsupported Match\\u{1b}");
    }

    #[test]
    fn command_keywords_are_withheld() {
        for key in [
            "proxycommand",
            "LocalCommand",
            "remotecommand",
            "knownhostscommand",
        ] {
            let option = option_dto(key, &["curl https://x | sh".into()], &config_origin(1));
            assert!(option.sensitive, "{key}");
            assert!(option.values.is_empty(), "{key}");
            assert!(!serde_json::to_string(&option).unwrap().contains("curl"));
        }
    }
}
