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
    fn truncation() {
        assert_eq!(truncate("web-01", 10), "web-01");
        assert_eq!(truncate("production-database", 8), "product…");
    }
}
