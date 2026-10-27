//! Projection of the local files into sync records (S1b, `docs/SYNC.md`
//! §4.6, security review F7).
//!
//! Every pass reads `hosts.conf`, `hosts.toml`, `snippets.toml` and
//! `settings.toml` into typed records ([`observe`]) and compares them with
//! the **last applied projection** (what those files held after the
//! previous pass, kept in the sealed replica). Any difference is a local
//! write, whoever made it: a UI, a hand edit, the other Portico process,
//! an import ([`local_changes`]).
//!
//! F7: the last applied projection only ever records what the files
//! **really held**. A received record that was not written (pending
//! confirmation, quarantined, shadowed, skipped, held for a newer
//! Portico) is absent from it and from the files, so it never turns into
//! a tombstone: only the observed disappearance of a value that was
//! really there does.
//!
//! - A file that cannot be read suspends its kind (neither diffed nor
//!   applied, its last projection kept): `hosts.conf` edited by hand with
//!   lines Portico would not rewrite gives `sync_hosts_file_unmanaged`.
//! - A local entry that cannot be represented (an identity file outside
//!   the home directory, a snippet `snippets.toml` hides as invalid…) is
//!   **unsynced**: never published, and its absence is never a deletion.
//! - Never synced: `gui.copy_on_select`, `updates.check`, import
//!   provenance, bound-password index, recent sessions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::SyncError;
use super::model::{
    HomeRelPath, HostValue, MetaField, RecordKey, RecordValue, SettingKey, SnippetScopeValue,
    SnippetValue, WireKey, WireValue, validate_record,
};
use crate::managed_hosts::ManagedHost;
use crate::snippets::{Snippet, SnippetScope, SnippetStore};
use crate::store::Paths;

pub(crate) const KIND_HOST: &str = "host";
pub(crate) const KIND_META: &str = "host_meta";
pub(crate) const KIND_SNIPPET: &str = "snippet";
pub(crate) const KIND_SETTING: &str = "setting";
/// Passwords (S2b); filtered at the merge while the policy is off (S2a).
pub(crate) const KIND_SECRET: &str = "secret";

/// The local files, as records.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalView {
    pub(crate) records: BTreeMap<RecordKey, RecordValue>,
    /// Keys present locally that cannot be synced: never diffed.
    pub(crate) unsynced: BTreeSet<RecordKey>,
    /// Kinds whose file could not be read: neither diffed nor applied.
    pub(crate) suspended: BTreeSet<&'static str>,
    /// Why kinds are suspended (issues).
    pub(crate) problems: Vec<SyncError>,
}

impl LocalView {
    pub(crate) fn is_suspended(&self, key: &RecordKey) -> bool {
        self.suspended.contains(key.kind())
    }
}

/// `ManagedHost` → synced value (`None`: cannot be synced).
pub(crate) fn host_value(home: &Path, host: &ManagedHost) -> Option<HostValue> {
    let identity_file = match &host.identity_file {
        None => None,
        Some(path) => {
            let rest = path.strip_prefix(home).ok()?;
            let text = format!("~/{}", crate::store::slash_relative(rest));
            Some(HomeRelPath::parse(&text).ok()?)
        }
    };
    Some(HostValue {
        hostname: host.hostname.clone(),
        user: host.user.clone(),
        port: host.port,
        identity_file,
        proxy_jump: host.proxy_jump.clone(),
        renamed_from: None,
    })
}

/// Synced value → `ManagedHost` (the identity path under `home`).
pub(crate) fn managed_host(home: &Path, alias: &str, value: &HostValue) -> ManagedHost {
    let identity_file = value.identity_file.as_ref().map(|path| {
        let rest = path.as_str().strip_prefix("~/").unwrap_or(path.as_str());
        rest.split('/')
            .fold(home.to_path_buf(), |dir, part| dir.join(part))
    });
    ManagedHost {
        alias: alias.to_owned(),
        hostname: value.hostname.clone(),
        user: value.user.clone(),
        port: value.port,
        identity_file,
        proxy_jump: value.proxy_jump.clone(),
    }
}

