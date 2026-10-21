//! Small display helpers shared by the views.

/// `/home/me/.ssh/x` → `~/.ssh/x` (display only).
pub(crate) fn display_path(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => {
            format!("~{}", path.get(home.len()..).unwrap_or_default())
        }
        _ => path.to_owned(),
    }
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
    fn truncation() {
        assert_eq!(truncate("web-01", 10), "web-01");
        assert_eq!(truncate("production-database", 8), "product…");
    }
}
