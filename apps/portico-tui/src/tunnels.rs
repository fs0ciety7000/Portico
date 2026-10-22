//! Tunnels panel state: running tunnels (all sessions), forwards declared in
//! `ssh_config` for the current host (offered, never started on their own),
//! and a one-line form to add `-L` / `-R` / `-D` specs.

use crossterm::event::{KeyCode, KeyEvent};
use portico_core::ErrorSummary;
use portico_core::events::TunnelEvent;
use portico_core::forward::{ForwardSpec, TunnelInfo};
use portico_core::ids::{SessionId, TunnelId};
use ratatui::widgets::ListState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TunnelStatus {
    Starting,
    Listening(String),
    Failed(ErrorSummary),
}

#[derive(Debug, Clone)]
pub(crate) struct TunnelRow {
    pub(crate) id: TunnelId,
    pub(crate) session: SessionId,
    pub(crate) alias: String,
    pub(crate) spec: ForwardSpec,
    pub(crate) status: TunnelStatus,
    pub(crate) connections: usize,
}

/// One line of the panel.
#[derive(Debug, Clone)]
pub(crate) enum Item {
    Running(TunnelId),
    /// Declared in `ssh_config` for this host, not started.
    Suggested(ForwardSpec),
}

#[derive(Debug, Default)]
pub(crate) struct TunnelsPanel {
    /// Live session (connected or connecting) the panel adds tunnels to;
    /// `None` when there is none, and then only `Esc` is offered.
    pub(crate) session: Option<(SessionId, String)>,
    pub(crate) items: Vec<Item>,
    pub(crate) list: ListState,
    /// `Some` while typing a new spec.
    pub(crate) input: Option<String>,
    /// Exposed spec waiting for an explicit `y` (listens beyond loopback).
    pub(crate) confirm: Option<ForwardSpec>,
    /// Last error (bad spec, refused start), cleared by the next key.
    pub(crate) error: Option<ErrorSummary>,
}

/// What the app should do after a key in the panel.
pub(crate) enum PanelOutcome {
    None,
    Close,
    /// Start a tunnel; the flag is true only after the user confirmed an
    /// exposed bind address.
    Start(ForwardSpec, bool),
    Stop(TunnelId),
}

impl TunnelsPanel {
    pub(crate) fn new(session: Option<(SessionId, String)>) -> Self {
        Self {
            session,
            ..Self::default()
        }
    }

    /// Rebuilds the list: running tunnels, then `ssh_config` suggestions that
    /// are not running yet.
    pub(crate) fn rebuild(&mut self, rows: &[TunnelRow], declared: &[ForwardSpec]) {
        let selected = self.list.selected();
        self.items = rows.iter().map(|r| Item::Running(r.id)).collect();
        for spec in declared {
            let running = rows.iter().any(|r| {
                Some(r.session) == self.session.as_ref().map(|(s, _)| *s) && r.spec == *spec
            });
            if !running {
                self.items.push(Item::Suggested(spec.clone()));
            }
        }
        let len = self.items.len();
        self.list
            .select((len > 0).then(|| selected.unwrap_or(0).min(len - 1)));
    }

    /// Starts `spec` at once when it stays on loopback, or asks first.
    fn request_start(&mut self, spec: ForwardSpec) -> PanelOutcome {
        if spec.is_exposed() {
            self.confirm = Some(spec);
            PanelOutcome::None
        } else {
            PanelOutcome::Start(spec, false)
        }
    }

    /// The item under the cursor.
    pub(crate) fn selected(&self) -> Option<&Item> {
        self.items.get(self.list.selected()?)
    }

