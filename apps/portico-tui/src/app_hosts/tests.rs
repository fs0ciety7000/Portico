//! Hosts created in Portico, end to end on a throwaway HOME (`Paths::rooted`
//! through the core's test harness): add, edit, rename, delete and
//! duplicate through the form and its keys, the read-only refusal, the
//! `Include` banner, renders at 80×24, and one connection with a
//! remembered password against the in-process SSH server (no network).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::EngineEvent;
use portico_core::events::SessionPhase;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tokio::sync::broadcast;

use super::*;
use crate::dialog::ARM_DELAY;
use crate::host_form::{HostField, READ_ONLY_HINT};
use crate::ssh_support::{Behaviour, Env, TestServer, target_config};

fn screen(app: &mut App, cols: u16, rows: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect()
        })
        .collect()
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// An app on a throwaway HOME whose `~/.ssh/config` holds `config`.
struct Lab {
    env: Env,
    events: broadcast::Receiver<EngineEvent>,
    app: App,
}

impl Lab {
    async fn new(config: &str) -> Self {
        Self::from_env(Env::new(config)).await
    }

    /// No system keyring: secrets go to the Portico vault, which does not
    /// exist yet (production KDF; the throwaway HOME holds it).
    async fn without_vault(config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
        std::fs::write(dir.path().join(".ssh/config"), config).unwrap();
        let paths = portico_core::store::Paths::rooted(dir.path());
        let vault = portico_core::vault::VaultStore::open(paths.vault());
        let engine = portico_core::Engine::new(portico_core::EngineConfig {
            paths,
            local_user: "alice".into(),
            event_capacity: 4096,
            secrets: std::sync::Arc::new(vault),
            agent_socket: None,
            connect_timeout: Duration::from_secs(10),
            keepalive: Duration::from_millis(200),
        });
        let events = engine.subscribe();
        Self::from_env(Env {
            dir,
            engine,
            secrets: std::sync::Arc::default(),
            events,
        })
        .await
    }

    async fn from_env(env: Env) -> Self {
        let events = env.engine.subscribe();
        let mut app = App::new(env.engine.clone());
        app.clipboard = false;
        let inventory = env
            .engine
            .load_hosts()
            .await
            .map_err(|e| e.summary().message);
        app.update(Action::HostsLoaded(inventory));
        let mut lab = Self { env, events, app };
        // Let the first `Include` check (if any) come back.
        lab.settle().await;
        lab
    }

    async fn until(&mut self, done: impl FnMut(&App) -> bool) {
        self.app.pump_until(&mut self.events, done).await;
    }

    /// Runs queued actions and events for a short while.
    async fn settle(&mut self) {
        let end = Instant::now() + Duration::from_millis(150);
        self.until(|_| Instant::now() >= end).await;
    }

    fn form_key(&mut self, code: KeyCode) {
        self.app.update(Action::HostFormKey(key(code)));
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.form_key(KeyCode::Char(c));
        }
    }

    fn clear_field(&mut self) {
        self.app.update(Action::HostFormKey(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        )));
    }

    fn focus(&mut self, field: HostField) {
        for _ in 0..16 {
            if self
                .app
                .host_form
                .as_ref()
                .is_some_and(|f| f.focus == field)
            {
                return;
            }
            self.form_key(KeyCode::Tab);
        }
        panic!("no field {field:?}");
    }

    fn selected(&self) -> Option<&str> {
        self.app.selected_host().map(|h| h.alias.as_str())
    }

    fn select(&mut self, alias: &str) {
        self.app.update(Action::SearchClear);
        for c in alias.chars() {
            self.app.update(Action::SearchInput(c));
        }
        self.app.update(Action::BlurSearch);
        assert_eq!(self.selected(), Some(alias));
    }

    fn host(&self, alias: &str) -> Option<&portico_core::hosts::HostEntry> {
        self.app
            .inventory
            .as_ref()?
            .hosts
            .iter()
            .find(|h| h.alias == alias)
    }

    fn hosts_conf(&self) -> String {
        std::fs::read_to_string(self.env.dir.path().join("config/hosts.conf")).unwrap_or_default()
    }

    /// "Add host…" with a password too, saved; waits for the reload.
    async fn add_with_password(&mut self, alias: &str, hostname: &str, password: &str) {
        self.app.update(Action::AddHost);
        self.type_text(alias);
        self.form_key(KeyCode::Tab);
        self.type_text(hostname);
        self.focus(HostField::Password);
        self.type_text(password);
        self.form_key(KeyCode::Enter);
        let alias = alias.to_owned();
        self.until(|app| {
            app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == alias)
        })
        .await;
    }

    /// "Add host…" with an alias and a host name, saved; waits for the
    /// reload that selects it.
    async fn add(&mut self, alias: &str, hostname: &str) {
        self.app.update(Action::AddHost);
        self.type_text(alias);
        self.form_key(KeyCode::Tab);
        self.type_text(hostname);
        self.form_key(KeyCode::Enter);
        let alias = alias.to_owned();
        self.until(|app| {
            app.host_form.is_none()
                && app.selected_host().is_some_and(|h| h.alias == alias)
                && app.pending_select.is_none()
        })
        .await;
    }
}

