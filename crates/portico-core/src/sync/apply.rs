//! Validated application of the merged state to the local files (S1b,
//! `docs/SYNC.md` §4.4, §4.7, security review F1, F7, F8).
//!
//! Only [`Projection::known`] registers holding **one** value are applied
//! (a conflict waits for the user; a held register, written by a newer
//! Portico, is never applied and never deletes anything). Order: hosts
//! (one write of `hosts.conf`, jump hosts first) → metadata → snippets →
//! settings. Every received value goes through `validate_record`, then the
//! existing validators of the target file; a value that fails is
//! **quarantined**: not written, kept in the state, reported
//! (`sync_record_invalid` names the kind and the field, never the value).
//!
//! Hosts (F1):
//! - the import's shadowing checks: an alias of the user's own config (or
//!   the old import file), a name that reads like another host,
//!   `localhost`, a jump host or a `HostName` the config relies on →
//!   `sync_alias_shadowed`;
//! - a **new** host, or a change of `HostName`, `Port`, `User`,
//!   `ProxyJump` or `IdentityFile`, is written only once confirmed on this
//!   device ([`PendingHost`]); a refusal keeps it out until another
//!   destination arrives. Nothing else (no bulk choice) confirms;
//! - an unknown jump host holds the host (`sync_jump_missing`); a missing
//!   identity file only warns (`sync_key_missing`);
//! - a received deletion removes the host from `hosts.conf` (refused while
//!   another host jumps through it); its metadata and remembered password
//!   are left alone. `~/.ssh/config` is never written.
//!
//! Snippets (F8): a snippet written from sync is recorded in
//! `confirm.toml` **before** it is written to `snippets.toml` (a failure
//! there writes nothing) and stays there until the user confirms that
//! exact content; the engine refuses to type it before that.
//!
//! A key this device holds in a form that cannot be synced (`unsynced`:
//! an identity file outside the home directory, a value its own file
//! accepts but sync does not) is never overwritten nor deleted by a
//! received value: `sync_local_host_unsynced` / `sync_local_unsynced`.

use std::collections::{BTreeMap, BTreeSet};

use super::SyncError;
use super::clock::Dot;
use super::model::{
    HostValue, MetaField, Projection, RecordKey, RecordValue, SettingKey, TypedSibling, WireValue,
    validate_record,
};
use super::project::{
    KIND_HOST, KIND_META, KIND_SETTING, KIND_SNIPPET, LocalView, destination, managed_host,
    snippet_from,
};
use super::state::{content_hash, edit_confirmations};
use crate::engine::EngineConfig;
use crate::hosts::{HostMeta, MetaChange};
use crate::managed_hosts::{SyncHostRefusal, SyncedHosts};
use crate::snippets::SnippetStore;

/// A problem with one record (or a whole kind) on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    /// Stable key (`host:prod`, `host_meta:prod:tags`, `snippet:x`,
    /// `setting:theme`, `kind:hosts`…).
    pub(crate) key: String,
    pub(crate) record: Option<RecordKey>,
    pub(crate) error: SyncError,
    /// Applied anyway (a missing key file).
    pub(crate) warning: bool,
    /// "Skip on this device" makes sense.
    pub(crate) skippable: bool,
}

/// A received host waiting for a confirmation on this device (F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingHost {
    pub(crate) alias: String,
    pub(crate) current: Option<HostValue>,
    pub(crate) incoming: HostValue,
    /// [`destination_hash`] of `incoming`: what a confirmation is about.
    pub(crate) hash: String,
    pub(crate) dots: Vec<Dot>,
}

/// What [`apply`] did.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApplyOutcome {
    pub(crate) issues: Vec<Issue>,
    pub(crate) pending: Vec<PendingHost>,
    pub(crate) hosts_changed: bool,
    pub(crate) meta_changed: Vec<(String, HostMeta)>,
    pub(crate) snippets_changed: bool,
    /// Aliases whose received destination was written to `hosts.conf`
    /// (their confirmation is used up, F1).
    pub(crate) written_hosts: Vec<String>,
    pub(crate) settings_changed: Option<crate::settings::Settings>,
}