/// The destination of a host value (everything but `renamed_from`): two
/// values with the same destination need no confirmation (F1).
pub(crate) fn destination(value: &HostValue) -> HostValue {
    HostValue {
        renamed_from: None,
        ..value.clone()
    }
}

pub(crate) fn snippet_value(snippet: &Snippet) -> SnippetValue {
    SnippetValue {
        name: snippet.name.clone(),
        command: snippet.command.clone(),
        description: snippet.description.clone(),
        scope: match &snippet.scope {
            SnippetScope::Global => SnippetScopeValue::Global,
            SnippetScope::Hosts(hosts) => SnippetScopeValue::Hosts(hosts.clone()),
            SnippetScope::Tags(tags) => SnippetScopeValue::Tags(tags.clone()),
        },
        run: snippet.run,
    }
}

pub(crate) fn snippet_from(id: &str, value: &SnippetValue) -> Snippet {
    Snippet {
        id: id.to_owned(),
        name: value.name.clone(),
        command: value.command.clone(),
        description: value.description.clone(),
        scope: match &value.scope {
            SnippetScopeValue::Global => SnippetScope::Global,
            SnippetScopeValue::Hosts(hosts) => SnippetScope::Hosts(hosts.clone()),
            SnippetScopeValue::Tags(tags) => SnippetScope::Tags(tags.clone()),
        },
        run: value.run,
    }
}

/// Puts `value` under `key` when it is a valid record, else marks the key
/// unsynced.
fn record(view: &mut LocalView, key: RecordKey, value: RecordValue) {
    if validate_record(&key, &value).is_ok() {
        view.records.insert(key, value);
    } else {
        view.unsynced.insert(key);
    }
}

fn observe_hosts(paths: &Paths, view: &mut LocalView) {
    match crate::managed_hosts::read_hosts(paths) {
        Ok(hosts) => {
            for host in hosts {
                let key = RecordKey::Host {
                    alias: host.alias.clone(),
                };
                match host_value(&paths.home, &host) {
                    Some(value) => record(view, key, RecordValue::Host(value)),
                    None => {
                        view.unsynced.insert(key);
                    }
                }
            }
        }
        Err(e) => {
            view.suspended.insert(KIND_HOST);
            view.problems.push(match e {
                crate::Error::ManagedHost(_) => SyncError::HostsFileUnmanaged,
                _ => SyncError::FolderIo {
                    op: "read hosts.conf",
                    kind: e.summary().code,
                },
            });
        }
    }
}

fn observe_meta(paths: &Paths, view: &mut LocalView) {
    let Ok((meta, _warnings)) = crate::hosts::load_meta(&paths.hosts_meta()) else {
        view.suspended.insert(KIND_META);
        return;
    };
    for (alias, meta) in meta.hosts {
        let key = |field| RecordKey::HostMeta {
            alias: alias.clone(),
            field,
        };
        if !meta.tags.is_empty() {
            record(
                view,
                key(MetaField::Tags),
                RecordValue::Tags(meta.tags.iter().cloned().collect()),
            );
        }
        if let Some(group) = &meta.group {
            record(
                view,
                key(MetaField::Group),
                RecordValue::Text(group.clone()),
            );
        }
        if meta.favorite {
            record(view, key(MetaField::Favorite), RecordValue::Bool(true));
        }
        if let Some(notes) = &meta.notes {
            record(
                view,
                key(MetaField::Notes),
                RecordValue::Text(notes.clone()),
            );
        }
    }
}

fn observe_snippets(store: &SnippetStore, view: &mut LocalView) {
    let Ok((snippets, invalid)) = store.list_with_invalid() else {
        view.suspended.insert(KIND_SNIPPET);
        return;
    };
    for id in invalid {
        view.unsynced.insert(RecordKey::Snippet { id });
    }
    for snippet in snippets {
        record(
            view,
            RecordKey::Snippet {
                id: snippet.id.clone(),
            },
            RecordValue::Snippet(snippet_value(&snippet)),
        );
    }
}