const USER_CONFIG: &str =
    "Host web\n  HostName 10.0.0.1\n  User deploy\n\nHost bastion\n  HostName 10.0.0.9\n";

#[tokio::test]
async fn add_selects_the_new_host_marks_it_and_offers_the_include_line() {
    let mut lab = Lab::new(USER_CONFIG).await;
    // No Portico host yet: no banner.
    assert!(!lab.app.include_banner());
    assert!(!text(&screen(&mut lab.app, 80, 24)).contains("ssh · scp · git"));

    lab.add("db", "10.0.0.5").await;
    assert_eq!(lab.selected(), Some("db"));
    assert_eq!(lab.host("db").map(|h| h.origin), Some(HostOrigin::Portico));
    assert!(lab.hosts_conf().contains("Host db\n"));
    // `~/.ssh/config` is never written.
    assert_eq!(
        std::fs::read_to_string(lab.env.ssh("config")).unwrap(),
        USER_CONFIG
    );
    lab.until(|app| app.include.active.is_some()).await;
    assert!(lab.app.include_banner());
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    // The list marks it, the details say where it lives.
    assert!(lines.iter().any(|l| l.contains("db ◆")), "{shown}");
    assert!(shown.contains("Portico · e edits"), "{shown}");
    // The banner: the shared sentence, the exact line, never-edits, keys.
    assert!(
        shown.contains("Optional: for ssh, scp and git to see these"),
        "{shown}"
    );
    assert!(shown.contains("Include ~/config/hosts.conf"), "{shown}");
    assert!(shown.contains("Portico never edits that file."), "{shown}");
    assert!(shown.contains(" y  copy"), "{shown}");

    lab.app.update(Action::CopyInclude);
    assert!(
        lab.app
            .toast
            .as_ref()
            .is_some_and(|t| t.message.contains("copied"))
    );
    lab.app.update(Action::HideInclude);
    assert!(!lab.app.include_banner());
    assert!(!text(&screen(&mut lab.app, 80, 24)).contains("ssh · scp · git"));
}

#[tokio::test]
async fn the_banner_goes_once_the_include_line_is_added() {
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.add("db", "10.0.0.5").await;
    lab.until(|app| app.include.active.is_some()).await;
    assert!(lab.app.include_banner());
    let line = lab.env.engine.managed_include_line();
    std::fs::write(lab.env.ssh("config"), format!("{line}\n\n{USER_CONFIG}")).unwrap();
    lab.app.update(Action::Reload);
    lab.until(|app| app.include.active == Some(true)).await;
    assert!(!lab.app.include_banner());
    // Still a Portico host, listed once.
    let count = lab
        .app
        .inventory
        .as_ref()
        .map_or(0, |i| i.hosts.iter().filter(|h| h.alias == "db").count());
    assert_eq!(count, 1);
}

#[tokio::test]
async fn edit_and_rename_keep_the_selection_on_the_host() {
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.add("db", "10.0.0.5").await;
    lab.app.update(Action::EditHost);
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(matches!(&form.mode, FormMode::Edit { original } if original == "db"));
    // Read from hosts.conf off the UI thread: nothing typed meanwhile.
    assert!(form.loading);
    lab.form_key(KeyCode::Char('x'));
    assert_eq!(lab.app.host_form.as_ref().unwrap().alias, "db");
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    // Its own values, never inherited ones.
    assert_eq!(form.hostname, "10.0.0.5");
    assert_eq!(form.user, "");
    lab.clear_field();
    lab.type_text("db-main");
    lab.focus(HostField::User);
    lab.type_text("postgres");
    lab.focus(HostField::Jump);
    lab.type_text("b");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| {
        app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == "db-main")
    })
    .await;
    assert!(lab.host("db").is_none());
    let conf = lab.hosts_conf();
    assert!(conf.contains("Host db-main\n"), "{conf}");
    assert!(conf.contains("User postgres"), "{conf}");
    assert!(conf.contains("ProxyJump bastion"), "{conf}");
    assert!(
        lab.app
            .toast
            .as_ref()
            .is_some_and(|t| t.message.contains("Saved db-main"))
    );
}