/// Everything [`apply`] reads.
#[derive(Debug)]
pub(crate) struct ApplyInput<'a> {
    pub(crate) config: &'a EngineConfig,
    pub(crate) snippets: &'a SnippetStore,
    pub(crate) projection: &'a Projection,
    pub(crate) view: &'a LocalView,
    pub(crate) excluded: &'a BTreeSet<String>,
    pub(crate) confirmed: &'a BTreeMap<String, String>,
    pub(crate) rejected: &'a BTreeMap<String, String>,
}

/// The issue key of a record.
pub(crate) fn issue_key(key: &RecordKey) -> String {
    match key {
        RecordKey::Host { alias } => format!("host:{alias}"),
        RecordKey::HostMeta { alias, field } => {
            let field = match field {
                MetaField::Tags => "tags",
                MetaField::Group => "group",
                MetaField::Favorite => "favorite",
                MetaField::Notes => "notes",
            };
            format!("host_meta:{alias}:{field}")
        }
        RecordKey::Snippet { id } => format!("snippet:{id}"),
        RecordKey::Setting { key } => format!("setting:{}", key.path()),
        RecordKey::Secret { .. } => "secret".to_owned(),
    }
}

/// Hash of a host's destination (what a confirmation is about).
pub(crate) fn destination_hash(value: &HostValue) -> String {
    let wire = WireValue::encode(&RecordValue::Host(destination(value)))
        .map(|w| w.as_str().to_owned())
        .unwrap_or_default();
    content_hash("portico-sync/1 host destination", &wire)
}

/// Hash of the sides of a conflict as shown (A5, S1c review): every
/// side's value and the dots that wrote it, in order. A resolution names it
/// so a conflict whose sides changed since (a pass merged a new write) is
/// refused with `NotPending` instead of resolving content never shown.
pub(crate) fn sides_hash(siblings: &[TypedSibling]) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    for sibling in siblings {
        let wire = WireValue::encode(&sibling.value)
            .map(|w| w.as_str().to_owned())
            .unwrap_or_default();
        let _ = write!(text, "{}\0{wire}\0", sibling.dots.len());
        for dot in &sibling.dots {
            let _ = write!(text, "{}:{}\0", dot.device, dot.counter);
        }
    }
    content_hash("portico-sync/1 conflict sides", &text)
}

/// Hash of a snippet's command as received (what F8 confirms).
pub(crate) fn snippet_hash(command: &str, run: bool) -> String {
    content_hash(
        "portico-sync/1 snippet",
        &format!("{}\0{command}", u8::from(run)),
    )
}

/// The snippets whose current content is gated in `confirm.toml` (changed
/// by sync, not confirmed on this device, F8). Blocking.
pub(crate) fn unconfirmed_snippets(
    paths: &crate::store::Paths,
    store: &SnippetStore,
) -> crate::Result<Vec<super::api::UnconfirmedSnippet>> {
    let confirmations = super::state::load_confirmations(paths)?;
    if confirmations.snippets.is_empty() {
        return Ok(Vec::new());
    }
    Ok(store
        .list()?
        .into_iter()
        .filter_map(|s| {
            let hash = snippet_hash(&s.command, s.run);
            let gated = confirmations.snippets.get(&s.id) == Some(&hash);
            gated.then_some(super::api::UnconfirmedSnippet {
                id: s.id,
                snippet: super::api::SyncSnippetValue {
                    name: s.name,
                    command: s.command,
                    description: s.description,
                    run: s.run,
                },
                hash,
            })
        })
        .collect())
}

fn issue(key: &RecordKey, error: SyncError) -> Issue {
    Issue {
        key: issue_key(key),
        record: Some(key.clone()),
        error,
        warning: false,
        skippable: true,
    }
}

