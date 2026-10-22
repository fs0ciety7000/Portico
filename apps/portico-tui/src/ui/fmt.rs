//! Small display helpers shared by the views.
//!
//! Dates are shown in local time: the local UTC offset is read once by
//! [`init_local_offset`], at startup, before any other thread exists (the
//! `time` crate refuses to read it from a multi-threaded process on Unix,
//! where the environment could change under it). When it cannot be read,
//! dates stay in UTC and say so (`22 Oct 13:34 UTC`).

use std::sync::OnceLock;

use time::{OffsetDateTime, UtcOffset};

/// `/home/me/.ssh/x` → `~/.ssh/x` (display only). Only a whole `$HOME`
/// component is replaced (`/home/me2` stays as is).
pub(crate) fn display_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => home_relative(path, &home),
        _ => path.to_owned(),
    }
}

fn home_relative(path: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// Cuts the middle of `text` to fit `max` characters (`~/.local/…/recordings/`):
/// both ends of a path say the most.
pub(crate) fn truncate_middle(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_owned();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    let keep = max - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let mut out: String = text.chars().take(head).collect();
    out.push('…');
    out.extend(text.chars().skip(len - tail));
    out
}

/// The local UTC offset (`None`: unknown, dates are shown in UTC).
static LOCAL_OFFSET: OnceLock<Option<UtcOffset>> = OnceLock::new();

/// Reads the local UTC offset. Call it once, first thing in `main`, while
/// the process has a single thread.
pub(crate) fn init_local_offset() {
    let _ = LOCAL_OFFSET.set(UtcOffset::current_local_offset().ok());
}

/// `22 Oct 13:34` in local time, 24 h; the year only when it is not the
/// current one (`3 Mar 2025 09:05`); ` UTC` appended when the local
/// offset is unknown.
pub(crate) fn date_label(at: OffsetDateTime) -> String {
    date_label_at(
        at,
        OffsetDateTime::now_utc(),
        LOCAL_OFFSET.get().copied().flatten(),
    )
}

fn date_label_at(at: OffsetDateTime, now: OffsetDateTime, offset: Option<UtcOffset>) -> String {
    let (at, now, suffix) = match offset {
        Some(offset) => (at.to_offset(offset), now.to_offset(offset), ""),
        None => (
            at.to_offset(UtcOffset::UTC),
            now.to_offset(UtcOffset::UTC),
            " UTC",
        ),
    };
    let month = at.month().to_string();
    let month = month.get(..3).unwrap_or(&month);
    let year = if at.year() == now.year() {
        String::new()
    } else {
        format!(" {}", at.year())
    };
    format!(
        "{} {month}{year} {:02}:{:02}{suffix}",
        at.day(),
        at.hour(),
        at.minute()
    )
}

/// Truncates to `max` characters with a trailing `…`.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut t: String = text.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Core messages are lowercase sentences (`wrong vault passphrase`): the
/// first word is capitalised for display, unless it is a name, a path or an
/// identifier (`id_ed25519 …`, `~/.ssh/…`, `ssh-agent …`, `web: …`), left as
/// written. Same rule as the GUI's `sentence()`; never used to decide
/// anything (no string matching on errors).
pub(crate) fn sentence(text: &str) -> String {
    let first_word = text.split(char::is_whitespace).next().unwrap_or_default();
    let plain = !first_word.is_empty() && first_word.chars().all(|c| c.is_ascii_lowercase());
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if plain => first.to_uppercase().chain(chars).collect(),
        _ => text.to_owned(),
    }
}

/// Greedy word wrap to `width` columns (character count); words longer
/// than a line are cut. Always returns at least one line.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let mut word: String = word.to_owned();
        loop {
            let len = word.chars().count();
            let need = if used == 0 { len } else { used + 1 + len };
            if need <= width {
                if used > 0 {
                    current.push(' ');
                }
                current.push_str(&word);
                used = need;
                break;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut current));
                used = 0;
                continue;
            }
            // Longer than a whole line: cut it.
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
            if word.is_empty() {
                break;
            }
        }
    }
    if used > 0 || lines.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping() {
        assert_eq!(wrap("a bb ccc", 4), vec!["a bb", "ccc"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("fits as is", 20), vec!["fits as is"]);
    }

    #[test]
    fn sentences() {
        assert_eq!(sentence("wrong vault passphrase"), "Wrong vault passphrase");
        assert_eq!(sentence("cannot connect"), "Cannot connect");
        assert_eq!(sentence("refused"), "Refused");
        // Names, paths and identifiers stay as written.
        for kept in [
            "id_ed25519 is encrypted",
            "~/.ssh/config:3: bad",
            "ssh-agent is not running",
            "web: closed",
            "settings.toml ignored",
            "Already capitalised",
            "",
        ] {
            assert_eq!(sentence(kept), kept);
        }
    }

    #[test]
    fn home_is_a_whole_component() {
        assert_eq!(home_relative("/home/me/.ssh/x", "/home/me"), "~/.ssh/x");
        assert_eq!(home_relative("/home/me", "/home/me/"), "~");
        assert_eq!(home_relative("/home/me2/x", "/home/me"), "/home/me2/x");
        assert_eq!(home_relative("/srv/x", "/home/me"), "/srv/x");
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends() {
        assert_eq!(truncate_middle("~/short/", 20), "~/short/");
        let cut = truncate_middle("~/.local/share/portico/recordings/", 20);
        assert_eq!(cut.chars().count(), 20);
        assert_eq!(cut, "~/.local/s…cordings/");
        assert!(cut.contains('…'));
        assert_eq!(truncate_middle("abc", 1), "…");
        assert_eq!(truncate_middle("abc", 0), "");
    }

    #[test]
    fn dates_are_local_24h_with_the_year_only_when_needed() {
        use time::macros::{datetime, offset};
        let now = datetime!(2026-10-22 18:00 UTC);
        let at = datetime!(2026-10-22 11:34 UTC);
        // Local time (here UTC+2), 24 h.
        assert_eq!(date_label_at(at, now, Some(offset!(+2))), "22 Oct 13:34");
        assert_eq!(date_label_at(at, now, Some(offset!(-5))), "22 Oct 06:34");
        // Another year: said.
        let old = datetime!(2025-03-03 09:05 UTC);
        assert_eq!(
            date_label_at(old, now, Some(UtcOffset::UTC)),
            "3 Mar 2025 09:05"
        );
        // The offset is unknown: UTC, and it says so.
        assert_eq!(date_label_at(at, now, None), "22 Oct 11:34 UTC");
        // The local date decides the day (and the year).
        let late = datetime!(2026-12-31 23:30 UTC);
        assert_eq!(
            date_label_at(late, now, Some(offset!(+1))),
            "1 Jan 2027 00:30"
        );
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("web-01", 10), "web-01");
        assert_eq!(truncate("production-database", 8), "product…");
    }
}