/// `Include` at the bottom of `~/.ssh/config`, under a `Host web*` block
/// that sets `User` first: OpenSSH resolves `early`, but the form shows
/// (and saving keeps) what hosts.conf holds. (A literal `Host web` block
/// would declare the alias in `~/.ssh/config` first: a read-only host.)
#[tokio::test]
async fn edit_prefills_from_hosts_conf_not_from_resolved_options() {
    let mut lab = Lab::new("").await;
    lab.add("web", "203.0.113.10").await;
    lab.app.update(Action::EditHost);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    lab.focus(HostField::User);
    lab.type_text("deploy");
    lab.focus(HostField::Port);
    lab.type_text("2222");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.is_none() && app.pending_select.is_none())
        .await;
    let include = lab.env.engine.managed_include_line();
    std::fs::write(
        lab.env.dir.path().join(".ssh/config"),
        format!("Host web*\n    User early\n\n{include}\n"),
    )
    .unwrap();
    lab.app.on_hosts_changed();
    lab.until(|app| {
        app.inventory.as_ref().is_some_and(|i| {
            i.hosts
                .iter()
                .any(|h| h.alias == "web" && h.resolved.user.as_deref() == Some("early"))
        })
    })
    .await;
    lab.select("web");

    lab.app.update(Action::EditHost);
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(matches!(&form.mode, FormMode::Edit { original } if original == "web"));
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    assert_eq!(form.user, "deploy", "the hosts.conf value, not `early`");
    assert_eq!(form.port, "2222");
    assert_eq!(form.hostname, "203.0.113.10");
    assert!(!form.dirty());
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("deploy"), "{shown}");
    assert!(!shown.contains("early"), "{shown}");

    // Saving another change keeps the user.
    lab.focus(HostField::HostName);
    lab.clear_field();
    lab.type_text("203.0.113.11");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.is_none() && app.pending_select.is_none())
        .await;
    let conf = lab.hosts_conf();
    assert!(conf.contains("HostName 203.0.113.11"), "{conf}");
    assert!(conf.contains("User deploy"), "{conf}");
    assert!(conf.contains("Port 2222"), "{conf}");
    assert!(!conf.contains("early"), "{conf}");
}

#[tokio::test]
async fn edit_of_a_host_gone_from_hosts_conf_closes_with_a_toast() {
    let mut lab = Lab::new("").await;
    lab.add("db", "10.0.0.5").await;
    lab.app.update(Action::EditHost);
    lab.app.update(Action::HostFormManaged {
        alias: "db".into(),
        result: Ok(None),
    });
    assert!(lab.app.host_form.is_none());
    assert!(
        lab.app
            .toast
            .as_ref()
            .is_some_and(|t| t.message == "db is no longer in hosts.conf")
    );
}

#[tokio::test]
async fn engine_refusals_show_inline_with_their_hint() {
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.app.update(Action::AddHost);
    // Uppercase is refused, under the alias.
    lab.type_text("Web");
    lab.form_key(KeyCode::Tab);
    lab.type_text("10.0.0.7");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.busy))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    assert_eq!(form.focus, HostField::Alias);
    assert!(
        matches!(&form.error, Some((Some(HostField::Alias), e)) if e.code == "host_invalid_alias")
    );
    lab.clear_field();
    lab.focus(HostField::HostName);
    lab.clear_field();
    lab.focus(HostField::Alias);
    // `web` is taken by ~/.ssh/config.
    lab.type_text("web");
    lab.form_key(KeyCode::Tab);
    lab.type_text("10.0.0.7");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.busy))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    assert_eq!(form.focus, HostField::Alias);
    assert!(
        matches!(&form.error, Some((Some(HostField::Alias), e)) if e.code == "host_alias_taken")
    );
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("Pick another alias"), "{shown}");

    // A bad host name goes under HostName.
    lab.clear_field();
    lab.type_text("api");
    lab.focus(HostField::HostName);
    lab.clear_field();
    lab.type_text("-bad host");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.busy))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(
        matches!(&form.error, Some((Some(HostField::HostName), e)) if e.code == "host_invalid_hostname")
    );
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    let field = lines.iter().position(|l| l.contains("HostName"));
    let message = lines.iter().position(|l| l.contains("Invalid host name"));
    assert!(field < message && message.is_some(), "{shown}");
    assert!(shown.contains("Use a DNS name or an IP address"), "{shown}");
    assert!(lab.hosts_conf().is_empty());

    // A port out of range: the message alone (its hint only repeats it).
    lab.clear_field();
    lab.type_text("10.0.0.7");
    lab.focus(HostField::Port);
    lab.type_text("70000");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.busy))
        .await;
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(
        matches!(&form.error, Some((Some(HostField::Port), e)) if e.code == "host_invalid_port" && e.hint.is_some())
    );
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("Invalid port 70000"), "{shown}");
    assert!(!shown.contains("A port is a number"), "{shown}");
}

