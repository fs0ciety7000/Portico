//! The host form alone: navigation, pickers, the draft it builds, refusals
//! routed by code, the discard question and the password's hygiene.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use secrecy::ExposeSecret;

use super::*;

fn k(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(form: &mut HostForm, text: &str) {
    for c in text.chars() {
        form.handle_key(k(KeyCode::Char(c)));
    }
}

fn summary(code: &str) -> ErrorSummary {
    ErrorSummary {
        code: code.into(),
        message: format!("{code} message"),
        hint: Some(format!("{code} hint")),
    }
}

fn key(name: &str) -> LocalKey {
    LocalKey {
        private_path: PathBuf::from(format!("/home/alice/.ssh/{name}")),
        public_path: PathBuf::from(format!("/home/alice/.ssh/{name}.pub")),
        algorithm: "ED25519".into(),
        fingerprint: "SHA256:x".into(),
        comment: String::new(),
        has_private: true,
        certificate: None,
    }
}

#[test]
fn tab_walks_every_field_and_wraps() {
    let mut form = HostForm::create(&["web".into(), "bastion".into()]);
    let order = [
        HostField::Alias,
        HostField::HostName,
        HostField::User,
        HostField::Port,
        HostField::Key,
        HostField::Jump,
        // The password right after ProxyJump (shared with the GUI).
        HostField::Password,
        HostField::Tags,
        HostField::Group,
        HostField::Favorite,
        HostField::Notes,
    ];
    for expected in order.iter().skip(1).chain(order.first()) {
        form.handle_key(k(KeyCode::Tab));
        assert_eq!(form.focus, *expected);
    }
    form.handle_key(k(KeyCode::BackTab));
    assert_eq!(form.focus, HostField::Notes);
    // Edit with a password remembered has the choice; its input only when
    // changing, both right after ProxyJump.
    let mut edit = HostForm::create(&[]);
    edit.mode = FormMode::Edit {
        original: "web".into(),
    };
    edit.password_remembered = Some(true);
    let after_jump = |form: &HostForm| -> Vec<HostField> {
        let fields = form.fields();
        let at = fields.iter().position(|f| *f == HostField::Jump).unwrap();
        fields[at + 1..].to_vec()
    };
    assert_eq!(after_jump(&edit).first(), Some(&HostField::PasswordMode));
    edit.focus = HostField::PasswordMode;
    edit.handle_key(k(KeyCode::Right));
    assert_eq!(edit.password_mode, PasswordMode::Change);
    assert_eq!(
        after_jump(&edit)[..3],
        [
            HostField::PasswordMode,
            HostField::Password,
            HostField::Tags
        ]
    );
    // Duplicate asks for the alias only.
    assert_eq!(HostForm::duplicate("Web").fields(), vec![HostField::Alias]);
}

#[test]
fn values_build_the_draft() {
    let home = Path::new("/home/alice");
    let mut form = HostForm::create(&["web".into(), "bastion".into()]);
    form.set_keys(&[key("id_ed25519"), key("id_rsa")], home);
    typed(&mut form, "db");
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, " 10.0.0.5 ");
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, "admin");
    form.handle_key(k(KeyCode::Tab));
    // The port takes digits only, five at most.
    typed(&mut form, "2x2:22999");
    assert_eq!(form.port, "22229");
    form.handle_key(k(KeyCode::Backspace));
    form.handle_key(k(KeyCode::Backspace));
    form.handle_key(k(KeyCode::Backspace));
    assert_eq!(form.port, "22");
    form.handle_key(k(KeyCode::Tab));
    // Key picker: none, then the keys; ← wraps; type-ahead.
    assert_eq!(form.keys.len(), 3);
    form.handle_key(k(KeyCode::Left));
    assert_eq!(
        form.keys.get(form.key).map(|c| c.label.as_str()),
        Some("~/.ssh/id_rsa")
    );
    form.handle_key(k(KeyCode::Char('i')));
    assert_eq!(
        form.keys.get(form.key).map(|c| c.label.as_str()),
        Some("~/.ssh/id_ed25519")
    );
    form.handle_key(k(KeyCode::Tab));
    form.handle_key(k(KeyCode::Char('b')));
    assert_eq!(form.jumps.get(form.jump), Some(&Some("bastion".into())));
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, "s3cr3t");
    assert!(form.password_typed());
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, "#prod, eu");
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, "EU");
    form.handle_key(k(KeyCode::Tab));
    form.handle_key(k(KeyCode::Char(' ')));
    assert!(form.favorite);
    form.handle_key(k(KeyCode::Tab));
    typed(&mut form, "first");
    form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    typed(&mut form, "second");

    let FormOutcome::Save(HostRequest::Create(draft)) = form.handle_key(k(KeyCode::Enter)) else {
        panic!("a create")
    };
    assert!(form.busy);
    assert_eq!(draft.alias, "db");
    assert_eq!(draft.hostname, "10.0.0.5");
    assert_eq!(draft.user.as_deref(), Some("admin"));
    assert_eq!(draft.port, Some(22));
    assert_eq!(
        draft.identity_file.as_deref(),
        Some(Path::new("/home/alice/.ssh/id_ed25519"))
    );
    assert_eq!(draft.proxy_jump.as_deref(), Some("bastion"));
    assert_eq!(draft.tags, ["#prod", "eu"]);
    assert_eq!(draft.group.as_deref(), Some("EU"));
    assert!(draft.favorite);
    assert_eq!(draft.notes.as_deref(), Some("first\nsecond"));
    let PasswordChange::Set(password) = &draft.password else {
        panic!("a password to remember")
    };
    assert_eq!(password.expose_secret(), "s3cr3t");
    // Keys are ignored while the engine answers.
    form.handle_key(k(KeyCode::Char('x')));
    assert_eq!(form.alias, "db");
    // No secret in `Debug`.
    assert!(!format!("{form:?}").contains("s3cr3t"));
}

