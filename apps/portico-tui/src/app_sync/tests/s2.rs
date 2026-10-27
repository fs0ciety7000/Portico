//! S2c-1 in the TUI: the rotation dialogs, forgetting a device, the
//! adoption (previous passphrase first on a device that does not remember
//! its key, RR4), the rotation conflict, a forgotten device, held values,
//! newer device files, old keychain entries, "Leave the device list" and
//! the re-join review; then the lost-device drill through the panel's keys
//! (three engines, one folder, public API only).
//!
//! Captures: `PORTICO_CAPTURE_DIR=… cargo test -p portico-tui
//! s2c1_captures -- --ignored`.

use portico_core::sync::api::{
    ForgetPreview, NewerEpochFile, RevokedValueItem, RotationOutcome, RotationPeer,
    RotationPreview, SyncKeyEntry,
};
use portico_core::sync::{NewPassphrase, WaitReason};

use super::*;
use crate::sync_panel::PassField;
use crate::sync_rotate::{RotateStep, Rotated};

fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn press(app: &mut App, keys: &str) {
    for c in keys.chars() {
        app.update(Action::SyncKey(ch(c)));
    }
}

fn with_status(app: &mut App, status: SyncStatus, remember_key: bool) {
    app.sync.overview = Some(SyncOverview {
        status,
        remember_key,
        ..idle(0)
    });
}

fn alert(kind: AlertKind, device: Option<&str>) -> SyncStatus {
    SyncStatus::Alert {
        kind,
        device: device.map(Into::into),
    }
}

fn preview(outdated: bool) -> RotationPreview {
    RotationPreview {
        devices: vec![
            RotationPeer {
                id: "3f9c0ae1".repeat(4),
                label: "laptop (3f9c0ae1)".into(),
                this_device: true,
                app_version: Some("0.7.0".into()),
                outdated: false,
                waiting: false,
            },
            RotationPeer {
                id: "77a1b2c3".repeat(4),
                label: "desk (77a1b2c3)".into(),
                this_device: false,
                app_version: Some("0.7.0".into()),
                outdated: false,
                waiting: true,
            },
            RotationPeer {
                id: "0b5e7d21".repeat(4),
                label: "old-mac (0b5e7d21)".into(),
                this_device: false,
                app_version: Some("0.6.2".into()),
                outdated,
                waiting: false,
            },
        ],
        outdated: if outdated {
            vec!["old-mac (0b5e7d21)".into()]
        } else {
            Vec::new()
        },
        format_upgrade: outdated,
        exposed_accounts: Vec::new(),
    }
}

fn forget_preview() -> ForgetPreview {
    ForgetPreview {
        id: "0b5e7d21".repeat(4),
        label: "old-mac (0b5e7d21)".into(),
        last_seen: Some(now_unix() - 86_400 * 200),
        last_written: vec![
            SyncSubject::Host {
                alias: "staging".into(),
            },
            SyncSubject::HostMeta {
                alias: "web".into(),
                field: "tags".into(),
            },
        ],
        exposed_accounts: Vec::new(),
        enter_new_passphrase_on: vec!["desk (77a1b2c3)".into()],
    }
}

fn keys() -> Vec<SyncKeyEntry> {
    vec![SyncKeyEntry {
        account: "sync-key:9f0e11aa:2".into(),
        bundle: "9f0e11aa".into(),
        epoch: 2,
        this_bundle: true,
    }]
}

fn held() -> Vec<RevokedValueItem> {
    vec![RevokedValueItem {
        key: "host:staging".into(),
        subject: SyncSubject::Host {
            alias: "staging".into(),
        },
        device: Some("old-mac (0b5e7d21)".into()),
    }]
}

fn newer() -> Vec<NewerEpochFile> {
    vec![NewerEpochFile {
        device: "77a1b2c3".repeat(4),
        label: "desk (77a1b2c3)".into(),
        file: "devices/77a1b2c377a1b2c377a1b2c377a1b2c3.snap".into(),
        epoch: 4,
        fingerprint: "fp-1".into(),
    }]
}

fn armed() -> Instant {
    Instant::now()
        .checked_sub(ARM_DELAY * 2)
        .unwrap_or_else(Instant::now)
}

/// The panel on `tab` with the devices, old keys and newer files.
fn open_s2(app: &mut App, tab: SyncTab) {
    open_panel(app, tab);
    app.update(Action::SyncKeyEntries(Ok(keys())));
    app.update(Action::SyncNewerFiles(Ok(Vec::new())));
}

fn rotate_flow(app: &App) -> &crate::sync_rotate::RotateFlow {
    match &app.sync_panel.as_ref().unwrap().mode {
        PanelMode::Rotate(flow) => flow,
        other => panic!("{other:?}"),
    }
}