/// The value to apply for `key`: its single sibling (a conflict waits).
fn single(siblings: &[TypedSibling]) -> Option<&TypedSibling> {
    match siblings {
        [one] => Some(one),
        _ => None,
    }
}

fn invalid_field(field: &'static str, reason: &'static str) -> SyncError {
    SyncError::InvalidRecord {
        kind: KIND_HOST,
        field,
        reason,
    }
}

fn refusal_error(alias: &str, refusal: SyncHostRefusal) -> SyncError {
    match refusal {
        SyncHostRefusal::Invalid { field, reason } => invalid_field(field, reason),
        SyncHostRefusal::Shadowed => SyncError::AliasShadowed {
            alias: alias.to_owned(),
        },
        SyncHostRefusal::JumpMissing { jump } => SyncError::JumpMissing {
            alias: alias.to_owned(),
            jump,
        },
        SyncHostRefusal::Cycle => invalid_field("proxy_jump", "leads back to the host"),
        SyncHostRefusal::InUse { by } => SyncError::HostInUse {
            alias: alias.to_owned(),
            by,
        },
    }
}

/// A written host whose identity file is missing here only warns
/// (`sync_key_missing`): OpenSSH tolerates it.
fn warn_missing_keys(written: &[crate::managed_hosts::ManagedHost], out: &mut ApplyOutcome) {
    for host in written {
        if let Some(path) = &host.identity_file
            && !path.is_file()
        {
            out.issues.push(Issue {
                warning: true,
                skippable: false,
                ..issue(
                    &RecordKey::Host {
                        alias: host.alias.clone(),
                    },
                    SyncError::KeyMissing {
                        alias: host.alias.clone(),
                    },
                )
            });
        }
    }
}

/// A host this device holds in a form sync cannot represent: never
/// overwritten (nor called "new"), never deleted.
fn local_unsyncable(
    input: &ApplyInput<'_>,
    synced: &SyncedHosts<'_>,
    key: &RecordKey,
    alias: &str,
) -> bool {
    input.view.unsynced.contains(key)
        || synced.hosts().iter().any(|h| {
            h.alias.eq_ignore_ascii_case(alias)
                && super::project::host_value(&input.config.paths.home, h).is_none()
        })
}