#[test]
fn empty_optional_fields_stay_unset() {
    let mut form = HostForm::create(&[]);
    typed(&mut form, "lab");
    let FormOutcome::Save(HostRequest::Create(draft)) = form.handle_key(k(KeyCode::Enter)) else {
        panic!("a create")
    };
    assert_eq!(draft.user, None);
    assert_eq!(draft.port, None);
    assert_eq!(draft.identity_file, None);
    assert_eq!(draft.proxy_jump, None);
    assert!(matches!(draft.password, PasswordChange::Keep));
}

#[test]
fn refusals_land_on_the_field_their_code_names() {
    for (code, field) in [
        ("host_invalid_alias", HostField::Alias),
        ("host_alias_taken", HostField::Alias),
        ("host_invalid_hostname", HostField::HostName),
        ("host_invalid_user", HostField::User),
        ("host_invalid_port", HostField::Port),
        ("host_invalid_identity_file", HostField::Key),
        ("host_invalid_proxy_jump", HostField::Jump),
        ("host_proxy_jump_cycle", HostField::Jump),
        ("host_tag", HostField::Tags),
        ("host_group", HostField::Group),
        ("host_notes", HostField::Notes),
    ] {
        let mut form = HostForm::create(&[]);
        form.busy = true;
        form.refused(summary(code), Path::new("/home/alice"));
        assert!(!form.busy);
        assert_eq!(form.focus, field, "{code}");
        assert!(matches!(&form.error, Some((Some(f), e)) if *f == field && e.hint.is_some()));
    }
    // Another code: on the whole form, focus unchanged.
    let mut form = HostForm::create(&[]);
    form.focus = HostField::Group;
    form.refused(summary("host_file_unmanaged"), Path::new("/home/alice"));
    assert_eq!(form.focus, HostField::Group);
    assert!(matches!(&form.error, Some((None, _))));
    // A code naming a field the form does not show stays general.
    let mut dup = HostForm::duplicate("web");
    dup.refused(summary("host_invalid_hostname"), Path::new("/home/alice"));
    assert!(matches!(&dup.error, Some((None, _))));
    dup.refused(summary("host_alias_taken"), Path::new("/home/alice"));
    assert!(matches!(&dup.error, Some((Some(HostField::Alias), _))));
    // Typing in the field clears its error.
    dup.handle_key(k(KeyCode::Char('2')));
    assert!(dup.error.is_none());
}