#[tokio::test]
async fn ssh_config_hosts_are_read_only_and_duplicate() {
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.select("web");
    lab.app.update(Action::EditHost);
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    assert!(
        matches!(
            lab.app.host_form.as_ref().map(|f| &f.mode),
            Some(FormMode::ReadOnly { .. })
        ),
        "{shown}"
    );
    // The shared hint, on two lines at 80 columns.
    let joined = lines
        .iter()
        .map(|l| l.trim_matches(|c: char| c == ' ' || c == '│'))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains(READ_ONLY_HINT), "{shown}");
    assert!(READ_ONLY_HINT.starts_with("Defined in ~/.ssh/config"));
    assert!(shown.contains("~/.ssh/config · read-only"), "{shown}");
    // The title names the host (no ellipsis); the keys as in the GUI.
    assert!(shown.contains("╭ Edit web ─"), "{shown}");
    assert!(shown.contains(" D  duplicate "), "{shown}");
    assert!(shown.contains(" t  tags and notes"), "{shown}");
    assert!(
        lines
            .get(23)
            .is_some_and(|l| l.contains("D  duplicate  t  tags and notes  Esc  close")),
        "{shown}"
    );

    // The engine refuses to edit it, whatever the UI does.
    let refused = lab
        .env
        .engine
        .update_host(
            "web",
            portico_core::managed_hosts::HostDraft {
                alias: "web".into(),
                hostname: "10.0.0.1".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.summary().code);
    assert_eq!(refused.err().as_deref(), Some("host_read_only"));

    // Delete on it: the hint, no confirmation.
    lab.app.update(Action::CloseOverlay);
    lab.app.update(Action::DeleteHost);
    assert!(lab.app.dialogs.is_empty());
    let toast = lab.app.toast.clone().unwrap();
    assert_eq!(toast.hint.as_deref(), Some(READ_ONLY_HINT));
    // The alias keeps its case: never "Web is read-only".
    assert_eq!(toast.shown(), "web is read-only");
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("web is read-only"), "{shown}");
    assert!(!shown.contains("Web is read-only"), "{shown}");

    // `t` from the read-only view: its tags, group and notes.
    lab.app.update(Action::EditHost);
    lab.form_key(KeyCode::Char('t'));
    assert_eq!(lab.app.overlay, Some(Overlay::Edit));
    lab.app.update(Action::CloseOverlay);

    // Duplicate (`D` from the list): the copy is a Portico host, selected.
    lab.app.update(Action::DuplicateHost);
    assert_eq!(
        lab.app.host_form.as_ref().map(|f| f.alias.as_str()),
        Some("web-copy")
    );
    lab.form_key(KeyCode::Enter);
    lab.until(|app| {
        app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == "web-copy")
    })
    .await;
    assert_eq!(
        lab.host("web-copy").map(|h| h.origin),
        Some(HostOrigin::Portico)
    );
    let conf = lab.hosts_conf();
    assert!(conf.contains("HostName 10.0.0.1"), "{conf}");
    assert!(conf.contains("User deploy"), "{conf}");
}