/// Overview → `o`, the preview arrives.
fn rotate_about(app: &mut App) {
    open_s2(app, SyncTab::Overview);
    press(app, "o");
    app.update(Action::SyncRotationPreview(Ok(preview(true))));
}

fn rotate_new(app: &mut App) {
    rotate_about(app);
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    press(app, "old words");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
}

fn forget_about(app: &mut App) {
    open_s2(app, SyncTab::Devices);
    press(app, "jj");
    press(app, "f");
    app.update(Action::SyncRotationPreview(Ok(preview(false))));
    app.update(Action::SyncForgetPreview(Ok(forget_preview())));
}

fn rotate_done(app: &mut App, pending: bool) {
    forget_about(app);
    let PanelMode::Rotate(flow) = &mut app.sync_panel.as_mut().unwrap().mode else {
        unreachable!()
    };
    flow.step = RotateStep::Working;
    let status = if pending {
        SyncStatus::Waiting {
            reason: WaitReason::RotationPending,
            device: None,
        }
    } else {
        idle(0).status
    };
    app.update(Action::SyncRotated(Ok(Rotated {
        status: status.clone(),
        outcome: Some(RotationOutcome {
            epoch: 3,
            status,
            enter_new_passphrase_on: vec!["desk (77a1b2c3)".into()],
            forgotten: vec!["old-mac (0b5e7d21)".into()],
            exposed_accounts: Vec::new(),
        }),
    })));
}

#[test]
fn status_words_for_the_new_states() {
    let now = time::OffsetDateTime::now_utc();
    let cases = [
        (
            alert(AlertKind::KeysRotatedElsewhere, None),
            "Keys changed — enter new passphrase",
        ),
        (alert(AlertKind::RotationConflict, None), "Key conflict"),
        (
            alert(AlertKind::DeviceForgotten, Some("desk (77a1b2c3)")),
            "Removed from sync",
        ),
        (
            alert(AlertKind::RevokedWrites, Some("desk (77a1b2c3)")),
            "Changes from a removed device",
        ),
        (
            SyncStatus::Waiting {
                reason: WaitReason::RotationPending,
                device: None,
            },
            "Finishing key rotation…",
        ),
        (
            SyncStatus::Waiting {
                reason: WaitReason::NeedsNewKeys,
                device: Some("desk (77a1b2c3)".into()),
            },
            "New keys needed on desk (77a1b2c3)",
        ),
    ];
    for (status, words) in cases {
        let (label, _) = crate::sync_panel::status_label(&status, now);
        assert_eq!(label, words);
    }
}