fn observe_settings(paths: &Paths, view: &mut LocalView) {
    let Ok(settings) = crate::settings::load(&paths.settings()) else {
        view.suspended.insert(KIND_SETTING);
        return;
    };
    let mut put = |key: SettingKey, value: &str| {
        record(
            view,
            RecordKey::Setting { key },
            RecordValue::Text(value.to_owned()),
        );
    };
    if let Some(theme) = &settings.theme {
        put(SettingKey::Theme, theme);
    }
    if let Some(theme) = &settings.dark_theme {
        put(SettingKey::DarkTheme, theme);
    }
    for (action, binding) in &settings.keymap.tui {
        if let Some(key) = SettingKey::parse(&format!("keymap.tui.{action}")) {
            put(key, binding);
        }
    }
    for (action, binding) in &settings.keymap.gui {
        if let Some(key) = SettingKey::parse(&format!("keymap.gui.{action}")) {
            put(key, binding);
        }
    }
    // `gui.copy_on_select` and `updates.check` are never synced (F18).
}

/// Reads the local files into records. Blocking.
pub(crate) fn observe(paths: &Paths, snippets: &SnippetStore) -> LocalView {
    let mut view = LocalView::default();
    observe_hosts(paths, &mut view);
    observe_meta(paths, &mut view);
    observe_snippets(snippets, &mut view);
    observe_settings(paths, &mut view);
    view
}

/// One local change: `None` is a deletion (a tombstone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalChange {
    pub(crate) key: RecordKey,
    pub(crate) value: Option<RecordValue>,
}

/// The local changes since the last applied projection (see the module
/// doc). A host deleted and another created with the same destination in
/// one pass is a rename (`renamed_from`, `docs/SYNC.md` §4.5).
pub(crate) fn local_changes(
    view: &LocalView,
    applied: &BTreeMap<WireKey, WireValue>,
) -> Vec<LocalChange> {
    let previous: BTreeMap<RecordKey, &WireValue> = applied
        .iter()
        .filter_map(|(key, value)| key.decode().map(|key| (key, value)))
        .collect();
    let keys: BTreeSet<&RecordKey> = previous.keys().chain(view.records.keys()).collect();
    let mut changes = Vec::new();
    for key in keys {
        if view.is_suspended(key) || view.unsynced.contains(key) {
            continue;
        }
        let now = view.records.get(key);
        let now_wire = now.and_then(|value| WireValue::encode(value).ok());
        let before = previous.get(key).copied();
        if now_wire.as_ref() == before {
            continue;
        }
        changes.push(LocalChange {
            key: key.clone(),
            value: now.cloned(),
        });
    }
    mark_renames(&mut changes, applied);
    changes
}