#[tokio::test]
async fn delete_asks_armed_then_selects_the_neighbour() {
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.add("db", "10.0.0.5").await;
    lab.add("lab", "10.0.0.6").await;
    assert!(lab.app.select_alias("db"));
    lab.app.update(Action::DeleteHost);
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    assert!(shown.contains("Delete host db?"), "{shown}");
    assert!(
        shown.contains("Delete db (alice@10.0.0.5:22) and its tags and notes. Its")
            && shown.contains("Open sessions stay connected. This cannot"),
        "{shown}"
    );
    assert!(shown.contains("n / Esc  cancel"), "{shown}");
    // Too soon: ignored. Then a key that is not an answer is ignored too,
    // and `n` cancels.
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    assert_eq!(lab.app.dialogs.len(), 1);
    arm(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Enter)));
    assert_eq!(lab.app.dialogs.len(), 1);
    lab.app.update(Action::DialogKey(key(KeyCode::Char('n'))));
    assert!(lab.app.dialogs.is_empty());
    assert!(lab.host("db").is_some());

    lab.app.update(Action::DeleteHost);
    arm(&mut lab.app);
    lab.app.update(Action::DialogKey(key(KeyCode::Char('y'))));
    lab.until(|app| {
        app.inventory
            .as_ref()
            .is_some_and(|i| !i.hosts.iter().any(|h| h.alias == "db"))
            && app.pending_select.is_none()
    })
    .await;
    assert_eq!(lab.selected(), Some("lab"));
    assert!(!lab.hosts_conf().contains("Host db\n"));
    assert!(
        lab.app
            .toast
            .as_ref()
            .is_some_and(|t| t.message == "Deleted db")
    );
}

/// The front confirmation, armed as if opened long enough ago.
fn arm(app: &mut App) {
    let long_ago = Instant::now()
        .checked_sub(ARM_DELAY * 2)
        .unwrap_or_else(Instant::now);
    match app.dialogs.front_mut() {
        Some(Dialog::Confirm(c)) => c.armed = Some(long_ago),
        Some(Dialog::Secret(d)) => d.opened_at = long_ago,
        _ => {}
    }
}