#[test]
fn refusals_read_as_sentences_and_keep_an_alias_as_typed() {
    let home = Path::new("/home/alice");
    let mut form = HostForm::create(&["bastion".into()]);
    typed(&mut form, "alpha");
    form.refused(
        ErrorSummary {
            code: "host_alias_taken".into(),
            message: "alpha is already defined in /home/alice/.ssh/config:1".into(),
            hint: None,
        },
        home,
    );
    assert!(matches!(
        &form.error,
        Some((_, e)) if e.message == "alpha is already defined in ~/.ssh/config:1"
    ));
    form.refused(
        ErrorSummary {
            code: "host_invalid_hostname".into(),
            message: "invalid host name: empty".into(),
            hint: None,
        },
        home,
    );
    assert!(matches!(
        &form.error,
        Some((_, e)) if e.message == "Invalid host name: empty"
    ));
}

/// An edit form whose `hosts.conf` values are read (`remembered`: a
/// password is remembered for the account).
fn edit_form(remembered: bool) -> HostForm {
    let entry = crate::host_list::test_host("web", None);
    let mut form = HostForm::edit(&entry, &[]);
    form.set_managed(
        &ManagedHostDetails {
            host: portico_core::managed_hosts::ManagedHost {
                alias: "web".into(),
                hostname: "203.0.113.10".into(),
                user: Some("deploy".into()),
                port: Some(2222),
                identity_file: None,
                proxy_jump: None,
            },
            password_remembered: remembered,
            imported_from: Some(ImportSource::Termix),
        },
        Path::new("/home/alice"),
    );
    form
}

#[test]
fn nothing_remembered_offers_set_only() {
    let mut form = edit_form(false);
    assert_eq!(form.password_mode, PasswordMode::Change);
    assert_eq!(form.password_mode_label(), "Set");
    assert!(!form.fields().contains(&HostField::PasswordMode));
    assert!(form.fields().contains(&HostField::Password));
    assert!(!form.dirty(), "Set with an empty field is no change");
    // Empty: nothing to remember; typed: set.
    form.hostname.push('9');
    assert!(matches!(form.draft().password, PasswordChange::Keep));
    assert!(!form.forgets_on_destination_change());
    form.focus = HostField::Password;
    typed(&mut form, "pw");
    assert!(form.sets_password());
    assert!(form.password_set_or_kept());
    assert_eq!(form.imported_from, Some(ImportSource::Termix));
}

#[test]
fn a_new_destination_forgets_the_kept_password() {
    let mut form = edit_form(true);
    assert_eq!(form.password_mode, PasswordMode::Keep);
    assert_eq!(form.password_mode_label(), "Keep");
    assert!(form.password_set_or_kept() && !form.forgets_on_destination_change());
    for (field, extra) in [
        (HostField::HostName, "x"),
        (HostField::User, "x"),
        (HostField::Port, "1"),
    ] {
        let mut form = edit_form(true);
        form.focus = field;
        typed(&mut form, extra);
        assert!(form.destination_changed(), "{field:?}");
        assert!(form.forgets_on_destination_change(), "{field:?}");
        assert!(!form.password_set_or_kept(), "{field:?}");
        // Back as it was: kept again.
        form.handle_key(k(KeyCode::Backspace));
        assert!(!form.destination_changed(), "{field:?}");
    }
    // Change, with a new password: it goes to the new destination.
    form.hostname.push('9');
    form.focus = HostField::PasswordMode;
    form.handle_key(k(KeyCode::Right));
    assert_eq!(form.password_mode, PasswordMode::Change);
    assert_eq!(form.password_mode_label(), "Change");
    assert!(!form.forgets_on_destination_change());
}

#[test]
fn a_missing_vault_waits_then_retries_or_returns_to_the_password() {
    let home = Path::new("/home/alice");
    for code in ["vault_missing", "vault_locked"] {
        assert_eq!(field_of(code), Some(HostField::Password));
    }
    let mut form = HostForm::create(&[]);
    typed(&mut form, "db");
    form.focus = HostField::Password;
    typed(&mut form, "pw");
    assert!(form.sets_password());
    assert!(matches!(
        form.handle_key(k(KeyCode::Enter)),
        FormOutcome::Save(_)
    ));
    form.wait_for_vault(summary("vault_missing"));
    assert!(!form.busy && form.error.is_none());
    // Ready: the same save again, the password still in it.
    let Some(HostRequest::Create(draft)) = form.retry_after_vault() else {
        panic!("a retry")
    };
    let PasswordChange::Set(password) = &draft.password else {
        panic!("the password")
    };
    assert_eq!(password.expose_secret(), "pw");
    assert!(form.busy);
    assert!(form.retry_after_vault().is_none(), "only once");
    // Declined: the refusal under the password, still typed.
    form.wait_for_vault(summary("vault_missing"));
    form.vault_declined(home);
    assert!(matches!(
        &form.error,
        Some((Some(HostField::Password), e)) if e.code == "vault_missing"
    ));
    assert!(form.password_typed());
    assert_eq!(form.focus, HostField::Password);
}

