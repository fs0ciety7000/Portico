//! Session logs panel (pure state): sessions with logs on the left (a
//! one-line selector on narrow screens); on the right two tabs, **Logs**
//! (records: level, target, message; text filter, minimum level, live
//! follow, copy) and **Effective config** (effective config of the
//! session's alias: route, options with their origin, warnings).
//!
//! Key grammar (same as the GUI): `y` copy, `Ctrl-R` refresh, `Tab` other
//! tab, `v` level, `/` filter, `PgUp` / `PgDn` / `g` / `G` scroll (`G`
//! follows again), `↑` `↓` or `[` `]` / `←` `→` session, `?` help.
//!
//! Invariants: sensitive options (commands run locally or remotely) are
//! masked unless revealed and are never part of a copy; copied logs are
//! [`LogRecord::to_line`] text, bounded by [`MAX_COPY_BYTES`]. The logs
//! scroll by whole records, so the view always starts on a record.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::ErrorSummary;
use portico_core::effective::{EffectiveConfig, EffectiveOption, OptionOrigin};
use portico_core::ids::SessionId;
use portico_core::logs::{LogLevel, LogRecord, SessionLogInfo};

use crate::keys::{Load, Notice};

/// Most records kept for the shown session (the engine's ring is smaller).
const MAX_LINES: usize = 5000;
/// Largest copy sent through OSC 52 (many terminals refuse more).
pub(crate) const MAX_COPY_BYTES: usize = 64 * 1024;
/// Records (logs) or rows (config) moved by `PgUp` / `PgDn`.
pub(crate) const PAGE: usize = 10;
/// Below this screen width the session column becomes a one-line selector.
pub(crate) const NARROW: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DebugTab {
    Logs,
    Config,
}

/// Options whose value is a command line (may embed credentials).
///
/// The core decides (commands may embed tokens): masked by default, never
/// copied.
pub(crate) fn is_sensitive(option: &EffectiveOption) -> bool {
    option.sensitive
}

/// Where an option comes from, as both UIs word it.
pub(crate) fn origin_label(origin: &OptionOrigin) -> String {
    match origin {
        OptionOrigin::Config { file, line } => format!("{file}:{line}"),
        OptionOrigin::Default => "default".to_owned(),
        // Set by the connection itself (`-l`, the alias…).
        OptionOrigin::Command => "at connect".to_owned(),
    }
}

#[derive(Debug)]
pub(crate) struct DebugPanel {
    /// Newest first.
    pub(crate) sessions: Vec<SessionLogInfo>,
    pub(crate) selected: usize,
    pub(crate) tab: DebugTab,
    pub(crate) logs: Vec<LogRecord>,
    pub(crate) filter: String,
    /// The filter field has the keys.
    pub(crate) filtering: bool,
    pub(crate) min_level: LogLevel,
    /// Visible records below the view (0 = following new records).
    pub(crate) scroll: usize,
    pub(crate) config: Option<(String, Load<EffectiveConfig>)>,
    pub(crate) config_scroll: usize,
    /// Sensitive values shown (on this screen only).
    pub(crate) reveal: bool,
    pub(crate) notice: Option<Notice>,
    /// Host whose config is shown when there is no session yet.
    pub(crate) fallback_alias: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DebugOutcome {
    None,
    Close,
    /// Another session was selected: reload its logs.
    Selected,
    /// The config of this alias is needed.
    LoadConfig(String),
    /// Text for the clipboard and how many records (or options) it holds.
    Copy {
        text: String,
        lines: usize,
        total: usize,
    },
    Refresh,
    /// `?`: the help overlay, over the panel.
    Help,
}

impl DebugPanel {
    /// `sessions` as the engine lists them (oldest first).
    pub(crate) fn new(
        mut sessions: Vec<SessionLogInfo>,
        preselect: Option<SessionId>,
        fallback_alias: Option<String>,
    ) -> Self {
        sessions.reverse();
        let selected = preselect
            .and_then(|id| sessions.iter().position(|s| s.id == id))
            .unwrap_or(0);
        Self {
            sessions,
            selected,
            tab: DebugTab::Logs,
            logs: Vec::new(),
            filter: String::new(),
            filtering: false,
            min_level: LogLevel::Debug,
            scroll: 0,
            config: None,
            config_scroll: 0,
            reveal: false,
            notice: None,
            fallback_alias,
        }
    }

    pub(crate) fn session(&self) -> Option<&SessionLogInfo> {
        self.sessions.get(self.selected)
    }

