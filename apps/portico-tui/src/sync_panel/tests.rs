//! The Sync panel's state: words, conflict choices, passphrase forms and
//! the decisions on received hosts and snippets (bound to what is shown,
//! armed, never while a reload is in flight).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portico_core::sync::{
    ConflictSide, SyncHostValue, SyncOverview, SyncSnippetValue, generate_passphrase,
};
use secrecy::ExposeSecret;

use super::*;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(form: &mut PassForm, text: &str) {
    for c in text.chars() {
        form.handle_key(ch(c));
    }
}

pub(crate) fn host(hostname: &str) -> SyncHostValue {
    SyncHostValue {
        hostname: hostname.into(),
        user: Some("deploy".into()),
        port: Some(22),
        identity_file: Some("~/.ssh/id_ed25519".into()),
        proxy_jump: None,
    }
}

pub(crate) fn pending(alias: &str, hostname: &str, hash: &str) -> PendingHostChange {
    PendingHostChange {
        alias: alias.into(),
        new: false,
        current: Some(host("10.0.0.5")),
        incoming: host(hostname),
        renamed_from: None,
        devices: vec!["laptop (3f9c0ae1)".into()],
        destination_hash: hash.into(),
    }
}

pub(crate) fn unconfirmed(id: &str, command: &str, hash: &str) -> UnconfirmedSnippet {
    UnconfirmedSnippet {
        id: id.into(),
        snippet: SyncSnippetValue {
            name: format!("{id} name"),
            command: command.into(),
            description: None,
            run: true,
        },
        hash: hash.into(),
    }
}

fn idle() -> SyncOverview {
    SyncOverview {
        status: SyncStatus::Idle {
            last_pass: 0,
            devices: 2,
            conflicts: 0,
            issues: 0,
            pending_confirmations: 1,
        },
        folder: None,
        device: None,
        remember_key: true,
    }
}

/// A time well after `at` (decisions armed).
fn later(at: Instant) -> Instant {
    at + ARM_DELAY + Duration::from_millis(10)
}

#[test]
fn status_words() {
    let now = OffsetDateTime::from_unix_timestamp(10_000).unwrap();
    let label = |status: SyncStatus| status_label(&status, now);
    assert_eq!(
        label(SyncStatus::Disabled),
        ("Sync off".into(), Tone::Neutral)
    );
    assert_eq!(label(SyncStatus::Locked).0, "Sync locked");
    assert_eq!(
        label(SyncStatus::Idle {
            last_pass: 10_000 - 120,
            devices: 2,
            conflicts: 0,
            issues: 0,
            pending_confirmations: 0
        })
        .0,
        "Synced 2 min ago"
    );
    assert_eq!(
        label(SyncStatus::Idle {
            last_pass: 10_000,
            devices: 2,
            conflicts: 0,
            issues: 0,
            pending_confirmations: 0
        })
        .0,
        "Synced just now"
    );
    assert_eq!(label(SyncStatus::Syncing), ("Syncing…".into(), Tone::Busy));
    assert_eq!(
        label(SyncStatus::Waiting {
            reason: WaitReason::PartialSync,
            device: Some("desk (77a1b2c3)".into())
        })
        .0,
        "Waiting for desk (77a1b2c3)"
    );
    assert_eq!(
        label(SyncStatus::FolderUnavailable {
            reason: FolderReason::Missing
        }),
        ("Sync folder unavailable".into(), Tone::Warning)
    );
    assert_eq!(
        label(SyncStatus::Alert {
            kind: AlertKind::Fork,
            device: None
        })
        .1,
        Tone::Error
    );
    // Alerts name the device and say what to do.
    let (what, hint) = alert_text(AlertKind::PassphraseChanged, Some("desk (77a1b2c3)"));
    assert!(what.contains("desk (77a1b2c3)"));
    assert!(hint.contains("acknowledge"));
}

fn side(index: usize, local: bool, value: SyncValue) -> ConflictSide {
    ConflictSide {
        index,
        devices: vec![format!("dev{index} (0000000{index})")],
        local,
        at: 0,
        value,
    }
}