#[test]
fn shared_labels() {
    let form = HostForm::create(&[]);
    assert_eq!(
        form.keys.first().map(|k| k.label.as_str()),
        Some("none · agent and default keys")
    );
    assert_eq!(
        origin_with_source(HostOrigin::Portico, Some(ImportSource::Termius)),
        "Portico · from Termius"
    );
    assert_eq!(origin_with_source(HostOrigin::Portico, None), "Portico");
    assert_eq!(
        read_only_hint(HostOrigin::Imported),
        "Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it."
    );
}

#[test]
fn esc_asks_before_dropping_changes_and_wipes_the_password() {
    let mut form = HostForm::create(&[]);
    assert!(matches!(
        form.handle_key(k(KeyCode::Esc)),
        FormOutcome::Close
    ));

    let mut form = HostForm::create(&[]);
    typed(&mut form, "web");
    assert!(matches!(
        form.handle_key(k(KeyCode::Esc)),
        FormOutcome::None
    ));
    assert!(form.confirm_discard);
    // Another key leaves the question up (no "any key"), and types nothing.
    for code in [KeyCode::Char('x'), KeyCode::Enter, KeyCode::Tab] {
        assert!(matches!(form.handle_key(k(code)), FormOutcome::None));
        assert!(form.confirm_discard, "{code:?}");
    }
    assert_eq!(form.alias, "web");
    // `n` goes back to the form, nothing lost; so does `Esc`.
    assert!(matches!(
        form.handle_key(k(KeyCode::Char('n'))),
        FormOutcome::None
    ));
    assert!(!form.confirm_discard);
    assert_eq!(form.alias, "web");
    form.handle_key(k(KeyCode::Esc));
    assert!(form.confirm_discard);
    assert!(matches!(
        form.handle_key(k(KeyCode::Esc)),
        FormOutcome::None
    ));
    assert!(!form.confirm_discard);
    assert_eq!(form.alias, "web");
    // A typed password alone is a change, and is wiped on discard.
    let mut form = HostForm::create(&[]);
    form.focus = HostField::Password;
    typed(&mut form, "pw");
    form.handle_key(k(KeyCode::Esc));
    assert!(matches!(
        form.handle_key(k(KeyCode::Char('y'))),
        FormOutcome::Close
    ));
    assert!(!form.password_typed());
}

#[test]
fn edit_password_choices() {
    let mut form = HostForm::create(&[]);
    form.mode = FormMode::Edit {
        original: "web".into(),
    };
    typed(&mut form, "web");
    form.initial = form.snapshot();
    form.password_remembered = Some(true);
    // Nothing changed: ↵ just closes.
    assert!(matches!(
        form.handle_key(k(KeyCode::Enter)),
        FormOutcome::Close
    ));
    form.focus = HostField::PasswordMode;
    form.handle_key(k(KeyCode::Left));
    assert_eq!(form.password_mode, PasswordMode::Forget);
    let FormOutcome::Save(HostRequest::Update { original, draft }) =
        form.handle_key(k(KeyCode::Enter))
    else {
        panic!("an update")
    };
    assert_eq!(original, "web");
    assert!(matches!(draft.password, PasswordChange::Forget));
    // Change, type, then leave Change: the typed password is wiped.
    form.busy = false;
    form.handle_key(k(KeyCode::Right));
    form.handle_key(k(KeyCode::Right));
    assert_eq!(form.password_mode, PasswordMode::Change);
    form.handle_key(k(KeyCode::Tab));
    assert_eq!(form.focus, HostField::Password);
    typed(&mut form, "new");
    form.handle_key(k(KeyCode::BackTab));
    form.handle_key(k(KeyCode::Right));
    assert_eq!(form.password_mode, PasswordMode::Forget);
    assert!(!form.password_typed());
}