    /// The alias the Effective config tab describes.
    pub(crate) fn alias(&self) -> Option<String> {
        self.session()
            .map(|s| s.alias.clone())
            .or_else(|| self.fallback_alias.clone())
    }

    /// New session list, keeping the selected session.
    pub(crate) fn set_sessions(&mut self, mut sessions: Vec<SessionLogInfo>) {
        let current = self.session().map(|s| s.id);
        sessions.reverse();
        self.sessions = sessions;
        self.selected = current
            .and_then(|id| self.sessions.iter().position(|s| s.id == id))
            .unwrap_or(0);
    }

    pub(crate) fn set_logs(&mut self, logs: Vec<LogRecord>) {
        self.logs = logs;
        self.scroll = 0;
    }

    /// A live record: kept if it belongs to the shown session.
    pub(crate) fn push(&mut self, record: LogRecord) {
        if record.session.is_some() && record.session == self.session().map(|s| s.id) {
            if self.logs.len() >= MAX_LINES {
                self.logs.remove(0);
            }
            // Scrolled up: the view stays on the same records.
            if self.scroll > 0 && self.shows(&record) {
                self.scroll = self.scroll.saturating_add(1);
            }
            self.logs.push(record);
        }
    }

    /// The record passes the level and the text filter.
    fn shows(&self, record: &LogRecord) -> bool {
        let needle = self.filter.to_lowercase();
        (record.level >= self.min_level || self.min_level == LogLevel::Debug)
            && (needle.is_empty()
                || record.message.to_lowercase().contains(&needle)
                || record.target.to_lowercase().contains(&needle))
    }

    pub(crate) fn visible(&self) -> Vec<&LogRecord> {
        self.logs.iter().filter(|r| self.shows(r)).collect()
    }

    fn cycle_level(&mut self) {
        self.min_level = match self.min_level {
            LogLevel::Trace | LogLevel::Debug => LogLevel::Info,
            LogLevel::Info => LogLevel::Warn,
            LogLevel::Warn => LogLevel::Error,
            LogLevel::Error => LogLevel::Debug,
        };
        self.scroll = 0;
    }

    /// Every record of the session (the filter does not apply), the
    /// newest kept when over [`MAX_COPY_BYTES`].
    fn copy_logs(&self) -> DebugOutcome {
        let total = self.logs.len();
        let mut lines: Vec<String> = Vec::new();
        let mut size = 0usize;
        for record in self.logs.iter().rev() {
            let line = record.to_line();
            size = size.saturating_add(line.len() + 1);
            if size > MAX_COPY_BYTES {
                break;
            }
            lines.push(line);
        }
        lines.reverse();
        let count = lines.len();
        DebugOutcome::Copy {
            text: lines.join("\n"),
            lines: count,
            total,
        }
    }

    /// The effective config as text; sensitive options are left out, even
    /// revealed.
    fn copy_config(&self) -> DebugOutcome {
        let Some((_, Load::Ready(config))) = &self.config else {
            return DebugOutcome::None;
        };
        let mut lines = Vec::new();
        for o in config.options.iter().filter(|o| !is_sensitive(o)) {
            let origin = origin_label(&o.origin);
            lines.push(format!("{} {}  # {origin}", o.key, o.values.join(" ")));
        }
        let count = lines.len();
        DebugOutcome::Copy {
            text: lines.join("\n"),
            lines: count,
            total: config.options.len(),
        }
    }

    fn select(&mut self, delta: isize) -> DebugOutcome {
        let len = self.sessions.len();
        if len == 0 {
            return DebugOutcome::None;
        }
        let next = self.selected.saturating_add_signed(delta).min(len - 1);
        if next == self.selected {
            return DebugOutcome::None;
        }
        self.selected = next;
        self.notice = None;
        self.config_scroll = 0;
        DebugOutcome::Selected
    }