#[tokio::test]
async fn hosts_changed_elsewhere_reloads_in_place() {
    let mut lab = Lab::new(USER_CONFIG).await;
    // Not the first host: a reload that lost the selection would show.
    assert_eq!(lab.selected(), Some("web"));
    assert!(lab.app.select_alias("bastion"));
    // Another UI creates a host: the list reloads, `bastion` stays selected.
    lab.env
        .engine
        .create_host(portico_core::managed_hosts::HostDraft {
            alias: "api".into(),
            hostname: "10.0.0.8".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    lab.until(|app| {
        app.inventory
            .as_ref()
            .is_some_and(|i| i.hosts.iter().any(|h| h.alias == "api"))
    })
    .await;
    assert_eq!(lab.selected(), Some("bastion"));
}

#[tokio::test]
async fn renders_fit_80x24() {
    let mut lab = Lab::new(USER_CONFIG).await;
    // The form, empty.
    lab.app.update(Action::AddHost);
    lab.settle().await;
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    for needle in [
        " Add host ",
        "Alias",
        "HostName",
        "User",
        "Port",
        "Key",
        "ProxyJump",
        "‹ none · agent and default keys ›",
        "Tags",
        "Group",
        "Favorite",
        "Notes",
        "Password",
        "↵  save",
        "Portico never edits ~/.ssh/config",
        "Esc cancel",
    ] {
        assert!(shown.contains(needle), "missing `{needle}`:\n{shown}");
    }
    assert!(lines.get(23).is_some_and(|l| l.contains("HOST")), "{shown}");
    // The password is a fixed mask, never the text.
    lab.focus(HostField::Password);
    lab.type_text("hunter2");
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(
        shown.contains("••••••••") && !shown.contains("hunter2"),
        "{shown}"
    );
    assert!(shown.contains("remembered in"), "{shown}");
    // Esc with changes: the question, in place of the keys.
    lab.form_key(KeyCode::Esc);
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("Discard your changes?"), "{shown}");
    lab.form_key(KeyCode::Char('y'));
    assert!(lab.app.host_form.is_none());

    // The list with its badges.
    lab.add("db", "10.0.0.5").await;
    lab.until(|app| app.include.active.is_some()).await;
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    assert!(lines.iter().any(|l| l.contains("db ◆")), "{shown}");
    assert!(lines.iter().any(|l| l.contains("● web ")), "{shown}");
    assert!(!lines.iter().any(|l| l.contains("web ◆")), "{shown}");
    assert!(shown.contains(" a  add host"), "{shown}");
}

#[tokio::test]
async fn a_remembered_password_connects_without_a_prompt() {
    let server = TestServer::start(Behaviour {
        password: Some(("alice".into(), "hunter2".into())),
        pty_echo: true,
        ..Behaviour::default()
    })
    .await;
    // `~/.ssh/config` only knows another host.
    let mut lab = Lab::new(&target_config("other", 1, "")).await;
    lab.env.trust(&server);
    lab.app.update(Action::AddHost);
    lab.type_text("pw");
    lab.form_key(KeyCode::Tab);
    lab.type_text("127.0.0.1");
    lab.form_key(KeyCode::Tab);
    lab.type_text("alice");
    lab.form_key(KeyCode::Tab);
    lab.type_text(&server.port.to_string());
    lab.focus(HostField::Password);
    lab.type_text("hunter2");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| {
        app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == "pw")
    })
    .await;
    // Never in the file.
    assert!(!lab.hosts_conf().contains("hunter2"));

    lab.app.update(Action::Connect);
    lab.until(|app| {
        assert!(
            app.dialogs.is_empty(),
            "a prompt was shown: {:?}",
            app.dialogs
        );
        app.session_for("pw").is_some_and(|s| {
            matches!(
                s.phase,
                SessionPhase::Connected | SessionPhase::Failed { .. }
            )
        })
    })
    .await;
    assert_eq!(
        lab.app.session_for("pw").map(|s| s.phase.clone()),
        Some(SessionPhase::Connected)
    );
    assert_eq!(
        server
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

/// The password's destination note: under a typed password (add), under
/// "Keep" (edit), never under "Forget"; the form still fits 80×24.
#[tokio::test]
async fn password_note_says_where_it_is_sent() {
    const NOTE: &str = "Sent automatically to the server that answers at this";
    let mut lab = Lab::new(USER_CONFIG).await;
    lab.app.update(Action::AddHost);
    lab.settle().await;
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(!shown.contains(NOTE), "nothing typed yet:\n{shown}");
    lab.focus(HostField::Password);
    lab.type_text("hunter2");
    let shown = text(&screen(&mut lab.app, 80, 24));
    // Aligned with the input, after the label column, wrapped.
    assert!(
        shown.contains(&format!("{}{NOTE}", " ".repeat(11))),
        "{shown}"
    );
    assert!(shown.contains("verified; then tied to that"), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(shown.contains("↵  save"), "{shown}");
    lab.form_key(KeyCode::Esc);
    lab.form_key(KeyCode::Char('y'));
    assert!(lab.app.host_form.is_none());

    // Edit with nothing remembered: "Set", an empty field, no Keep.
    lab.add("db", "10.0.0.5").await;
    lab.app.update(Action::EditHost);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains("No password remembered"), "{shown}");
    assert!(!shown.contains("Keep"), "{shown}");
    assert!(!shown.contains(NOTE), "{shown}");
    let form = lab.app.host_form.as_ref().unwrap();
    assert_eq!(form.password_mode_label(), "Set");
    assert!(!form.fields().contains(&HostField::PasswordMode));
    assert!(!form.dirty());
    lab.form_key(KeyCode::Esc);
    assert!(lab.app.host_form.is_none(), "nothing changed: closed");

    // Edit with a password remembered: "Keep" says it and where it goes,
    // "Forget" does not.
    lab.add_with_password("pw", "10.0.0.7", "hunter2").await;
    lab.app.update(Action::EditHost);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(
        shown.contains("‹ Keep ›  A password is remembered"),
        "{shown}"
    );
    assert!(shown.contains(NOTE), "{shown}");
    assert!(shown.contains("↵  save"), "{shown}");
    lab.focus(HostField::PasswordMode);
    lab.form_key(KeyCode::Left);
    assert_eq!(
        lab.app.host_form.as_ref().map(|f| f.password_mode),
        Some(crate::host_form::PasswordMode::Forget)
    );
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(!shown.contains(NOTE), "{shown}");
    lab.form_key(KeyCode::Right);
    // A new destination: the password is forgotten, the note says so in
    // place of where it goes.
    lab.focus(HostField::HostName);
    lab.type_text("9");
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(!shown.contains(NOTE), "{shown}");
    assert!(!shown.contains("A password is remembered"), "{shown}");
    assert!(
        shown.contains("Changing the server or user forgets the"),
        "{shown}"
    );
    assert!(
        shown.contains("Choose Change to set one for the new"),
        "{shown}"
    );
    assert!(shown.contains("↵  save"), "{shown}");
    // Back to the same server: Keep says where it goes again.
    lab.form_key(KeyCode::Backspace);
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(shown.contains(NOTE), "{shown}");
}

fn dialog_key(lab: &mut Lab, code: KeyCode) {
    lab.app.update(Action::DialogKey(key(code)));
}

/// B1: a password typed with no vault. The save chains to "Create the
/// vault"; "not now" goes back to the form with the password still typed
/// and the refusal under it; creating the vault saves the host again, its
/// password remembered (never in `hosts.conf`).
#[tokio::test]
async fn a_password_with_no_vault_creates_it_then_saves() {
    let mut lab = Lab::without_vault(USER_CONFIG).await;
    lab.app.update(Action::AddHost);
    lab.type_text("pw");
    lab.form_key(KeyCode::Tab);
    lab.type_text("10.0.0.7");
    lab.focus(HostField::Password);
    lab.type_text("hunter2");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| !app.dialogs.is_empty()).await;
    let Some(Dialog::Secret(dialog)) = lab.app.dialogs.front() else {
        panic!("the vault dialog: {:?}", lab.app.dialogs)
    };
    assert!(dialog.host_save);
    assert!(matches!(
        dialog.purpose,
        SecretPurpose::VaultCreate { offered: true }
    ));
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(form.password_typed() && !form.busy && form.error.is_none());
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    assert!(shown.contains("Create the vault"), "{shown}");
    assert!(
        shown.contains("This host's password is remembered in"),
        "{shown}"
    );
    assert!(shown.contains("Esc  not now"), "{shown}");
    assert!(shown.contains("create & save"), "{shown}");
    assert!(!shown.contains("hunter2"), "{shown}");

    // "Not now": the form again, the password kept, the refusal under it.
    arm(&mut lab.app);
    dialog_key(&mut lab, KeyCode::Esc);
    assert!(lab.app.dialogs.is_empty());
    let form = lab.app.host_form.as_ref().unwrap();
    assert!(form.password_typed(), "the password is still typed");
    assert_eq!(form.focus, HostField::Password);
    assert!(matches!(
        &form.error,
        Some((Some(HostField::Password), e)) if e.code == "vault_missing"
    ));
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(
        shown.contains("Create the vault (Keys, then V) to"),
        "{shown}"
    );
    // The refusal takes the notes' place: the form still fits 80×24.
    assert!(!shown.contains("Sent automatically"), "{shown}");
    assert!(shown.contains("↵  save"), "{shown}");
    assert!(lab.hosts_conf().is_empty(), "nothing saved");

    // Save again: the dialog again; create the vault, the save follows.
    lab.form_key(KeyCode::Enter);
    lab.until(|app| !app.dialogs.is_empty()).await;
    arm(&mut lab.app);
    for c in "correct horse battery".chars() {
        dialog_key(&mut lab, KeyCode::Char(c));
    }
    dialog_key(&mut lab, KeyCode::Tab);
    for c in "correct horse battery".chars() {
        dialog_key(&mut lab, KeyCode::Char(c));
    }
    dialog_key(&mut lab, KeyCode::Enter);
    lab.until(|app| {
        app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == "pw")
    })
    .await;
    assert!(lab.app.dialogs.is_empty());
    assert_eq!(
        lab.app.toast.as_ref().map(|t| t.message.as_str()),
        Some("Added pw · hosts.conf")
    );
    assert_eq!(
        lab.env.engine.vault_status().await.unwrap(),
        portico_core::vault::VaultStatus::Unlocked
    );
    let details = lab
        .env
        .engine
        .managed_host_details("pw")
        .await
        .unwrap()
        .unwrap();
    assert!(details.password_remembered);
    assert!(lab.hosts_conf().contains("HostName 10.0.0.7"));
    assert!(!lab.hosts_conf().contains("hunter2"));
}