#[test]
fn read_only_offers_duplicate_and_tags() {
    let mut form = HostForm::read_only("Web.Prod", HostOrigin::SshConfig);
    assert!(form.fields().is_empty());
    assert!(matches!(
        form.handle_key(k(KeyCode::Char('t'))),
        FormOutcome::EditMeta(a) if a == "Web.Prod"
    ));
    assert!(matches!(
        form.handle_key(k(KeyCode::Char('D'))),
        FormOutcome::None
    ));
    assert_eq!(
        form.mode,
        FormMode::Duplicate {
            source: "Web.Prod".into()
        }
    );
    assert_eq!(form.alias, "web.prod-copy");
    let FormOutcome::Save(HostRequest::Duplicate { source, new_alias }) =
        form.handle_key(k(KeyCode::Enter))
    else {
        panic!("a duplicate")
    };
    assert_eq!(
        (source.as_str(), new_alias.as_str()),
        ("Web.Prod", "web.prod-copy")
    );
    assert_eq!(copy_alias("-My Host!"), "my-host--copy");
    assert_eq!(copy_alias("..."), "host-copy");
    assert_eq!(read_only_hint(HostOrigin::SshConfig), READ_ONLY_HINT);
    assert_eq!(origin_label(HostOrigin::Imported), "Imported");
}

#[test]
fn a_key_kept_from_the_host_stays_listed() {
    let home = Path::new("/home/alice");
    let mut form = HostForm::create(&[]);
    form.keys.push(KeyChoice {
        path: Some(PathBuf::from("/home/alice/keys/work")),
        label: "~/keys/work".into(),
        note: String::new(),
    });
    form.key = 1;
    form.set_keys(&[key("id_ed25519")], home);
    assert_eq!(form.keys.len(), 3);
    assert_eq!(
        form.keys
            .get(form.key)
            .map(|c| (c.label.as_str(), c.note.as_str())),
        Some(("~/keys/work", "not discovered"))
    );
    // Pastes: first line, digits only in the port, never in a picker.
    form.focus = HostField::Port;
    form.paste("2 2\n33");
    assert_eq!(form.port, "22");
    form.focus = HostField::Key;
    form.paste("x");
    assert_eq!(form.key, 2);
}

#[test]
fn managed_values_fill_the_edit_form_whatever_comes_first() {
    let home = Path::new("/home/alice");
    let entry = crate::host_list::test_host("web", None);
    let managed = ManagedHostDetails {
        password_remembered: true,
        imported_from: None,
        host: portico_core::managed_hosts::ManagedHost {
            alias: "web".into(),
            hostname: "203.0.113.10".into(),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_file: Some(PathBuf::from("/home/alice/.ssh/id_ed25519")),
            proxy_jump: Some("Bastion".into()),
        },
    };
    let aliases = vec!["bastion".to_owned(), "web".to_owned()];
    // Values first, then the keys.
    let mut form = HostForm::edit(&entry, &aliases);
    assert!(form.loading);
    form.handle_key(k(KeyCode::Char('x')));
    form.paste("y");
    assert_eq!(form.alias, "web", "nothing typed while loading");
    form.set_managed(&managed, home);
    assert!(!form.loading && !form.dirty());
    assert_eq!(
        (
            form.hostname.as_str(),
            form.user.as_str(),
            form.port.as_str()
        ),
        ("203.0.113.10", "deploy", "2222")
    );
    assert_eq!(form.jumps.get(form.jump), Some(&Some("bastion".to_owned())));
    form.set_keys(&[key("id_rsa"), key("id_ed25519")], home);
    assert_eq!(
        form.keys.get(form.key).and_then(|c| c.path.clone()),
        managed.host.identity_file
    );
    // Keys first, then the values.
    let mut form = HostForm::edit(&entry, &aliases);
    form.set_keys(&[key("id_rsa"), key("id_ed25519")], home);
    form.set_managed(&managed, home);
    assert_eq!(form.keys.len(), 3);
    assert_eq!(
        form.keys.get(form.key).map(|c| c.label.as_str()),
        Some("~/.ssh/id_ed25519")
    );
    assert!(!form.dirty());
    // A key that is not discovered stays listed, and says so.
    let mut form = HostForm::edit(&entry, &aliases);
    form.set_keys(&[key("id_rsa")], home);
    form.set_managed(&managed, home);
    assert_eq!(
        form.keys
            .get(form.key)
            .map(|c| (c.label.as_str(), c.note.as_str())),
        Some(("~/.ssh/id_ed25519", "not discovered"))
    );
}
