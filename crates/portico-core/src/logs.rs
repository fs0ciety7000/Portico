//! Connection logs surfaced in the UI "debug" panel.
//!
//! Each session keeps a bounded ring of [`LogRecord`]s so the UI can show what
//! happened (kex algorithms, auth attempts, jump hops…) without a log file.
//! Log messages must never contain secret material.
//!
//! Phase 4: the logs of a session stay readable after it ends (failure,
//! close): the engine keeps the rings of the last
//! [`crate::engine::ENDED_SESSIONS_KEPT`] ended sessions, listed by
//! [`crate::Engine::sessions_with_logs`] ([`SessionLogInfo`]).
//! [`LogRecord::to_line`] is the stable text form used by "copy".

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::ErrorSummary;
use crate::events::CloseReason;
use crate::ids::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub level: LogLevel,
    pub session: Option<SessionId>,
    /// Short subsystem tag: `kex`, `auth`, `jump`, `sftp`, `tunnel`…
    pub target: String,
    pub message: String,
}

impl LogLevel {
    /// Uppercase name, as printed by [`LogRecord::to_line`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

impl LogRecord {
    /// One line of text, stable for "copy" and bug reports:
    /// `2026-10-21T12:34:56.789Z INFO  auth: message`
    /// (UTC, milliseconds, level padded to 5, control characters escaped
    /// so a record is always exactly one line).
    pub fn to_line(&self) -> String {
        let format = time::macros::format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
        );
        let at = self
            .at
            .to_offset(time::UtcOffset::UTC)
            .format(&format)
            .unwrap_or_else(|_| self.at.unix_timestamp().to_string());
        format!(
            "{at} {:<5} {}: {}",
            self.level.as_str(),
            escape_controls(&self.target),
            escape_controls(&self.message)
        )
    }
}

/// `\n`, `\r`, `\t`, other control characters and bidi or invisible
/// format characters as visible escapes.
fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || crate::snippets::is_hidden_format(c) => {
                out.extend(c.escape_unicode());
            }
            c => out.push(c),
        }
    }
    out
}

/// How an ended session ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionOutcome {
    Closed { reason: CloseReason },
    Failed { error: ErrorSummary },
}

/// A session whose logs can be read with [`crate::Engine::session_logs`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLogInfo {
    pub id: SessionId,
    pub alias: String,
    /// A copy-id verification, not a session the user opened.
    pub probe: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
    /// `None` while the session is alive.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended: Option<OffsetDateTime>,
    pub outcome: Option<SessionOutcome>,
}

/// Fixed-capacity ring buffer of log records.
#[derive(Debug, Clone)]
pub struct LogRing {
    capacity: usize,
    records: VecDeque<LogRecord>,
}

impl LogRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            records: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn push(&mut self, record: LogRecord) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        self.records.iter()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(msg: &str) -> LogRecord {
        LogRecord {
            at: OffsetDateTime::UNIX_EPOCH,
            level: LogLevel::Info,
            session: None,
            target: "test".into(),
            message: msg.into(),
        }
    }

    #[test]
    fn records_render_as_one_stable_line() {
        let mut record = rec("line 1\nline 2\t\u{1b}[31m");
        record.at = OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(1_234);
        assert_eq!(
            record.to_line(),
            "1970-01-01T00:00:01.234Z INFO  test: line 1\\nline 2\\t\\u{1b}[31m"
        );
        record.level = LogLevel::Error;
        record.at = record
            .at
            .to_offset(time::UtcOffset::from_hms(2, 0, 0).expect("offset"));
        assert!(
            record
                .to_line()
                .starts_with("1970-01-01T00:00:01.234Z ERROR test: ")
        );
    }

    #[test]
    fn ring_drops_oldest() {
        let mut ring = LogRing::new(2);
        ring.push(rec("a"));
        ring.push(rec("b"));
        ring.push(rec("c"));
        let msgs: Vec<_> = ring.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["b", "c"]);
    }
}