/// Applies the hosts. Blocking.
fn apply_hosts(input: &ApplyInput<'_>, out: &mut ApplyOutcome) -> crate::Result<()> {
    if input.view.suspended.contains(KIND_HOST) {
        return Ok(());
    }
    let home = &input.config.paths.home;
    let mut wanted: Vec<(&RecordKey, &str, &TypedSibling)> = Vec::new();
    for (key, siblings) in &input.projection.known {
        let RecordKey::Host { alias } = key else {
            continue;
        };
        if input.excluded.contains(&issue_key(key)) {
            continue;
        }
        if let Some(sibling) = single(siblings) {
            wanted.push((key, alias.as_str(), sibling));
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let synced = SyncedHosts::open(input.config)?;
    let current_of = |alias: &str| {
        synced
            .hosts()
            .iter()
            .find(|h| h.alias.eq_ignore_ascii_case(alias))
            .and_then(|h| super::project::host_value(home, h))
    };
    let mut upserts = Vec::new();
    let mut deletes = Vec::new();
    for (key, alias, sibling) in wanted {
        if local_unsyncable(input, &synced, key, alias) {
            out.issues.push(issue(
                key,
                SyncError::LocalHostUnsynced {
                    alias: alias.to_owned(),
                },
            ));
            continue;
        }
        let current = current_of(alias);
        let incoming = match &sibling.value {
            RecordValue::Tombstone => {
                if synced
                    .hosts()
                    .iter()
                    .any(|h| h.alias.eq_ignore_ascii_case(alias))
                {
                    deletes.push(alias.to_owned());
                }
                continue;
            }
            RecordValue::Host(value) => value,
            _ => {
                out.issues
                    .push(issue(key, invalid_field("value", "is not a host")));
                continue;
            }
        };
        if current.as_ref().map(destination) == Some(destination(incoming)) {
            continue;
        }
        if let Err(e) = validate_record(key, &sibling.value) {
            out.issues.push(issue(key, e));
            continue;
        }
        let host = managed_host(home, alias, incoming);
        if let Err(refusal) = synced.check(&host) {
            out.issues.push(issue(key, refusal_error(alias, refusal)));
            continue;
        }
        let hash = destination_hash(incoming);
        if input.rejected.get(alias) == Some(&hash) {
            continue;
        }
        if input.confirmed.get(alias) != Some(&hash) {
            out.pending.push(PendingHost {
                alias: alias.to_owned(),
                current,
                incoming: incoming.clone(),
                hash,
                dots: sibling.dots.clone(),
            });
            continue;
        }
        upserts.push(host);
    }
    let mut written: Vec<_> = upserts.clone();
    let (changed, refused) = synced.apply(upserts, &deletes)?;
    out.hosts_changed = changed;
    written.retain(|host| !refused.iter().any(|(alias, _)| *alias == host.alias));
    out.written_hosts = written.iter().map(|h| h.alias.clone()).collect();
    for (alias, refusal) in refused {
        let key = RecordKey::Host {
            alias: alias.clone(),
        };
        out.issues.push(issue(&key, refusal_error(&alias, refusal)));
    }
    warn_missing_keys(&written, out);
    Ok(())
}

/// The metadata change for a received value, normalized; a value the
/// editor would store differently is quarantined (it would never settle).
fn meta_change(field: MetaField, value: &RecordValue) -> Result<MetaChange, SyncError> {
    let invalid = |field, reason| SyncError::InvalidRecord {
        kind: KIND_META,
        field,
        reason,
    };
    let change = match (field, value) {
        (MetaField::Tags, RecordValue::Tags(tags)) => {
            MetaChange::Tags(tags.iter().cloned().collect())
        }
        (MetaField::Tags, RecordValue::Tombstone) => MetaChange::Tags(Vec::new()),
        (MetaField::Group, RecordValue::Text(text)) => MetaChange::Group(Some(text.clone())),
        (MetaField::Group, RecordValue::Tombstone) => MetaChange::Group(None),
        (MetaField::Notes, RecordValue::Text(text)) => MetaChange::Notes(Some(text.clone())),
        (MetaField::Notes, RecordValue::Tombstone) => MetaChange::Notes(None),
        (MetaField::Favorite, RecordValue::Bool(on)) => MetaChange::Favorite(*on),
        (MetaField::Favorite, RecordValue::Tombstone) => MetaChange::Favorite(false),
        _ => return Err(invalid("value", "does not match the field")),
    };
    let normalized = change
        .clone()
        .normalized()
        .map_err(|_| invalid("value", "is refused by hosts.toml"))?;
    let same = match (&change, &normalized) {
        (MetaChange::Tags(a), MetaChange::Tags(b)) => {
            a.iter().collect::<BTreeSet<_>>() == b.iter().collect::<BTreeSet<_>>()
        }
        (a, b) => a == b,
    };
    if !same {
        return Err(invalid("value", "is not in the form hosts.toml stores"));
    }
    Ok(normalized)
}

/// Applies the metadata. Blocking.
fn apply_meta(input: &ApplyInput<'_>, out: &mut ApplyOutcome) {
    if input.view.suspended.contains(KIND_META) {
        return;
    }
    let mut by_alias: BTreeMap<&str, Vec<MetaChange>> = BTreeMap::new();
    for (key, siblings) in &input.projection.known {
        let RecordKey::HostMeta { alias, field } = key else {
            continue;
        };
        let Some(sibling) = single(siblings) else {
            continue;
        };
        if input.excluded.contains(&issue_key(key)) {
            continue;
        }
        if input.view.unsynced.contains(key) {
            out.issues.push(issue(
                key,
                SyncError::LocalRecordUnsynced { kind: KIND_META },
            ));
            continue;
        }
        let current = input.view.records.get(key);
        let unchanged = match &sibling.value {
            RecordValue::Tombstone => current.is_none(),
            value => current == Some(value),
        };
        if unchanged {
            continue;
        }
        if let Err(e) = validate_record(key, &sibling.value) {
            out.issues.push(issue(key, e));
            continue;
        }
        match meta_change(*field, &sibling.value) {
            Ok(change) => by_alias.entry(alias.as_str()).or_default().push(change),
            Err(e) => out.issues.push(issue(key, e)),
        }
    }
    let path = input.config.paths.hosts_meta();
    for (alias, changes) in by_alias {
        match crate::hosts::replace_meta(&path, alias, None, changes) {
            Ok(meta) => out.meta_changed.push((alias.to_owned(), meta)),
            Err(e) => out.issues.push(Issue {
                key: format!("host_meta:{alias}"),
                record: None,
                error: SyncError::FolderIo {
                    op: "write hosts.toml",
                    kind: e.summary().code,
                },
                warning: false,
                skippable: false,
            }),
        }
    }
}

/// Applies the snippets. Blocking.
fn apply_snippets(input: &ApplyInput<'_>, out: &mut ApplyOutcome) {
    if input.view.suspended.contains(KIND_SNIPPET) {
        return;
    }
    for (key, siblings) in &input.projection.known {
        let RecordKey::Snippet { id } = key else {
            continue;
        };
        let Some(sibling) = single(siblings) else {
            continue;
        };
        if input.excluded.contains(&issue_key(key)) {
            continue;
        }
        if input.view.unsynced.contains(key) {
            out.issues.push(issue(
                key,
                SyncError::LocalRecordUnsynced { kind: KIND_SNIPPET },
            ));
            continue;
        }
        let current = input.view.records.get(key);
        match &sibling.value {
            RecordValue::Tombstone => {
                if current.is_some() {
                    match input.snippets.delete(id) {
                        Ok(()) => {
                            out.snippets_changed = true;
                            super::state::forget_confirmation(&input.config.paths, id);
                        }
                        Err(e) => out.issues.push(issue(
                            key,
                            SyncError::FolderIo {
                                op: "write snippets.toml",
                                kind: e.summary().code,
                            },
                        )),
                    }
                }
            }
            RecordValue::Snippet(value) => {
                if current == Some(&sibling.value) {
                    continue;
                }
                if let Err(e) = validate_record(key, &sibling.value) {
                    out.issues.push(issue(key, e));
                    continue;
                }
                let snippet = snippet_from(id, value);
                let mut normalized = snippet.clone();
                normalized.normalize();
                if normalized != snippet || snippet.validate().is_err() {
                    out.issues.push(issue(
                        key,
                        SyncError::InvalidRecord {
                            kind: KIND_SNIPPET,
                            field: "command",
                            reason: "is refused by the snippet safety rules",
                        },
                    ));
                    continue;
                }
                // F8: the gate first, then the snippet (a failure to
                // record the gate writes nothing).
                let paths = &input.config.paths;
                let gate = |stored: &crate::snippets::Snippet| {
                    edit_confirmations(paths, |c| {
                        c.snippets
                            .insert(stored.id.clone(), snippet_hash(&stored.command, stored.run));
                    })
                };
                match input.snippets.save_guarded(snippet, gate) {
                    Ok(_) => out.snippets_changed = true,
                    Err(e) => out.issues.push(issue(
                        key,
                        SyncError::FolderIo {
                            op: "write snippets.toml",
                            kind: e.summary().code,
                        },
                    )),
                }
            }
            _ => out.issues.push(issue(
                key,
                SyncError::InvalidRecord {
                    kind: KIND_SNIPPET,
                    field: "value",
                    reason: "is not a snippet",
                },
            )),
        }
    }
}

/// Applies the settings. Blocking.
fn apply_settings(input: &ApplyInput<'_>, out: &mut ApplyOutcome) {
    if input.view.suspended.contains(KIND_SETTING) {
        return;
    }
    let mut edits: Vec<(SettingKey, Option<String>)> = Vec::new();
    for (key, siblings) in &input.projection.known {
        let RecordKey::Setting { key: setting } = key else {
            continue;
        };
        let Some(sibling) = single(siblings) else {
            continue;
        };
        if input.excluded.contains(&issue_key(key)) {
            continue;
        }
        if input.view.unsynced.contains(key) {
            out.issues.push(issue(
                key,
                SyncError::LocalRecordUnsynced { kind: KIND_SETTING },
            ));
            continue;
        }
        let current = input.view.records.get(key);
        let value = match &sibling.value {
            RecordValue::Tombstone if current.is_none() => continue,
            RecordValue::Tombstone => None,
            RecordValue::Text(_) if current == Some(&sibling.value) => continue,
            RecordValue::Text(text) => Some(text.clone()),
            _ => {
                out.issues.push(issue(
                    key,
                    SyncError::InvalidRecord {
                        kind: KIND_SETTING,
                        field: "value",
                        reason: "is not text",
                    },
                ));
                continue;
            }
        };
        if let Err(e) = validate_record(key, &sibling.value) {
            out.issues.push(issue(key, e));
            continue;
        }
        edits.push((setting.clone(), value));
    }
    if edits.is_empty() {
        return;
    }
    let result = crate::settings::update_with(&input.config.paths.settings(), |settings| {
        for (key, value) in edits {
            match key {
                SettingKey::Theme => settings.theme = value,
                SettingKey::DarkTheme => settings.dark_theme = value,
                SettingKey::KeymapTui(action) => match value {
                    Some(binding) => {
                        settings.keymap.tui.insert(action, binding);
                    }
                    None => {
                        settings.keymap.tui.remove(&action);
                    }
                },
                SettingKey::KeymapGui(action) => match value {
                    Some(binding) => {
                        settings.keymap.gui.insert(action, binding);
                    }
                    None => {
                        settings.keymap.gui.remove(&action);
                    }
                },
            }
        }
    });
    match result {
        Ok(settings) => out.settings_changed = Some(settings),
        Err(e) => out.issues.push(Issue {
            key: "kind:settings".to_owned(),
            record: None,
            error: SyncError::InvalidRecord {
                kind: KIND_SETTING,
                field: "value",
                reason: if matches!(e, crate::Error::Settings(_)) {
                    "is refused by settings.toml"
                } else {
                    "could not be written"
                },
            },
            warning: false,
            skippable: false,
        }),
    }
}

/// Applies `input.projection` to the local files (see the module doc).
/// Blocking.
pub(crate) fn apply(input: &ApplyInput<'_>) -> ApplyOutcome {
    let mut out = ApplyOutcome::default();
    for held in &input.projection.held {
        out.issues.push(Issue {
            key: held
                .key
                .as_ref()
                .map_or_else(|| format!("newer:{}", held.kind), issue_key),
            record: held.key.clone(),
            error: SyncError::RecordHeld {
                kind: match held.kind.as_str() {
                    KIND_HOST => KIND_HOST,
                    KIND_META => KIND_META,
                    KIND_SNIPPET => KIND_SNIPPET,
                    KIND_SETTING => KIND_SETTING,
                    _ => "record",
                },
            },
            warning: true,
            skippable: false,
        });
    }
    if let Err(e) = apply_hosts(input, &mut out) {
        out.issues.push(Issue {
            key: "kind:hosts".to_owned(),
            record: None,
            error: match e {
                crate::Error::ManagedHost(_) => SyncError::HostsFileUnmanaged,
                other => SyncError::FolderIo {
                    op: "write hosts.conf",
                    kind: other.summary().code,
                },
            },
            warning: false,
            skippable: false,
        });
    }
    apply_meta(input, &mut out);
    apply_snippets(input, &mut out);
    apply_settings(input, &mut out);
    out
}