fn mark_renames(changes: &mut [LocalChange], applied: &BTreeMap<WireKey, WireValue>) {
    let deleted: Vec<(String, HostValue)> = changes
        .iter()
        .filter(|change| change.value.is_none())
        .filter_map(|change| match &change.key {
            RecordKey::Host { alias } => {
                let wire = WireKey::encode(&change.key).ok()?;
                match applied.get(&wire)?.decode()? {
                    RecordValue::Host(value) => Some((alias.clone(), destination(&value))),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    for change in changes.iter_mut() {
        let (RecordKey::Host { .. }, Some(RecordValue::Host(value))) =
            (&change.key, &mut change.value)
        else {
            continue;
        };
        let same: Vec<&String> = deleted
            .iter()
            .filter(|(_, old)| *old == destination(value))
            .map(|(alias, _)| alias)
            .collect();
        if let [old] = same.as_slice() {
            value.renamed_from = Some((*old).clone());
        }
    }
}

/// The projection to remember after a pass: what the files hold now, and
/// the previous entries of suspended kinds and unsynced keys (never
/// forgotten while they cannot be read).
pub(crate) fn next_applied(
    view: &LocalView,
    previous: &BTreeMap<WireKey, WireValue>,
) -> BTreeMap<WireKey, WireValue> {
    let mut next: BTreeMap<WireKey, WireValue> = previous
        .iter()
        .filter(|(key, _)| {
            key.decode()
                .is_none_or(|key| view.is_suspended(&key) || view.unsynced.contains(&key))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    for (key, value) in &view.records {
        if let (Ok(key), Ok(value)) = (WireKey::encode(key), WireValue::encode(value)) {
            next.insert(key, value);
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::rooted(dir.path());
        std::fs::create_dir_all(&paths.config_dir).expect("mkdir");
        std::fs::create_dir_all(&paths.ssh_dir).expect("mkdir");
        (dir, paths)
    }

    fn host_key(alias: &str) -> RecordKey {
        RecordKey::Host {
            alias: alias.into(),
        }
    }

    #[test]
    fn local_files_project_into_records() {
        let (_dir, paths) = paths();
        std::fs::write(
            paths.managed_hosts(),
            "Host prod\n  HostName 10.0.0.1\n  User deploy\n  Port 2222\n",
        )
        .expect("w");
        std::fs::write(
            paths.hosts_meta(),
            "[hosts.prod]\ntags = [\"web\", \"eu\"]\nfavorite = true\n[hosts.mine]\ngroup = \"Home\"\n",
        )
        .expect("w");
        std::fs::write(
            paths.settings(),
            "theme = \"basalt\"\n[keymap.tui]\nconnect = \"enter\"\n[gui]\ncopy_on_select = true\n[updates]\ncheck = true\n",
        )
        .expect("w");
        let store = SnippetStore::new(paths.snippets());
        store
            .save(Snippet {
                id: String::new(),
                name: "Disk".into(),
                command: "df -h".into(),
                description: None,
                scope: SnippetScope::Global,
                run: true,
            })
            .expect("save");
        let view = observe(&paths, &store);
        assert!(view.suspended.is_empty());
        let Some(RecordValue::Host(prod)) = view.records.get(&host_key("prod")) else {
            panic!("host");
        };
        assert_eq!(prod.hostname, "10.0.0.1");
        assert_eq!(prod.port, Some(2222));
        assert!(view.records.contains_key(&RecordKey::HostMeta {
            alias: "mine".into(),
            field: MetaField::Group
        }));
        assert!(
            view.records
                .contains_key(&RecordKey::Snippet { id: "disk".into() })
        );
        let settings: Vec<String> = view
            .records
            .keys()
            .filter_map(|key| match key {
                RecordKey::Setting { key } => Some(key.path()),
                _ => None,
            })
            .collect();
        assert_eq!(settings, ["theme", "keymap.tui.connect"]);
    }

    #[test]
    fn updates_check_and_copy_on_select_are_never_synced() {
        let (_dir, paths) = paths();
        std::fs::write(
            paths.settings(),
            "[gui]\ncopy_on_select = true\n[updates]\ncheck = true\n",
        )
        .expect("w");
        let view = observe(&paths, &SnippetStore::new(paths.snippets()));
        assert!(view.records.is_empty(), "{:?}", view.records);
    }

    #[test]
    fn unmanaged_hosts_conf_suspends_hosts_only() {
        let (_dir, paths) = paths();
        std::fs::write(
            paths.managed_hosts(),
            "Host prod\n  HostName a\n  ForwardAgent yes\n",
        )
        .expect("w");
        std::fs::write(paths.hosts_meta(), "[hosts.prod]\nfavorite = true\n").expect("w");
        let view = observe(&paths, &SnippetStore::new(paths.snippets()));
        assert!(view.suspended.contains(KIND_HOST));
        assert_eq!(view.problems, vec![SyncError::HostsFileUnmanaged]);
        assert_eq!(view.records.len(), 1, "metadata still syncs");
        // The hosts' last projection is kept, and their absence is no
        // deletion.
        let applied: BTreeMap<WireKey, WireValue> = [(
            WireKey::encode(&host_key("prod")).expect("k"),
            WireValue::encode(&RecordValue::Host(HostValue {
                hostname: "a".into(),
                user: None,
                port: None,
                identity_file: None,
                proxy_jump: None,
                renamed_from: None,
            }))
            .expect("v"),
        )]
        .into();
        assert!(
            local_changes(&view, &applied)
                .iter()
                .all(|c| !matches!(c.key, RecordKey::Host { .. }))
        );
        assert!(
            next_applied(&view, &applied)
                .contains_key(&WireKey::encode(&host_key("prod")).expect("k"))
        );
    }

    #[test]
    fn hand_edits_deletions_and_renames_are_local_changes() {
        let (_dir, paths) = paths();
        let store = SnippetStore::new(paths.snippets());
        std::fs::write(paths.managed_hosts(), "Host prod\n  HostName 10.0.0.1\n").expect("w");
        let first = observe(&paths, &store);
        let changes = local_changes(&first, &BTreeMap::new());
        assert_eq!(changes.len(), 1);
        let applied = next_applied(&first, &BTreeMap::new());
        assert!(local_changes(&first, &applied).is_empty(), "idempotent");
        // A hand edit: the alias is renamed, same destination.
        std::fs::write(paths.managed_hosts(), "Host prod2\n  HostName 10.0.0.1\n").expect("w");
        let second = observe(&paths, &store);
        let changes = local_changes(&second, &applied);
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&LocalChange {
            key: host_key("prod"),
            value: None
        }));
        let renamed = changes
            .iter()
            .find(|c| c.key == host_key("prod2"))
            .and_then(|c| match &c.value {
                Some(RecordValue::Host(value)) => value.renamed_from.clone(),
                _ => None,
            });
        assert_eq!(renamed.as_deref(), Some("prod"));
    }

    #[test]
    fn invalid_local_snippets_are_neither_published_nor_deleted() {
        let (_dir, paths) = paths();
        std::fs::write(
            paths.snippets(),
            "[[snippet]]\nid = \"bad\"\nname = \"Bad\"\ncommand = \"echo \\u001b[2J\"\nscope = { kind = \"global\" }\n",
        )
        .expect("w");
        let store = SnippetStore::new(paths.snippets());
        let view = observe(&paths, &store);
        let key = RecordKey::Snippet { id: "bad".into() };
        assert!(view.unsynced.contains(&key));
        assert!(!view.records.contains_key(&key));
        let applied: BTreeMap<WireKey, WireValue> = [(
            WireKey::encode(&key).expect("k"),
            WireValue::encode(&RecordValue::Snippet(SnippetValue {
                name: "Bad".into(),
                command: "echo".into(),
                description: None,
                scope: SnippetScopeValue::Global,
                run: true,
            }))
            .expect("v"),
        )]
        .into();
        assert!(local_changes(&view, &applied).is_empty(), "no tombstone");
    }

    #[test]
    fn identity_files_round_trip_home_relative() {
        let home = Path::new("/home/a");
        let host = ManagedHost {
            alias: "prod".into(),
            hostname: "h".into(),
            user: None,
            port: None,
            identity_file: Some(home.join(".ssh").join("id_ed25519")),
            proxy_jump: None,
        };
        let value = host_value(home, &host).expect("value");
        assert_eq!(
            value.identity_file.as_ref().map(HomeRelPath::as_str),
            Some("~/.ssh/id_ed25519")
        );
        assert_eq!(managed_host(home, "prod", &value), host);
        let outside = ManagedHost {
            identity_file: Some("/etc/key".into()),
            ..host
        };
        assert_eq!(host_value(home, &outside), None, "outside home: not synced");
    }
}