/// A Portico host that came from an import says so: details, the edit
/// title, and what an edit does to later imports.
#[tokio::test]
async fn an_imported_portico_host_shows_its_provenance() {
    let mut lab = Lab::new("").await;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/portico-core/tests/fixtures/termix/termix_export.json");
    let plan = lab
        .env
        .engine
        .plan_import(&fixture, portico_core::import::ImportOptions::default())
        .await
        .unwrap();
    lab.env
        .engine
        .apply_import(plan, portico_core::import::ImportOptions::default())
        .await
        .unwrap();
    lab.app.on_hosts_changed();
    lab.until(|app| app.inventory.as_ref().is_some_and(|i| !i.hosts.is_empty()))
        .await;
    lab.select("hbd");
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(
        shown.contains("Origin         Portico · from Termix"),
        "{shown}"
    );
    lab.app.update(Action::EditHost);
    lab.until(|app| app.host_form.as_ref().is_some_and(|f| !f.loading))
        .await;
    let shown = text(&screen(&mut lab.app, 80, 24));
    assert!(
        shown.contains("╭ Edit hbd · Portico · from Termix ─"),
        "{shown}"
    );
    assert!(
        shown.contains("Editing it keeps later imports from updating it."),
        "{shown}"
    );
    assert!(shown.contains("↵  save"), "{shown}");
}