    /// The Effective config tab needs (re)loading for the current alias.
    pub(crate) fn needs_config(&self) -> Option<String> {
        let alias = self.alias()?;
        let loaded = self.config.as_ref().is_some_and(|(a, _)| *a == alias);
        (self.tab == DebugTab::Config && !loaded).then_some(alias)
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> DebugOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.filtering {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char('u') if ctrl => self.filter.clear(),
                KeyCode::Char(c) if !ctrl => crate::form::push_char(&mut self.filter, c, 64),
                _ => {}
            }
            self.scroll = 0;
            return DebugOutcome::None;
        }
        // Logs count records below the view (0 follows); the config, rows
        // above it.
        let (scroll, up_grows) = match self.tab {
            DebugTab::Logs => (&mut self.scroll, true),
            DebugTab::Config => (&mut self.config_scroll, false),
        };
        let (up, down) = if up_grows {
            (scroll.saturating_add(PAGE), scroll.saturating_sub(PAGE))
        } else {
            (scroll.saturating_sub(PAGE), scroll.saturating_add(PAGE))
        };
        // Far end: clamped when drawn.
        let (top, bottom) = if up_grows {
            (usize::MAX / 2, 0)
        } else {
            (0, usize::MAX / 2)
        };
        match key.code {
            KeyCode::Esc if !self.filter.is_empty() && self.tab == DebugTab::Logs => {
                self.filter.clear();
            }
            KeyCode::Esc | KeyCode::Char('q') => return DebugOutcome::Close,
            KeyCode::Char('r') if ctrl => return DebugOutcome::Refresh,
            KeyCode::Char('?') => return DebugOutcome::Help,
            KeyCode::Down | KeyCode::Char('j' | ']') | KeyCode::Right => return self.select(1),
            KeyCode::Up | KeyCode::Char('k' | '[') | KeyCode::Left => return self.select(-1),
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab = match self.tab {
                    DebugTab::Logs => DebugTab::Config,
                    DebugTab::Config => DebugTab::Logs,
                };
                self.notice = None;
                if let Some(alias) = self.needs_config() {
                    return DebugOutcome::LoadConfig(alias);
                }
            }
            KeyCode::PageUp => *scroll = up,
            KeyCode::PageDown => *scroll = down,
            KeyCode::Char('g') | KeyCode::Home => *scroll = top,
            KeyCode::Char('G') | KeyCode::End => *scroll = bottom,
            KeyCode::Char('/') if self.tab == DebugTab::Logs => self.filtering = true,
            KeyCode::Char('v') if self.tab == DebugTab::Logs => self.cycle_level(),
            KeyCode::Char('s') if self.tab == DebugTab::Config => self.reveal = !self.reveal,
            KeyCode::Char('y') => {
                return match self.tab {
                    DebugTab::Logs if self.logs.is_empty() => DebugOutcome::None,
                    DebugTab::Logs => self.copy_logs(),
                    DebugTab::Config => self.copy_config(),
                };
            }
            _ => {}
        }
        DebugOutcome::None
    }

    pub(crate) fn set_config(
        &mut self,
        alias: String,
        result: Result<EffectiveConfig, ErrorSummary>,
    ) {
        let load = match result {
            Ok(c) => Load::Ready(c),
            Err(e) => Load::Failed(e),
        };
        self.config = Some((alias, load));
    }

    /// The scroll positions as drawn (clamped to their maximum).
    pub(crate) fn clamp_scroll(&mut self, logs_max: usize, config_max: usize) {
        self.scroll = self.scroll.min(logs_max);
        self.config_scroll = self.config_scroll.min(config_max);
    }

    /// Status bar keys, most important first; `narrow` when the sessions
    /// are a one-line selector (`[` `]`).
    pub(crate) fn hints(&self, narrow: bool) -> Vec<(&'static str, &'static str)> {
        if self.filtering {
            return vec![("↵", "done"), ("Ctrl-U", "clear"), ("Esc", "done")];
        }
        let mut hints = Vec::new();
        match self.tab {
            DebugTab::Logs => {
                hints.extend([("/", "filter"), ("v", "level"), ("y", "copy")]);
                if self.scroll > 0 {
                    hints.push(("G", "follow"));
                }
                hints.push(("Tab", "config"));
            }
            DebugTab::Config => {
                hints.push(("s", if self.reveal { "hide" } else { "reveal" }));
                hints.extend([("y", "copy"), ("Tab", "logs")]);
            }
        }
        hints.push(if narrow {
            ("[ ]", "session")
        } else {
            ("↑↓", "session")
        });
        hints.extend([
            ("?", "help"),
            ("PgUp PgDn", "scroll"),
            ("Ctrl-R", "refresh"),
            ("Esc", "close"),
        ]);
        hints
    }
}