#[test]
fn rotate_keys_from_the_overview_end_to_end() {
    let mut app = synced_app();
    rotate_about(&mut app);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Rotate keys"), "{all}");
    assert!(all.contains("desk (77a1b2c3)"), "{all}");
    assert!(all.contains("Portico older than 0.7"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert_eq!(rotate_flow(&app).step, RotateStep::Authorize);
    press(&mut app, "old words");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert_eq!(rotate_flow(&app).step, RotateStep::New);
    app.update(Action::SyncGenerated(
        portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
    ));
    assert!(rotate_flow(&app).new.generated.is_some());
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Generated: 6 random words"), "{all}");
    // Remember, then "I saved it".
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    press(&mut app, " ");
    let all = text(&mut app, 80, 24);
    assert!(all.contains("I saved it"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert!(matches!(rotate_flow(&app).step, RotateStep::Confirm(_)));
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Rotate the keys now?"), "{all}");
    if let PanelMode::Rotate(flow) = &mut app.sync_panel.as_mut().unwrap().mode {
        flow.step = RotateStep::Confirm(armed());
    }
    press(&mut app, "y");
    assert_eq!(rotate_flow(&app).step, RotateStep::Working);
    // Outside a runtime nothing was spawned: the answer comes back here.
    app.update(Action::SyncRotated(Err(portico_core::ErrorSummary {
        code: "sync_passphrase_reused".into(),
        message: "this sync passphrase opens older sync keys".into(),
        hint: Some("choose another one".into()),
    })));
    let flow = rotate_flow(&app);
    assert_eq!(flow.step, RotateStep::New);
    assert!(flow.new.generated.is_none(), "the passphrase was used once");
    let all = text(&mut app, 80, 24);
    assert!(
        all.contains("This sync passphrase opens older sync keys"),
        "{all}"
    );
}

#[test]
fn forget_starts_with_the_cloud_folder_and_names_what_it_wrote() {
    let mut app = synced_app();
    forget_about(&mut app);
    let flow = rotate_flow(&app);
    assert!(matches!(
        &flow.kind,
        crate::sync_rotate::RotateKind::Forget { label, .. } if label == "old-mac (0b5e7d21)"
    ));
    let all = text(&mut app, 120, 36);
    assert!(
        all.contains("Also remove old-mac (0b5e7d21)'s access"),
        "{all}"
    );
    assert!(all.contains("Host staging"), "{all}");
    assert!(all.contains("Tags of web"), "{all}");
    rotate_done(&mut app, false);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Keys rotated."), "{all}");
    assert!(
        all.contains("Removed from sync: old-mac (0b5e7d21)."),
        "{all}"
    );
    assert!(all.contains("Enter the new passphrase on:"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert!(matches!(
        app.sync_panel.as_ref().unwrap().mode,
        PanelMode::Browse
    ));
}

#[test]
fn a_pending_rotation_says_it_finishes_later() {
    let mut app = synced_app();
    rotate_done(&mut app, true);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Finishing the key rotation…"), "{all}");
}

#[test]
fn this_device_is_never_forgotten_from_the_list() {
    let mut app = synced_app();
    open_s2(&mut app, SyncTab::Devices);
    press(&mut app, "f");
    let panel = app.sync_panel.as_ref().unwrap();
    assert!(matches!(panel.mode, PanelMode::Browse));
    let notice = panel.notice.as_ref().unwrap();
    assert!(
        notice
            .hint
            .as_deref()
            .unwrap()
            .contains("Leave the device list")
    );
}

#[test]
fn old_keys_are_listed_and_removed_after_y() {
    let mut app = synced_app();
    open_s2(&mut app, SyncTab::Devices);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Old keys in this keychain"), "{all}");
    assert!(all.contains("Keys 2 · this sync"), "{all}");
    press(&mut app, "jjj");
    press(&mut app, "x");
    assert!(matches!(
        app.sync_panel.as_ref().unwrap().mode,
        PanelMode::Ask { .. }
    ));
    // Not armed yet: `y` is ignored.
    press(&mut app, "y");
    assert!(app.sync_panel.as_ref().unwrap().busy.is_none());
    if let PanelMode::Ask { opened_at, .. } = &mut app.sync_panel.as_mut().unwrap().mode {
        *opened_at = armed();
    }
    let all = text(&mut app, 80, 24);
    assert!(
        all.contains("Remove Keys 2 · this sync from this keychain?"),
        "{all}"
    );
    press(&mut app, "y");
    assert_eq!(
        app.sync_panel.as_ref().unwrap().busy,
        Some("Saving…"),
        "sent"
    );
}

#[test]
fn adoption_asks_the_previous_passphrase_first_when_not_remembered() {
    let mut app = synced_app();
    with_status(
        &mut app,
        alert(AlertKind::KeysRotatedElsewhere, None),
        false,
    );
    open_s2(&mut app, SyncTab::Overview);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("type nothing."), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    let panel = app.sync_panel.as_mut().unwrap();
    let form = panel.form_mut().unwrap();
    assert_eq!(form.focused(), Some(PassField::Previous));
    // The previous one is optional, the new one is not.
    press(&mut app, "previous words");
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    press(&mut app, "new words");
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Previous"), "{all}");
    assert!(all.contains("Enter the new passphrase"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    let form = app.sync_panel.as_mut().unwrap().form_mut().unwrap();
    assert!(form.busy);
    assert!(form.old.is_empty() && form.previous.is_empty(), "wiped");
    // The previous passphrase was refused: the problem goes under it.
    app.update(Action::SyncAdopted {
        previous_failed: true,
        result: Err(portico_core::ErrorSummary {
            code: "sync_wrong_passphrase".into(),
            message: "wrong sync passphrase".into(),
            hint: None,
        }),
    });
    let form = app.sync_panel.as_ref().unwrap().form().unwrap();
    assert_eq!(form.focused(), Some(PassField::Previous));
    let all = text(&mut app, 80, 24);
    assert!(all.contains("restarted since the keys changed"), "{all}");
    // A forgotten device: the form closes, the status says what to do.
    app.update(Action::SyncAdopted {
        previous_failed: false,
        result: Err(portico_core::ErrorSummary {
            code: "sync_device_forgotten".into(),
            message: "this device was removed from sync".into(),
            hint: Some("Turn sync off, or join again as a new device.".into()),
        }),
    });
    assert!(matches!(
        app.sync_panel.as_ref().unwrap().mode,
        PanelMode::Browse
    ));
}

#[test]
fn adoption_without_previous_when_the_key_is_remembered() {
    let mut app = synced_app();
    with_status(&mut app, alert(AlertKind::KeysRotatedElsewhere, None), true);
    open_s2(&mut app, SyncTab::Overview);
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    let form = app.sync_panel.as_ref().unwrap().form().unwrap();
    assert_eq!(form.fields(), &[PassField::Old]);
}

#[test]
fn a_newer_file_can_be_removed_after_y() {
    let mut app = synced_app();
    with_status(
        &mut app,
        alert(AlertKind::KeysRotatedElsewhere, Some("desk (77a1b2c3)")),
        true,
    );
    open_s2(&mut app, SyncTab::Overview);
    app.update(Action::SyncNewerFiles(Ok(newer())));
    let all = text(&mut app, 120, 36);
    assert!(all.contains("This file claims newer keys"), "{all}");
    press(&mut app, "x");
    if let PanelMode::Ask { opened_at, .. } = &mut app.sync_panel.as_mut().unwrap().mode {
        *opened_at = armed();
    } else {
        panic!("asked");
    }
    press(&mut app, "y");
    assert!(app.sync_panel.as_ref().unwrap().busy.is_some());
}

#[test]
fn rotation_conflict_offers_two_explicit_choices() {
    let mut app = synced_app();
    with_status(&mut app, alert(AlertKind::RotationConflict, None), true);
    open_s2(&mut app, SyncTab::Overview);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("use the other device's keys"), "{all}");
    assert!(all.contains("keep this device's keys"), "{all}");
    // Keep mine: R17, then the new passphrase (no authorization).
    press(&mut app, "o");
    app.update(Action::SyncRotationPreview(Ok(preview(false))));
    let all = text(&mut app, 80, 24);
    assert!(all.contains("removals are lost"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    assert_eq!(rotate_flow(&app).step, RotateStep::New);
    app.update(Action::SyncKey(key(KeyCode::Esc)));
    app.update(Action::SyncKey(key(KeyCode::Esc)));
    // Use theirs: the refusal E2 names the device.
    press(&mut app, "u");
    press(&mut app, "their words");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    app.update(Action::SyncDone {
        op: SyncOp::UseOther,
        result: Err(portico_core::ErrorSummary {
            code: "sync_rotation_conflict".into(),
            message: "the other device's sync keys still include forgotten device old-mac (0b5e7d21)".into(),
            hint: Some("The other device's keys still include a device you removed: keep this device's keys instead.".into()),
        }),
    });
    let all = text(&mut app, 120, 36);
    assert!(all.contains("old-mac (0b5e7d21)"), "{all}");
    // The conflict is over meanwhile: back to the Overview.
    app.update(Action::SyncDone {
        op: SyncOp::UseOther,
        result: Err(portico_core::ErrorSummary {
            code: "sync_not_pending".into(),
            message: "x".into(),
            hint: None,
        }),
    });
    assert!(matches!(
        app.sync_panel.as_ref().unwrap().mode,
        PanelMode::Browse
    ));
}

#[test]
fn a_forgotten_device_turns_off_then_joins_again() {
    let mut app = synced_app();
    with_status(
        &mut app,
        alert(AlertKind::DeviceForgotten, Some("desk (77a1b2c3)")),
        true,
    );
    open_s2(&mut app, SyncTab::Overview);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("removed from sync by desk"), "{all}");
    press(&mut app, "n");
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Turn sync off, then join again"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    app.update(Action::SyncDone {
        op: SyncOp::DisableJoin(Some("/home/alice/Dropbox/Portico".into())),
        result: Ok(Some(SyncStatus::Disabled)),
    });
    let wizard = app.sync_wizard.as_ref().unwrap();
    assert!(matches!(wizard.step, Step::Folder));
    assert_eq!(wizard.folder, "/home/alice/Dropbox/Portico");
}

#[test]
fn turn_sync_off_offers_leaving_the_device_list() {
    let mut app = synced_app();
    open_s2(&mut app, SyncTab::Overview);
    press(&mut app, "D");
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    press(&mut app, " ");
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Leave the device list"), "{all}");
    assert!(all.contains("can still read the current sync"), "{all}");
    let PanelMode::Disable { options, .. } = &app.sync_panel.as_ref().unwrap().mode else {
        panic!("disable");
    };
    assert!(options.leave_roster && options.forget_key);
    // As in the GUI, removing this device's file ticks it together.
    app.update(Action::SyncKey(key(KeyCode::BackTab)));
    press(&mut app, " ");
    let PanelMode::Disable { options, .. } = &app.sync_panel.as_ref().unwrap().mode else {
        panic!("disable");
    };
    assert!(options.remove_my_file && options.leave_roster);
    press(&mut app, " ");
    let PanelMode::Disable { options, .. } = &app.sync_panel.as_ref().unwrap().mode else {
        panic!("disable");
    };
    assert!(!options.remove_my_file && !options.leave_roster);
}

#[test]
fn held_values_are_decided_one_by_one_nothing_preselected() {
    let mut app = synced_app();
    app.sync.set_held(held(), armed());
    app.sync.changed_at = Some(armed());
    open_s2(&mut app, SyncTab::Received);
    // Hosts and snippets first; the held value last.
    let last = app.sync.to_review() - 1;
    app.sync_panel.as_mut().unwrap().received = last;
    app.sync_panel.as_mut().unwrap().selected_at = armed();
    let all = text(&mut app, 120, 36);
    assert!(all.contains("From forgotten devices"), "{all}");
    assert!(all.contains("held · from old-mac (0b5e7d21)"), "{all}");
    // `y` is not a choice here.
    press(&mut app, "y");
    assert!(app.sync_panel.as_ref().unwrap().busy.is_none());
    press(&mut app, "t");
    assert!(app.sync_panel.as_ref().unwrap().busy.is_some());
}

#[test]
fn a_rejoin_offers_only_review_each() {
    let mut app = synced_app();
    wizard_at_folder(&mut app);
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
            needs_newer: 0,
            rejoin: true,
        },
        Instant::now(),
    );
    app.update(Action::SyncWizardKey(key(KeyCode::Down)));
    assert_eq!(app.sync_wizard.as_ref().unwrap().bulk, 0);
    let all = text(&mut app, 80, 24);
    assert!(all.contains("not offered"), "{all}");
    assert!(all.contains("synced with this folder before"), "{all}");
}

#[test]
fn every_new_screen_fits_80x24_and_120x36() {
    for (_, setup) in screens() {
        for (cols, rows) in [(80u16, 24u16), (120, 36)] {
            let mut app = synced_app();
            setup(&mut app);
            let lines = screen(&mut app, cols, rows);
            assert_eq!(lines.len(), usize::from(rows));
        }
    }
}

#[allow(clippy::too_many_lines)] // the list of screens
fn screens() -> Vec<(&'static str, Setup)> {
    vec![
        ("status-keys-changed", |a| {
            with_status(a, alert(AlertKind::KeysRotatedElsewhere, None), true);
        }),
        ("rotate-about", rotate_about),
        ("rotate-authorize", |a| {
            rotate_about(a);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
        }),
        ("rotate-dont-know", |a| {
            rotate_about(a);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            press(a, " ");
        }),
        ("rotate-new", rotate_new),
        ("rotate-generated", |a| {
            rotate_new(a);
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
        }),
        ("rotate-saved", |a| {
            rotate_new(a);
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
        }),
        ("rotate-confirm", |a| {
            rotate_about(a);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            press(a, " ");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            press(a, " ");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            if let PanelMode::Rotate(flow) = &mut a.sync_panel.as_mut().unwrap().mode {
                flow.step = RotateStep::Confirm(armed());
            }
        }),
        ("forget-confirm", |a| {
            forget_about(a);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            press(a, "old words");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncGenerated(
                portico_core::sync::generate_passphrase().map_err(|e| e.summary()),
            ));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            a.update(Action::SyncKey(key(KeyCode::Tab)));
            press(a, " ");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            if let PanelMode::Rotate(flow) = &mut a.sync_panel.as_mut().unwrap().mode {
                flow.step = RotateStep::Confirm(armed());
            }
        }),
        ("rotate-refused", |a| {
            rotate_new(a);
            if let PanelMode::Rotate(flow) = &mut a.sync_panel.as_mut().unwrap().mode {
                flow.step = RotateStep::Working;
            }
            a.update(Action::SyncRotated(Err(portico_core::ErrorSummary {
                code: "sync_passphrase_reused".into(),
                message: "this sync passphrase opens older sync keys".into(),
                hint: Some("This passphrase opens older keys: choose another one (rotation), or type nothing if you did not change the keys (adoption).".into()),
            })));
        }),
        ("rotate-working", |a| {
            rotate_new(a);
            if let PanelMode::Rotate(flow) = &mut a.sync_panel.as_mut().unwrap().mode {
                flow.step = RotateStep::Working;
            }
        }),
        ("rotate-done", |a| rotate_done(a, false)),
        ("rotate-pending", |a| rotate_done(a, true)),
        ("forget-about", forget_about),
        ("devices-keys", |a| {
            open_s2(a, SyncTab::Devices);
            press(a, "j");
        }),
        ("remove-key", |a| {
            open_s2(a, SyncTab::Devices);
            press(a, "jjj");
            press(a, "x");
            if let PanelMode::Ask { opened_at, .. } = &mut a.sync_panel.as_mut().unwrap().mode {
                *opened_at = armed();
            }
        }),
        ("adopt", |a| {
            with_status(a, alert(AlertKind::KeysRotatedElsewhere, None), true);
            open_s2(a, SyncTab::Overview);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
        }),
        ("adopt-previous", |a| {
            with_status(a, alert(AlertKind::KeysRotatedElsewhere, None), false);
            open_s2(a, SyncTab::Overview);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            press(a, "words");
        }),
        ("adopt-reused", |a| {
            with_status(a, alert(AlertKind::KeysRotatedElsewhere, None), true);
            open_s2(a, SyncTab::Overview);
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncAdopted {
                previous_failed: false,
                result: Err(portico_core::ErrorSummary {
                    code: "sync_passphrase_reused".into(),
                    message: "this sync passphrase opens older sync keys".into(),
                    hint: Some("This passphrase opens older keys: choose another one (rotation), or type nothing if you did not change the keys (adoption).".into()),
                }),
            });
        }),
        ("keys-changed", |a| {
            with_status(a, alert(AlertKind::KeysRotatedElsewhere, None), true);
            open_s2(a, SyncTab::Overview);
        }),
        ("newer-file", |a| {
            with_status(
                a,
                alert(AlertKind::KeysRotatedElsewhere, Some("desk (77a1b2c3)")),
                true,
            );
            open_s2(a, SyncTab::Overview);
            a.update(Action::SyncNewerFiles(Ok(newer())));
        }),
        ("newer-file-remove", |a| {
            with_status(
                a,
                alert(AlertKind::KeysRotatedElsewhere, Some("desk (77a1b2c3)")),
                true,
            );
            open_s2(a, SyncTab::Overview);
            a.update(Action::SyncNewerFiles(Ok(newer())));
            press(a, "x");
            if let PanelMode::Ask { opened_at, .. } = &mut a.sync_panel.as_mut().unwrap().mode {
                *opened_at = armed();
            }
        }),
        ("conflict", |a| {
            with_status(a, alert(AlertKind::RotationConflict, None), true);
            open_s2(a, SyncTab::Overview);
        }),
        ("conflict-use-other", |a| {
            with_status(a, alert(AlertKind::RotationConflict, None), true);
            open_s2(a, SyncTab::Overview);
            press(a, "u");
            press(a, "x");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncDone {
                op: SyncOp::UseOther,
                result: Err(portico_core::ErrorSummary {
                    code: "sync_rotation_conflict".into(),
                    message: "the other device's sync keys still include forgotten device old-mac (0b5e7d21)".into(),
                    hint: Some("The other device's keys still include a device you removed: keep this device's keys instead.".into()),
                }),
            });
        }),
        ("conflict-keep-mine", |a| {
            with_status(a, alert(AlertKind::RotationConflict, None), true);
            open_s2(a, SyncTab::Overview);
            press(a, "o");
            a.update(Action::SyncRotationPreview(Ok(preview(false))));
        }),
        ("forgotten", |a| {
            with_status(
                a,
                alert(AlertKind::DeviceForgotten, Some("desk (77a1b2c3)")),
                true,
            );
            open_s2(a, SyncTab::Overview);
        }),
        ("forgotten-join-again", |a| {
            with_status(
                a,
                alert(AlertKind::DeviceForgotten, Some("desk (77a1b2c3)")),
                true,
            );
            open_s2(a, SyncTab::Overview);
            press(a, "n");
        }),
        ("rejoin-wizard-folder", |a| {
            open_s2(a, SyncTab::Overview);
            with_status(
                a,
                alert(AlertKind::DeviceForgotten, Some("desk (77a1b2c3)")),
                true,
            );
            press(a, "n");
            a.update(Action::SyncKey(key(KeyCode::Enter)));
            a.update(Action::SyncDone {
                op: SyncOp::DisableJoin(Some("/home/alice/Dropbox/Portico".into())),
                result: Ok(Some(SyncStatus::Disabled)),
            });
        }),
        ("revoked-writes", |a| {
            with_status(
                a,
                alert(AlertKind::RevokedWrites, Some("desk (77a1b2c3)")),
                true,
            );
            a.sync.set_held(held(), armed());
            open_s2(a, SyncTab::Overview);
        }),
        ("held-value", |a| {
            a.sync.set_held(held(), armed());
            a.sync.changed_at = Some(armed());
            open_s2(a, SyncTab::Received);
            let last = a.sync.to_review() - 1;
            let panel = a.sync_panel.as_mut().unwrap();
            panel.received = last;
            panel.selected_at = armed();
        }),
        ("needs-new-keys", |a| {
            with_status(
                a,
                SyncStatus::Waiting {
                    reason: WaitReason::NeedsNewKeys,
                    device: Some("desk (77a1b2c3)".into()),
                },
                true,
            );
            open_s2(a, SyncTab::Overview);
        }),
        ("rotation-pending", |a| {
            with_status(
                a,
                SyncStatus::Waiting {
                    reason: WaitReason::RotationPending,
                    device: None,
                },
                true,
            );
            open_s2(a, SyncTab::Overview);
        }),
        ("restore", |a| {
            with_status(a, alert(AlertKind::BundleTampered, None), true);
            open_s2(a, SyncTab::Overview);
            press(a, "b");
            if let PanelMode::Ask { opened_at, .. } = &mut a.sync_panel.as_mut().unwrap().mode {
                *opened_at = armed();
            }
        }),
        ("turn-off-leave", |a| {
            open_s2(a, SyncTab::Overview);
            press(a, "D");
        }),
        ("rejoin-review", |a| {
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
                    needs_newer: 0,
                    rejoin: true,
                },
                Instant::now(),
            );
        }),
    ]
}