/// First connection to a host with a remembered password: the warning
/// names the account (`user@host:port`), the target is `host:port`, and
/// the status bar gives the two answers.
#[tokio::test]
async fn new_host_key_names_the_account_its_password_goes_to() {
    let server = TestServer::start(Behaviour {
        password: Some(("bob".into(), "hunter2".into())),
        ..Behaviour::default()
    })
    .await;
    let mut lab = Lab::new(&target_config("other", 1, "")).await;
    lab.app.update(Action::AddHost);
    lab.type_text("tofu");
    lab.form_key(KeyCode::Tab);
    lab.type_text("127.0.0.1");
    lab.form_key(KeyCode::Tab);
    lab.type_text("bob");
    lab.form_key(KeyCode::Tab);
    lab.type_text(&server.port.to_string());
    lab.focus(HostField::Password);
    lab.type_text("hunter2");
    lab.form_key(KeyCode::Enter);
    lab.until(|app| {
        app.host_form.is_none() && app.selected_host().is_some_and(|h| h.alias == "tofu")
    })
    .await;
    lab.app.update(Action::Connect);
    lab.until(|app| matches!(app.dialogs.front(), Some(Dialog::Prompt(_))))
        .await;
    let account = format!("bob@127.0.0.1:{}", server.port);
    let Some(Dialog::Prompt(prompt)) = lab.app.dialogs.front() else {
        unreachable!()
    };
    assert_eq!(prompt.account.as_deref(), Some(account.as_str()));
    let lines = screen(&mut lab.app, 80, 24);
    let shown = text(&lines);
    let joined = lines
        .iter()
        .map(|l| l.trim_matches(|c: char| c == ' ' || c == '│'))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        shown.contains(&format!("First connection to 127.0.0.1:{}.", server.port)),
        "{shown}"
    );
    assert!(
        joined.contains(&format!(
            "▲ The password remembered for {account} will be sent to this server once you accept its key."
        )),
        "{shown}"
    );
    assert!(
        lines
            .get(23)
            .is_some_and(|l| l.contains("HOST KEY") && l.contains("y  trust  n  refuse")),
        "{shown}"
    );
    // Refused: nothing sent.
    if let Some(Dialog::Prompt(p)) = lab.app.dialogs.front_mut() {
        p.opened_at = Instant::now()
            .checked_sub(ARM_DELAY * 2)
            .unwrap_or_else(Instant::now);
    }
    lab.app.update(Action::DialogKey(key(KeyCode::Char('n'))));
    lab.until(|app| {
        app.session_for("tofu")
            .is_some_and(|s| matches!(s.phase, SessionPhase::Failed { .. }))
    })
    .await;
    assert_eq!(
        server
            .behaviour
            .logins
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

/// A refusal that starts with an alias keeps the alias's case in the toast.
#[test]
fn alias_first_errors_keep_their_case() {
    let mut app = App::new(Env::new("").engine.clone());
    app.notify_error_as_written(ErrorSummary {
        code: "host_in_use".into(),
        message: "db is the ProxyJump of web".into(),
        hint: None,
    });
    assert_eq!(
        app.toast.as_ref().map(crate::app::Toast::shown).as_deref(),
        Some("db is the ProxyJump of web")
    );
    app.notify_error(ErrorSummary {
        code: "other".into(),
        message: "something failed".into(),
        hint: None,
    });
    assert_eq!(
        app.toast.as_ref().map(crate::app::Toast::shown).as_deref(),
        Some("Something failed")
    );
    assert!(ALIAS_FIRST_CODES.contains(&"host_read_only"));
}