    /// Keys that do something right now (no inert hints). `Esc` is listed
    /// last; the panel shows it in its border instead.
    pub(crate) fn hints(&self, rows: &[TunnelRow]) -> Vec<(&'static str, &'static str)> {
        if self.confirm.is_some() {
            return vec![("y", "expose"), ("Esc", "cancel")];
        }
        if self.input.is_some() {
            return vec![("↵", "start"), ("Esc", "cancel")];
        }
        let mut hints = Vec::new();
        if self.session.is_some() {
            hints.push(("a", "add"));
        }
        match self.selected() {
            Some(Item::Suggested(_)) => hints.push(("↵", "start")),
            Some(Item::Running(id)) => {
                let failed = rows
                    .iter()
                    .any(|r| r.id == *id && matches!(r.status, TunnelStatus::Failed(_)));
                hints.push(("x", if failed { "dismiss" } else { "stop" }));
            }
            None => {}
        }
        hints.push(("Esc", "close"));
        hints
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        // An error stays until the next key.
        self.error = None;
        if let Some(spec) = self.confirm.take() {
            return match key.code {
                KeyCode::Char('y' | 'Y') => PanelOutcome::Start(spec, true),
                _ => PanelOutcome::None,
            };
        }
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => match parse(input) {
                    Ok(spec) => {
                        self.input = None;
                        return self.request_start(spec);
                    }
                    Err(e) => self.error = Some(e),
                },
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if input.chars().count() < 200 => input.push(c),
                _ => {}
            }
            return PanelOutcome::None;
        }
        let len = self.items.len();
        let current = self.list.selected();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return PanelOutcome::Close,
            KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                self.list
                    .select(Some(current.map_or(0, |i| (i + 1).min(len - 1))));
            }
            KeyCode::Up | KeyCode::Char('k') if len > 0 => {
                self.list
                    .select(Some(current.map_or(0, |i| i.saturating_sub(1))));
            }
            KeyCode::Char('a' | 'n') if self.session.is_some() => {
                self.input = Some(String::from("-L "));
            }
            KeyCode::Enter => {
                if let Some(Item::Suggested(spec)) = current.and_then(|i| self.items.get(i)) {
                    let spec = spec.clone();
                    return self.request_start(spec);
                }
            }
            KeyCode::Char('x' | 'd') | KeyCode::Delete => {
                if let Some(Item::Running(id)) = current.and_then(|i| self.items.get(i)) {
                    return PanelOutcome::Stop(*id);
                }
            }
            _ => {}
        }
        PanelOutcome::None
    }
}

/// `-L 8080:db:5432`, `L 8080 db:5432`, `-R 9000:localhost:3000`, `-D 1080`.
pub(crate) fn parse(input: &str) -> Result<ForwardSpec, ErrorSummary> {
    ForwardSpec::parse_cli(input).map_err(|e| ErrorSummary {
        code: "invalid_forward".into(),
        message: e.to_string(),
        hint: Some(
            "Type -L [bind:]port:host:port, -R [bind:]port:host:port or -D [bind:]port (SOCKS)."
                .into(),
        ),
    })
}

/// Applies an engine event to the rows; returns true when something changed.
pub(crate) fn apply(rows: &mut Vec<TunnelRow>, id: TunnelId, event: &TunnelEvent) -> bool {
    let Some(row) = rows.iter_mut().find(|r| r.id == id) else {
        return false;
    };
    match event {
        TunnelEvent::Listening { bind } => row.status = TunnelStatus::Listening(bind.clone()),
        TunnelEvent::ConnectionOpened { .. } => row.connections = row.connections.saturating_add(1),
        TunnelEvent::ConnectionClosed { .. } => row.connections = row.connections.saturating_sub(1),
        TunnelEvent::Failed { error } => row.status = TunnelStatus::Failed(error.clone()),
        TunnelEvent::Stopped => rows.retain(|r| r.id != id),
        _ => return false,
    }
    true
}