#[test]
#[ignore = "writes captures"]
fn s2c1_captures() {
    let Some(dir) = std::env::var_os("PORTICO_CAPTURE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (theme, id) in [("obsidian", "obsidian"), ("marble", "marble")] {
        for (cols, rows) in [(80u16, 24u16), (120, 36)] {
            for (name, setup) in screens() {
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

// ── the lost-device drill through the panel's keys ─────────────────────────

/// Key derivations (Argon2) under a loaded test run: a longer limit.
const DRILL_SECS: u64 = 120;

async fn slow(
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    done: impl FnMut(&App) -> bool,
) {
    app.pump_within(events, done, DRILL_SECS).await;
}

/// Waits for the rotation dialog to reach `step` (or a refusal).
async fn until_step(
    app: &mut App,
    events: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    step: fn(&RotateStep) -> bool,
) {
    slow(app, events, |a| {
        match &a.sync_panel.as_ref().unwrap().mode {
            PanelMode::Rotate(flow) => step(&flow.step) || flow.problem.is_some(),
            _ => true,
        }
    })
    .await;
}

/// B creates a sync; this app (A) and a device L join it. From A, through
/// the panel's keys: forget L (current passphrase, a new one, "I saved
/// it"). B then sees new keys, types them and syncs again; L's old
/// passphrase opens nothing new. Then B rotates and A adopts through the
/// "Enter the new passphrase" form.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, three devices, end to end
async fn lost_device_drill_through_the_panel() {
    use portico_core::sync::{JoinBulkChoice, SyncCreate, SyncJoin, generate_passphrase};
    use secrecy::{ExposeSecret, SecretString};

    let words =
        |p: &portico_core::sync::GeneratedPassphrase| p.as_secret().expose_secret().to_owned();
    let b = crate::ssh_support::Env::new("");
    let me = crate::ssh_support::Env::new("");
    let lost = crate::ssh_support::Env::new("");
    let shared = tempfile::tempdir().unwrap();
    let first = generate_passphrase().unwrap();
    let p = words(&first);
    b.engine
        .sync_create(SyncCreate {
            folder: shared.path().to_path_buf(),
            passphrase: NewPassphrase::Generated(first),
            device_name: "desk".into(),
            remember_key: true,
        })
        .await
        .unwrap();
    for (env, name) in [(&me, "laptop"), (&lost, "lost")] {
        let preview = env
            .engine
            .sync_join_preview(SyncJoin {
                folder: shared.path().to_path_buf(),
                passphrase: SecretString::from(p.clone()),
                device_name: name.into(),
                remember_key: true,
            })
            .await
            .unwrap();
        env.engine
            .sync_join_commit(preview.token, JoinBulkChoice::ReviewEach)
            .await
            .unwrap();
    }
    for env in [&b, &lost, &me, &b] {
        env.engine.sync_now().await.unwrap();
    }

    let mut events = me.engine.subscribe();
    let mut app = App::new(me.engine.clone());
    app.clipboard = false;
    app.start_sync();
    app.update(Action::OpenSync);
    app.update(Action::SyncKey(ch('5')));
    slow(&mut app, &mut events, |a| {
        a.sync_panel
            .as_ref()
            .and_then(|p| p.devices.ready())
            .is_some_and(|d| d.len() == 3)
    })
    .await;
    let index = app
        .sync_panel
        .as_ref()
        .unwrap()
        .devices
        .ready()
        .unwrap()
        .iter()
        .position(|d| d.name == "lost")
        .unwrap();
    for _ in 0..index {
        app.update(Action::SyncKey(ch('j')));
    }
    // A sync run started by this app (or a reload) may be under way: a
    // device is forgotten only while sync runs here, so wait for that.
    loop {
        slow(&mut app, &mut events, |a| {
            a.sync.status().is_some_and(|status| {
                crate::sync_panel::overview_actions(status, false)
                    .contains(&crate::sync_panel::OverviewAction::Rotate)
            }) && a.sync_panel.as_ref().is_some_and(|p| p.busy.is_none())
        })
        .await;
        app.update(Action::SyncKey(ch('f')));
        if matches!(app.sync_panel.as_ref().unwrap().mode, PanelMode::Rotate(_)) {
            break;
        }
        app.sync_panel.as_mut().unwrap().notice = None;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    slow(&mut app, &mut events, |a| {
        match &a.sync_panel.as_ref().unwrap().mode {
            PanelMode::Rotate(flow) => flow.previews_ready(),
            _ => false,
        }
    })
    .await;
    let all = text(&mut app, 120, 36);
    assert!(all.contains("Also remove lost ("), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    app.update(Action::Paste(p.clone()));
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    let next = generate_passphrase().unwrap();
    let p2 = words(&next);
    app.update(Action::Paste(p2.clone()));
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    app.update(Action::Paste(p2.clone()));
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    app.update(Action::SyncKey(key(KeyCode::Tab)));
    app.update(Action::SyncKey(ch(' ')));
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    // The last question is armed: only a later `y` forgets.
    tokio::time::sleep(ARM_DELAY + Duration::from_millis(50)).await;
    app.update(Action::SyncKey(ch('y')));
    until_step(&mut app, &mut events, |s| *s == RotateStep::Done).await;
    let flow = rotate_flow(&app);
    assert!(flow.problem.is_none(), "{:?}", flow.problem);
    let outcome = flow.result.as_ref().unwrap().outcome.clone().unwrap();
    assert_eq!(outcome.forgotten.len(), 1);
    assert!(outcome.forgotten[0].starts_with("lost ("));
    assert!(
        outcome
            .enter_new_passphrase_on
            .iter()
            .any(|d| d.starts_with("desk (")),
        "{outcome:?}"
    );
    let all = text(&mut app, 80, 24);
    assert!(all.contains("Keys rotated."), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));

    // B: new keys, it types them; L's old passphrase opens nothing new.
    let _ = b.engine.sync_now().await;
    assert!(matches!(
        b.engine.sync_status().await.unwrap().status,
        SyncStatus::Alert {
            kind: AlertKind::KeysRotatedElsewhere,
            ..
        }
    ));
    b.engine
        .sync_unlock(SecretString::from(p2.clone()))
        .await
        .unwrap();
    let _ = lost.engine.sync_now().await;
    assert!(
        lost.engine
            .sync_unlock(SecretString::from(p.clone()))
            .await
            .is_err()
    );

    // B rotates; A adopts through the panel's form.
    let third = generate_passphrase().unwrap();
    let p3 = words(&third);
    b.engine.sync_now().await.unwrap();
    b.engine
        .sync_rotate_keys(portico_core::sync::api::RotateKeys {
            authorization: portico_core::sync::api::RotateAuth::CurrentPassphrase(
                SecretString::from(p2.clone()),
            ),
            new_passphrase: NewPassphrase::Generated(third),
            remember_key: true,
        })
        .await
        .unwrap();
    app.update(Action::SyncKey(ch('1')));
    app.update(Action::SyncKey(ch('s')));
    slow(&mut app, &mut events, |a| {
        matches!(
            a.sync.status(),
            Some(SyncStatus::Alert {
                kind: AlertKind::KeysRotatedElsewhere,
                ..
            })
        ) && a.sync_panel.as_ref().is_some_and(|p| p.busy.is_none())
    })
    .await;
    let all = text(&mut app, 80, 24);
    assert!(all.contains("enter the new passphrase"), "{all}");
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    // An old passphrase is refused (it opens older keys).
    app.update(Action::Paste(p2.clone()));
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    slow(&mut app, &mut events, |a| {
        a.sync_panel
            .as_ref()
            .and_then(|p| p.form())
            .is_some_and(|f| !f.busy)
    })
    .await;
    let problem = app
        .sync_panel
        .as_ref()
        .unwrap()
        .form()
        .unwrap()
        .problem
        .clone();
    assert!(problem.is_some(), "refused");
    app.update(Action::Paste(p3));
    app.update(Action::SyncKey(key(KeyCode::Enter)));
    slow(&mut app, &mut events, |a| {
        a.sync_panel
            .as_ref()
            .is_some_and(|p| matches!(p.mode, PanelMode::Browse) && p.notice.is_some())
    })
    .await;
    let notice = app.sync_panel.as_ref().unwrap().notice.clone().unwrap();
    assert!(notice.ok, "{notice:?}");
    let status = me.engine.sync_now().await.unwrap();
    assert!(
        !matches!(status, SyncStatus::Alert { .. } | SyncStatus::Locked),
        "{status:?}"
    );
}
