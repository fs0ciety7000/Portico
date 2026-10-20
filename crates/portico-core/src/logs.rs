//! Connection logs surfaced in the UI "debug" panel.
//!
//! Each session keeps a bounded ring of [`LogRecord`]s so the UI can show what
//! happened (kex algorithms, auth attempts, jump hops…) without a log file.
//! Log messages must never contain secret material.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

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
    fn ring_drops_oldest() {
        let mut ring = LogRing::new(2);
        ring.push(rec("a"));
        ring.push(rec("b"));
        ring.push(rec("c"));
        let msgs: Vec<_> = ring.iter().map(|r| r.message.as_str()).collect();
        assert_eq!(msgs, ["b", "c"]);
    }
}
