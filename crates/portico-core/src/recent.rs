//! Recent sessions: `state/recent.toml` in the data dir (Phase 4).
//!
//! A bounded list ([`MAX_RECENT`]) of `{ alias, last, count }`, most recent
//! first, one entry per alias. The engine records an alias when a session
//! the user opened (not a copy-id probe) reaches `Connected`; recording runs
//! on `spawn_blocking` and a failure is only logged, never fatal to the
//! session.
//!
//! The file is Portico's own state (not meant to be edited by hand): it is
//! rewritten whole, atomically (`0600`, `state/` created `0700`), under an
//! exclusive inter-process lock (`recent.toml.lock`). Entries with an
//! invalid alias are dropped on read.

use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::Result;
use crate::store;

/// Most entries kept.
pub const MAX_RECENT: usize = 50;
/// Largest `recent.toml` read, in bytes.
const MAX_RECENT_FILE_BYTES: u64 = 256 * 1024;

/// One recently used host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentHost {
    pub alias: String,
    /// Last time a session to it reached `Connected` (RFC 3339).
    #[serde(with = "time::serde::rfc3339")]
    pub last: OffsetDateTime,
    /// Sessions recorded so far (saturating).
    #[serde(default)]
    pub count: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct RecentFile {
    #[serde(rename = "recent")]
    entries: Vec<RecentHost>,
}

/// Valid entries, most recent first, one per alias, at most [`MAX_RECENT`].
fn load(path: &Path) -> Result<Vec<RecentHost>> {
    let file: RecentFile = store::load_toml_capped(path, MAX_RECENT_FILE_BYTES)?;
    let mut entries: Vec<RecentHost> = Vec::with_capacity(file.entries.len());
    for entry in file.entries {
        if crate::hosts::check_alias(&entry.alias).is_err() {
            continue;
        }
        match entries.iter_mut().find(|e| e.alias == entry.alias) {
            Some(existing) => {
                existing.count = existing.count.saturating_add(entry.count);
                existing.last = existing.last.max(entry.last);
            }
            None => entries.push(entry),
        }
    }
    sort_and_bound(&mut entries);
    Ok(entries)
}

fn sort_and_bound(entries: &mut Vec<RecentHost>) {
    // Stable: equal dates keep their order.
    entries.sort_by_key(|e| std::cmp::Reverse(e.last));
    entries.truncate(MAX_RECENT);
}

fn save(path: &Path, entries: Vec<RecentHost>) -> Result<()> {
    store::save_toml(path, &RecentFile { entries })
}

/// The `limit` most recent hosts.
pub(crate) fn list(path: &Path, limit: usize) -> Result<Vec<RecentHost>> {
    let mut entries = load(path)?;
    entries.truncate(limit);
    Ok(entries)
}

/// Records a session to `alias` at `at`. Blocking.
pub(crate) fn record(path: &Path, alias: &str, at: OffsetDateTime) -> Result<()> {
    crate::hosts::check_alias(alias)?;
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut entries = load(path)?;
    match entries.iter_mut().find(|e| e.alias == alias) {
        Some(entry) => {
            entry.last = at;
            entry.count = entry.count.saturating_add(1);
        }
        None => entries.push(RecentHost {
            alias: alias.to_owned(),
            last: at,
            count: 1,
        }),
    }
    sort_and_bound(&mut entries);
    save(path, entries)
}

/// Removes `alias`; returns whether it was listed. Blocking.
pub(crate) fn forget(path: &Path, alias: &str) -> Result<bool> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut entries = load(path)?;
    let before = entries.len();
    entries.retain(|e| e.alias != alias);
    if entries.len() == before {
        return Ok(false);
    }
    save(path, entries)?;
    Ok(true)
}

/// Empties the list (also repairs an unreadable file). Blocking.
pub(crate) fn clear(path: &Path) -> Result<()> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    save(path, Vec::new())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use time::Duration;

    use super::*;

    fn at(minutes: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::minutes(minutes)
    }

    fn aliases(list: &[RecentHost]) -> Vec<&str> {
        list.iter().map(|e| e.alias.as_str()).collect()
    }

    #[test]
    fn records_sorts_counts_and_bounds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("data/state/recent.toml");
        assert!(list(&path, 10).expect("empty").is_empty());

        record(&path, "a", at(1)).expect("a");
        record(&path, "b", at(2)).expect("b");
        record(&path, "a", at(3)).expect("a again");
        let all = list(&path, 10).expect("list");
        assert_eq!(aliases(&all), ["a", "b"]);
        assert_eq!(all.first().map(|e| (e.count, e.last)), Some((2, at(3))));
        assert_eq!(aliases(&list(&path, 1).expect("list")), ["a"]);

        for i in 0..60 {
            record(&path, &format!("h{i}"), at(100 + i)).expect("record");
        }
        let all = list(&path, usize::MAX).expect("list");
        assert_eq!(all.len(), MAX_RECENT);
        assert_eq!(all.first().map(|e| e.alias.as_str()), Some("h59"));
        assert_eq!(all.last().map(|e| e.alias.as_str()), Some("h10"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&dir.path().join("data/state")), 0o700);
        }
    }

    #[test]
    fn forget_and_clear() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent.toml");
        record(&path, "a", at(1)).expect("a");
        record(&path, "b", at(2)).expect("b");
        assert!(forget(&path, "a").expect("forget"));
        assert!(!forget(&path, "a").expect("again"));
        assert_eq!(aliases(&list(&path, 10).expect("list")), ["b"]);
        clear(&path).expect("clear");
        assert!(list(&path, 10).expect("list").is_empty());

        // An unreadable file is reported, and `clear` repairs it.
        std::fs::write(&path, "recent = 3").expect("write");
        assert_eq!(list(&path, 1).expect_err("bad").summary().code, "serde");
        assert!(record(&path, "a", at(1)).is_err());
        clear(&path).expect("clear");
        record(&path, "a", at(1)).expect("a");
    }

    #[test]
    fn hand_edited_entries_are_merged_and_filtered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent.toml");
        std::fs::write(
            &path,
            r#"
[[recent]]
alias = "a"
last = "1970-01-01T00:01:00Z"
count = 2
[[recent]]
alias = "bad alias"
last = "1970-01-01T00:09:00Z"
[[recent]]
alias = "a"
last = "1970-01-01T00:05:00Z"
count = 1
"#,
        )
        .expect("write");
        let all = list(&path, 10).expect("list");
        assert_eq!(
            all,
            [RecentHost {
                alias: "a".into(),
                last: at(5),
                count: 3
            }]
        );
        assert!(record(&path, "", at(1)).is_err());
    }

    #[test]
    fn concurrent_records_are_all_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("recent.toml");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..5 {
                        record(&path, "shared", at(i)).expect("record");
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread");
        }
        let all = list(&path, 10).expect("list");
        assert_eq!(all.first().map(|e| e.count), Some(40));
    }
}
