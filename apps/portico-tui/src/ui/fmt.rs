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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation() {
        assert_eq!(truncate("web-01", 10), "web-01");
        assert_eq!(truncate("production-database", 8), "product…");
    }
}