/// Records `[start, end)` that fill `rows` rows with `scroll` visible
/// records below the view, `heights` being each record's row count. Only
/// whole records, so the view always begins on a record's first row (a
/// record taller than the view shows its head). Returns the scroll clamped
/// to its maximum (the first record at the top) as well.
pub(crate) fn window(heights: &[usize], scroll: usize, rows: usize) -> (usize, usize, usize) {
    let len = heights.len();
    // Records that fit from the top: the most that can be scrolled away.
    let mut used = 0usize;
    let mut first_page = 0usize;
    for h in heights {
        if first_page > 0 && used.saturating_add(*h) > rows {
            break;
        }
        used = used.saturating_add(*h);
        first_page += 1;
    }
    let scroll = scroll.min(len.saturating_sub(first_page));
    let end = len.saturating_sub(scroll);
    let mut start = end;
    let mut used = 0usize;
    while let Some(h) = start.checked_sub(1).and_then(|i| heights.get(i)) {
        if start < end && used.saturating_add(*h) > rows {
            break;
        }
        used = used.saturating_add(*h);
        start -= 1;
    }
    (start, end, scroll)
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn info(id: SessionId, alias: &str) -> SessionLogInfo {
        SessionLogInfo {
            id,
            alias: alias.into(),
            probe: false,
            started: OffsetDateTime::UNIX_EPOCH,
            ended: None,
            outcome: None,
        }
    }

    fn rec(session: SessionId, level: LogLevel, target: &str, message: &str) -> LogRecord {
        LogRecord {
            at: OffsetDateTime::UNIX_EPOCH,
            level,
            session: Some(session),
            target: target.into(),
            message: message.into(),
        }
    }

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn preselects_filters_follows_and_copies() {
        let (a, b) = (SessionId::next(), SessionId::next());
        let mut panel = DebugPanel::new(vec![info(a, "web"), info(b, "db")], Some(a), None);
        assert_eq!(panel.session().map(|s| s.alias.as_str()), Some("web"));
        panel.set_logs(vec![
            rec(a, LogLevel::Debug, "kex", "curve25519"),
            rec(a, LogLevel::Info, "auth", "trying agent"),
            rec(a, LogLevel::Warn, "auth", "agent refused"),
        ]);
        // Live records of another session are not mixed in.
        panel.push(rec(b, LogLevel::Error, "x", "other"));
        panel.push(rec(a, LogLevel::Error, "auth", "no method left"));
        assert_eq!(panel.visible().len(), 4);
        panel.handle_key(k(KeyCode::Char('v')));
        assert_eq!(panel.min_level, LogLevel::Info);
        assert_eq!(panel.visible().len(), 3);
        panel.handle_key(k(KeyCode::Char('/')));
        for c in "AGENT".chars() {
            panel.handle_key(k(KeyCode::Char(c)));
        }
        panel.handle_key(k(KeyCode::Enter));
        assert_eq!(panel.visible().len(), 2);
        // Copy takes every record, not only the filtered ones.
        let DebugOutcome::Copy { text, lines, total } = panel.handle_key(k(KeyCode::Char('y')))
        else {
            panic!("copy");
        };
        assert_eq!((lines, total), (4, 4));
        assert!(
            text.starts_with("1970-01-01T00:00:00.000Z DEBUG kex: curve25519"),
            "{text}"
        );
        // Esc clears the filter first, then closes.
        assert_eq!(panel.handle_key(k(KeyCode::Esc)), DebugOutcome::None);
        assert!(panel.filter.is_empty());
        assert_eq!(panel.handle_key(k(KeyCode::Esc)), DebugOutcome::Close);
        assert_eq!(panel.handle_key(k(KeyCode::Up)), DebugOutcome::Selected);
        assert_eq!(panel.session().map(|s| s.alias.as_str()), Some("db"));
    }

    #[test]
    fn common_key_grammar() {
        let (a, b) = (SessionId::next(), SessionId::next());
        let mut panel = DebugPanel::new(vec![info(a, "web"), info(b, "db")], Some(b), None);
        // `[` / `]` and `←` / `→` move between sessions, like `↑` / `↓`.
        assert_eq!(
            panel.handle_key(k(KeyCode::Char(']'))),
            DebugOutcome::Selected
        );
        assert_eq!(panel.session().map(|s| s.alias.as_str()), Some("web"));
        assert_eq!(
            panel.handle_key(k(KeyCode::Char('['))),
            DebugOutcome::Selected
        );
        assert_eq!(panel.handle_key(k(KeyCode::Right)), DebugOutcome::Selected);
        assert_eq!(panel.handle_key(k(KeyCode::Left)), DebugOutcome::Selected);
        assert_eq!(panel.handle_key(k(KeyCode::Char('?'))), DebugOutcome::Help);
        assert_eq!(
            panel.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            DebugOutcome::Refresh
        );
        // `g` goes to the oldest record, `G` follows again.
        panel.handle_key(k(KeyCode::Char('g')));
        assert!(panel.scroll > 0);
        assert!(panel.hints(false).contains(&("G", "follow")));
        panel.handle_key(k(KeyCode::Char('G')));
        assert_eq!(panel.scroll, 0);
        let keys: Vec<&str> = panel.hints(true).iter().map(|(k, _)| *k).collect();
        for key in [
            "/",
            "v",
            "y",
            "Tab",
            "[ ]",
            "PgUp PgDn",
            "Ctrl-R",
            "?",
            "Esc",
        ] {
            assert!(keys.contains(&key), "{key} in {keys:?}");
        }
        assert!(panel.hints(false).contains(&("↑↓", "session")));
    }

    #[test]
    fn scrolled_view_keeps_its_records() {
        let a = SessionId::next();
        let mut panel = DebugPanel::new(vec![info(a, "web")], None, None);
        panel.set_logs(vec![rec(a, LogLevel::Info, "t", "one")]);
        panel.handle_key(k(KeyCode::PageUp));
        assert_eq!(panel.scroll, PAGE);
        panel.push(rec(a, LogLevel::Info, "t", "two"));
        assert_eq!(panel.scroll, PAGE + 1, "one more record below the view");
        // A record the filter hides does not move the view.
        panel.min_level = LogLevel::Warn;
        panel.push(rec(a, LogLevel::Info, "t", "three"));
        assert_eq!(panel.scroll, PAGE + 1);
    }

    #[test]
    fn windows_hold_whole_records() {
        // Following: the newest records that fit, whole.
        assert_eq!(window(&[2, 3, 1, 2], 0, 4), (2, 4, 0));
        // A record that would be cut at the top is left out.
        assert_eq!(window(&[2, 3, 1, 2], 0, 5), (2, 4, 0));
        assert_eq!(window(&[2, 3, 1, 2], 0, 6), (1, 4, 0));
        // Scrolled by one record.
        assert_eq!(window(&[2, 3, 1, 2], 1, 4), (1, 3, 1));
        // Scrolling stops when the first record is at the top.
        assert_eq!(window(&[2, 3, 1, 2], 99, 4), (0, 1, 3));
        // Taller than the view: its head.
        assert_eq!(window(&[9], 0, 4), (0, 1, 0));
        assert_eq!(window(&[], 3, 4), (0, 0, 0));
    }

    #[test]
    fn copy_is_bounded_and_keeps_the_newest() {
        let a = SessionId::next();
        let mut panel = DebugPanel::new(vec![info(a, "web")], None, None);
        let long = "x".repeat(1000);
        panel.set_logs(
            (0..100)
                .map(|_| rec(a, LogLevel::Info, "t", &long))
                .collect(),
        );
        let DebugOutcome::Copy { text, lines, total } = panel.handle_key(k(KeyCode::Char('y')))
        else {
            panic!("copy");
        };
        assert!(text.len() <= MAX_COPY_BYTES);
        assert!(lines < total && lines > 50, "{lines}");
    }

    #[test]
    fn config_tab_loads_masks_and_never_copies_secrets() {
        let mut panel = DebugPanel::new(Vec::new(), None, Some("web".into()));
        assert_eq!(
            panel.handle_key(k(KeyCode::Tab)),
            DebugOutcome::LoadConfig("web".into())
        );
        let option = |key: &str, value: &str| EffectiveOption {
            key: key.into(),
            values: vec![value.into()],
            origin: OptionOrigin::Command,
            sensitive: portico_core::effective::SENSITIVE_KEYWORDS.contains(&key),
        };
        panel.set_config(
            "web".into(),
            Ok(EffectiveConfig {
                alias: "web".into(),
                options: vec![
                    option("hostname", "web.internal"),
                    option("proxycommand", "sshpass -p hunter2 ssh"),
                ],
                route: Vec::new(),
                route_error: None,
                warnings: Vec::new(),
            }),
        );
        assert_eq!(panel.needs_config(), None);
        let DebugOutcome::Copy { text, .. } = panel.handle_key(k(KeyCode::Char('y'))) else {
            panic!("copy");
        };
        assert!(
            text.contains("hostname web.internal  # at connect"),
            "{text}"
        );
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!panel.reveal);
        panel.handle_key(k(KeyCode::Char('s')));
        assert!(panel.reveal);
        let DebugOutcome::Copy { text, .. } = panel.handle_key(k(KeyCode::Char('y'))) else {
            panic!("copy");
        };
        assert!(!text.contains("hunter2"), "revealed is still never copied");
    }
}