#[test]
fn tags_preselect_union_and_snippets_keep_both() {
    let tags = SyncConflict {
        id: "c1".into(),
        subject: SyncSubject::HostMeta {
            alias: "web".into(),
            field: "tags".into(),
        },
        sides: vec![
            side(0, false, SyncValue::Tags(vec!["a".into()])),
            side(1, true, SyncValue::Tags(vec!["b".into()])),
        ],
    };
    assert_eq!(
        choices(&tags),
        vec![
            Choice::Union,
            Choice::Pick { index: 0 },
            Choice::Pick { index: 1 }
        ]
    );
    let snippet = |name: &str| {
        SyncValue::Snippet(SyncSnippetValue {
            name: name.into(),
            command: "ls".into(),
            description: None,
            run: false,
        })
    };
    let snippets = SyncConflict {
        id: "c2".into(),
        subject: SyncSubject::Snippet { id: "s".into() },
        sides: vec![side(0, false, snippet("x")), side(1, true, snippet("y"))],
    };
    // The local side keeps the id.
    assert_eq!(
        choices(&snippets).first(),
        Some(&Choice::KeepBoth { keep: 1 })
    );
    let hosts = SyncConflict {
        id: "c3".into(),
        subject: SyncSubject::Host {
            alias: "web".into(),
        },
        sides: vec![
            side(0, false, SyncValue::Host(host("a"))),
            side(1, true, SyncValue::Host(host("b"))),
        ],
    };
    assert_eq!(choices(&hosts).first(), Some(&Choice::Pick { index: 0 }));

    // ↵ resolves the selected conflict with the choice on screen.
    let now = Instant::now();
    let mut panel = SyncPanel::new(SyncTab::Conflicts, "me".into(), now);
    panel.set_conflicts(Ok(vec![tags, hosts]));
    let data = SyncData::default();
    match panel.handle_key(key(KeyCode::Enter), &data, now) {
        SyncOutcome::Resolve { id, resolution } => {
            assert_eq!(id, "c1");
            assert_eq!(resolution, SyncResolution::Union);
        }
        other => panic!("{other:?}"),
    }
    panel.handle_key(ch(']'), &data, now);
    panel.handle_key(ch('j'), &data, now);
    match panel.handle_key(key(KeyCode::Enter), &data, now) {
        SyncOutcome::Resolve { id, resolution } => {
            assert_eq!(id, "c3");
            assert_eq!(resolution, SyncResolution::Pick { index: 1 });
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn typed_passphrase_needs_a_matching_confirmation() {
    let mut form = PassForm::new(PassKind::Create, "laptop".into());
    assert_eq!(form.handle_key(key(KeyCode::Enter)), PassOutcome::None);
    assert_eq!(
        form.problem.as_ref().map(|(f, ..)| *f),
        Some(PassField::New)
    );
    typed(&mut form, "correct horse battery staple");
    assert!(form.strength.is_some(), "the meter follows the typing");
    form.handle_key(key(KeyCode::Tab));
    typed(&mut form, "correct horse battery");
    assert_eq!(form.handle_key(key(KeyCode::Enter)), PassOutcome::None);
    assert_eq!(
        form.problem.as_ref().map(|(f, ..)| *f),
        Some(PassField::Confirm)
    );
    typed(&mut form, " staple");
    assert_eq!(form.handle_key(key(KeyCode::Enter)), PassOutcome::Submit);
    match form.take_new() {
        NewPassphrase::Typed(secret) => {
            assert_eq!(secret.expose_secret(), "correct horse battery staple");
        }
        NewPassphrase::Generated(_) => panic!("typed"),
    }
    // Wiped from the form.
    assert!(form.new.is_empty() && form.confirm.is_empty());
    // Never in `Debug`.
    typed(&mut form, "secret");
    assert!(!format!("{form:?}").contains("secret"));
}

#[test]
fn generated_passphrase_is_shown_and_must_be_written_down() {
    let mut form = PassForm::new(PassKind::Create, "laptop".into());
    assert_eq!(form.handle_key(ctrl('g')), PassOutcome::Generate);
    form.set_generated(generate_passphrase().unwrap());
    let words = form.generated_text().unwrap().expose_secret().to_owned();
    assert_eq!(words.split(' ').count(), 6);
    assert!(form.strength.is_some_and(|s| s.acceptable));
    assert_eq!(form.focused(), Some(PassField::New));
    assert_eq!(form.handle_key(ch('c')), PassOutcome::Copy);
    assert_eq!(form.handle_key(key(KeyCode::Enter)), PassOutcome::None);
    assert_eq!(
        form.problem.as_ref().map(|(f, ..)| *f),
        Some(PassField::Confirm)
    );
    form.handle_key(ch(' '));
    assert!(form.wrote_down);
    assert_eq!(form.handle_key(key(KeyCode::Enter)), PassOutcome::Submit);
    assert!(matches!(form.take_new(), NewPassphrase::Generated(_)));
    assert!(form.generated.is_none());

    // Backspace on the generated one goes back to typing one.
    form.set_generated(generate_passphrase().unwrap());
    form.handle_key(key(KeyCode::Backspace));
    assert!(form.generated.is_none());
}

#[test]
fn core_refusals_land_under_their_field() {
    let mut form = PassForm::new(PassKind::Create, "laptop".into());
    form.busy = true;
    form.refused(ErrorSummary {
        code: "sync_passphrase_weak".into(),
        message: "the sync passphrase is too weak".into(),
        hint: Some("Use at least five random words.".into()),
    });
    assert!(!form.busy);
    assert_eq!(form.focused(), Some(PassField::New));
    let (field, message, hint) = form.problem.clone().unwrap();
    assert_eq!(field, PassField::New);
    assert!(message.contains("weak"));
    assert!(hint.is_some());
}

#[test]
fn host_decisions_are_armed_bound_to_the_hash_and_wait_for_reloads() {
    let t0 = Instant::now();
    let mut data = SyncData {
        overview: Some(idle()),
        ..SyncData::default()
    };
    data.set_pending(
        vec![
            pending("web", "10.0.0.9", "h1"),
            pending("db", "10.0.1.1", "h2"),
        ],
        t0,
    );
    let mut panel = SyncPanel::new(SyncTab::Hosts, "me".into(), t0);
    // Too soon: typed for something else.
    assert!(matches!(
        panel.handle_key(ch('y'), &data, t0),
        SyncOutcome::None
    ));
    let t1 = later(t0);
    match panel.handle_key(ch('y'), &data, t1) {
        SyncOutcome::ConfirmHost { alias, hash } => {
            assert_eq!((alias.as_str(), hash.as_str()), ("web", "h1"));
        }
        other => panic!("{other:?}"),
    }
    // Moving re-arms: the new item must be on screen long enough.
    panel.handle_key(ch('j'), &data, t1);
    assert!(matches!(
        panel.handle_key(ch('n'), &data, t1),
        SyncOutcome::None
    ));
    let t2 = later(t1);
    match panel.handle_key(ch('n'), &data, t2) {
        SyncOutcome::RejectHost { alias, hash } => {
            assert_eq!((alias.as_str(), hash.as_str()), ("db", "h2"));
        }
        other => panic!("{other:?}"),
    }
    // `SyncPendingChanged`: nothing is decided until the reload is back.
    data.stale_hosts = true;
    assert!(matches!(
        panel.handle_key(ch('n'), &data, later(t2)),
        SyncOutcome::None
    ));
    // The reload brings new content: armed again from then on.
    let t3 = later(t2);
    data.stale_hosts = false;
    data.set_pending(
        vec![
            pending("web", "10.0.0.9", "h1"),
            pending("db", "10.9.9.9", "h3"),
        ],
        t3,
    );
    assert!(matches!(
        panel.handle_key(ch('y'), &data, t3),
        SyncOutcome::None
    ));
    match panel.handle_key(ch('y'), &data, later(t3)) {
        SyncOutcome::ConfirmHost { hash, .. } => assert_eq!(hash, "h3", "the hash shown now"),
        other => panic!("{other:?}"),
    }
    // Same content again: no re-arming needed.
    let t4 = later(t3);
    data.set_pending(
        vec![
            pending("web", "10.0.0.9", "h1"),
            pending("db", "10.9.9.9", "h3"),
        ],
        t4,
    );
    assert_eq!(data.changed_at, Some(t3));
}

#[test]
fn snippet_confirmation_carries_the_hash_shown() {
    let t0 = Instant::now();
    let mut data = SyncData::default();
    data.set_snippets(
        vec![
            unconfirmed("a", "ls", "ha"),
            unconfirmed("b", "rm -rf /tmp/x", "hb"),
        ],
        t0,
    );
    let mut panel = SyncPanel::new(SyncTab::Snippets, "me".into(), t0);
    panel.select_snippet(&data, "b");
    match panel.handle_key(ch('y'), &data, later(t0)) {
        SyncOutcome::ConfirmSnippet { id, hash, .. } => {
            assert_eq!((id.as_str(), hash.as_str()), ("b", "hb"));
        }
        other => panic!("{other:?}"),
    }
    data.stale_snippets = true;
    assert!(matches!(
        panel.handle_key(ch('y'), &data, later(t0)),
        SyncOutcome::None
    ));
}

#[test]
fn reset_asks_once_more_and_disable_forgets_the_key_by_default() {
    let t0 = Instant::now();
    let data = SyncData {
        overview: Some(idle()),
        ..SyncData::default()
    };
    let mut panel = SyncPanel::new(SyncTab::Overview, "me".into(), t0);
    panel.handle_key(ch('R'), &data, t0);
    let PanelMode::Reset { form, .. } = &mut panel.mode else {
        panic!("reset form");
    };
    typed(form, "wide awake pottery yearning blanket");
    form.handle_key(key(KeyCode::Tab));
    typed(form, "wide awake pottery yearning blanket");
    assert!(matches!(
        panel.handle_key(key(KeyCode::Enter), &data, t0),
        SyncOutcome::None
    ));
    assert!(matches!(
        panel.mode,
        PanelMode::Reset {
            confirm: Some(_),
            ..
        }
    ));
    // Armed: a quick `y` does nothing, `y` later resets.
    assert!(matches!(
        panel.handle_key(ch('y'), &data, t0),
        SyncOutcome::None
    ));
    assert!(matches!(
        panel.handle_key(ch('y'), &data, later(t0)),
        SyncOutcome::Reset(NewPassphrase::Typed(_))
    ));

    let mut panel = SyncPanel::new(SyncTab::Overview, "me".into(), t0);
    panel.handle_key(ch('D'), &data, t0);
    match panel.handle_key(key(KeyCode::Enter), &data, t0) {
        SyncOutcome::Disable(options) => {
            assert!(options.forget_key);
            assert!(!options.remove_my_file);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn overview_offers_what_the_status_allows() {
    let t0 = Instant::now();
    let mut data = SyncData {
        overview: Some(SyncOverview {
            status: SyncStatus::Disabled,
            ..idle()
        }),
        ..SyncData::default()
    };
    let mut panel = SyncPanel::new(SyncTab::Overview, "me".into(), t0);
    assert!(matches!(
        panel.handle_key(key(KeyCode::Enter), &data, t0),
        SyncOutcome::OpenWizard
    ));
    // Sync off: `s` does nothing.
    assert!(matches!(
        panel.handle_key(ch('s'), &data, t0),
        SyncOutcome::None
    ));
    data.overview = Some(SyncOverview {
        status: SyncStatus::Locked,
        ..idle()
    });
    panel.handle_key(key(KeyCode::Enter), &data, t0);
    assert!(matches!(panel.mode, PanelMode::Unlock(_)));
    data.overview = Some(SyncOverview {
        status: SyncStatus::Alert {
            kind: AlertKind::PassphraseChanged,
            device: Some("desk (77a1b2c3)".into()),
        },
        ..idle()
    });
    let mut panel = SyncPanel::new(SyncTab::Overview, "me".into(), t0);
    assert!(matches!(
        panel.handle_key(ch('a'), &data, t0),
        SyncOutcome::Acknowledge
    ));
    assert!(matches!(
        panel.handle_key(ch('m'), &data, t0),
        SyncOutcome::SetRemember(false)
    ));
    // Tabs: Tab, Shift-Tab, digits.
    panel.handle_key(key(KeyCode::Tab), &data, t0);
    assert_eq!(panel.tab, SyncTab::Conflicts);
    panel.handle_key(ch('5'), &data, t0);
    assert_eq!(panel.tab, SyncTab::Snippets);
    panel.handle_key(key(KeyCode::BackTab), &data, t0);
    assert_eq!(panel.tab, SyncTab::Hosts);
    assert!(matches!(
        panel.handle_key(key(KeyCode::Esc), &data, t0),
        SyncOutcome::Close
    ));
}