/// Rows for tunnels the engine knows about but the app has not seen start
/// (resynchronisation after a lagged event stream).
pub(crate) fn sync(
    rows: &mut Vec<TunnelRow>,
    known: &[TunnelInfo],
    alias_of: impl Fn(SessionId) -> String,
) {
    rows.retain(|r| {
        known.iter().any(|k| k.id == r.id) || matches!(r.status, TunnelStatus::Failed(_))
    });
    for k in known {
        if !rows.iter().any(|r| r.id == k.id) {
            rows.push(TunnelRow {
                id: k.id,
                session: k.session,
                alias: alias_of(k.session),
                spec: k.spec.clone(),
                status: TunnelStatus::Starting,
                connections: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_input() {
        assert!(matches!(
            parse("-L 8080:db:5432"),
            Ok(ForwardSpec::Local { .. })
        ));
        assert!(matches!(
            parse("L 8080 db:5432"),
            Ok(ForwardSpec::Local { .. })
        ));
        assert!(matches!(
            parse("-R 9000:localhost:3000"),
            Ok(ForwardSpec::Remote { .. })
        ));
        assert!(matches!(parse("-D 1080"), Ok(ForwardSpec::Dynamic { .. })));
        assert!(parse("8080:db:5432").is_err());
        assert!(parse("-D nope").is_err());
        assert!(parse("-L nope").is_err());
    }

    fn press(panel: &mut TunnelsPanel, code: KeyCode) -> PanelOutcome {
        panel.handle_key(KeyEvent::new(code, crossterm::event::KeyModifiers::NONE))
    }

    #[test]
    fn exposed_specs_need_an_explicit_yes() {
        let mut panel = TunnelsPanel::new(Some((SessionId::next(), "web".into())));
        panel.input = Some("-L 0.0.0.0:8080:db:5432".into());
        assert!(matches!(
            press(&mut panel, KeyCode::Enter),
            PanelOutcome::None
        ));
        assert!(panel.confirm.is_some());
        // Anything but `y` cancels.
        assert!(matches!(
            press(&mut panel, KeyCode::Enter),
            PanelOutcome::None
        ));
        assert!(panel.confirm.is_none());
        panel.input = Some("-L 0.0.0.0:8080:db:5432".into());
        press(&mut panel, KeyCode::Enter);
        assert!(matches!(
            press(&mut panel, KeyCode::Char('y')),
            PanelOutcome::Start(_, true)
        ));
        panel.input = Some("-L 8080:db:5432".into());
        assert!(matches!(
            press(&mut panel, KeyCode::Enter),
            PanelOutcome::Start(_, false)
        ));
        // A SOCKS proxy on every interface asks first too.
        panel.input = Some("-D 0.0.0.0:1080".into());
        press(&mut panel, KeyCode::Enter);
        assert!(matches!(panel.confirm, Some(ForwardSpec::Dynamic { .. })));
        assert!(matches!(
            press(&mut panel, KeyCode::Char('y')),
            PanelOutcome::Start(ForwardSpec::Dynamic { .. }, true)
        ));
        panel.input = Some("-D 1080".into());
        assert!(matches!(
            press(&mut panel, KeyCode::Enter),
            PanelOutcome::Start(ForwardSpec::Dynamic { .. }, false)
        ));
    }

    #[test]
    fn errors_clear_on_the_next_key() {
        let mut panel = TunnelsPanel::new(Some((SessionId::next(), "web".into())));
        panel.input = Some("-L nope".into());
        press(&mut panel, KeyCode::Enter);
        let error = panel.error.clone().map(|e| e.code);
        assert_eq!(error.as_deref(), Some("invalid_forward"));
        assert!(panel.input.is_some(), "the spec stays editable");
        press(&mut panel, KeyCode::Backspace);
        assert!(panel.error.is_none());
    }

    #[test]
    fn without_a_live_session_only_close_is_offered() {
        let mut panel = TunnelsPanel::new(None);
        panel.rebuild(&[], &[]);
        assert_eq!(panel.hints(&[]), vec![("Esc", "close")]);
        // `a` is not an inert key that pretends to work.
        assert!(matches!(
            press(&mut panel, KeyCode::Char('a')),
            PanelOutcome::None
        ));
        assert!(panel.input.is_none());
        let live = TunnelsPanel::new(Some((SessionId::next(), "web".into())));
        assert_eq!(live.hints(&[]), vec![("a", "add"), ("Esc", "close")]);
    }
}
