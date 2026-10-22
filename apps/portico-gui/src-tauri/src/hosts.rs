//! Hosts created in Portico (phase 6b, ADR "Hôtes créés dans Portico").
//!
//! Create, edit, duplicate and delete the hosts of Portico's own
//! `hosts.conf`, written by `portico_core::managed_hosts` (validated,
//! atomic, never `~/.ssh/config`). Every change emits `hosts_changed` (the
//! webview reloads the inventory), then `host_meta`.
//!
//! - **Key**: the webview never names a local path. It picks a key the
//!   engine discovered, by id (the file name, as in the keys dialog), or
//!   keeps the host's current `IdentityFile`, which Rust reads back from
//!   the inventory. The draft refuses unknown fields: no path can ride
//!   along.
//! - **Password**: deserialized straight into `SecretString`, handed to the
//!   engine (keyring, or the vault), never in a DTO that prints it.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use std::path::PathBuf;

use portico_core::keys::LocalKey;
use portico_core::managed_hosts::{HostDraft, ManagedHost, PasswordChange};
use portico_core::{Engine, ErrorSummary};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::dto::display_path;
use crate::keys::{key_id, private_key_path};

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

/// The key of the form: none, the host's current one, or a discovered key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum IdentityChoiceDto {
    None,
    /// Edit only: the `IdentityFile` already in `hosts.conf` (it may not be
    /// a discovered key); on creation, no key.
    Keep,
    /// A key of `Engine::local_keys()`, by file name (`id_ed25519`).
    Key {
        id: String,
    },
}

/// The password of the form.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PasswordChangeDto {
    Keep,
    // Deserialized straight into `SecretString`: no intermediate `String`.
    Set { value: SecretString },
    Forget,
}

// Manual impl: the derived one would print the password.
impl std::fmt::Debug for PasswordChangeDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Keep => "Keep",
            Self::Set { .. } => "Set([redacted])",
            Self::Forget => "Forget",
        })
    }
}

impl From<PasswordChangeDto> for PasswordChange {
    fn from(dto: PasswordChangeDto) -> Self {
        match dto {
            PasswordChangeDto::Keep => Self::Keep,
            PasswordChangeDto::Set { value } => Self::Set(value),
            PasswordChangeDto::Forget => Self::Forget,
        }
    }
}

/// The host form, as sent by the webview (validated by the engine).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HostDraftDto {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    /// `u32`: an out-of-range port reaches the engine's typed error.
    pub port: Option<u32>,
    pub identity: IdentityChoiceDto,
    pub proxy_jump: Option<String>,
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    pub notes: Option<String>,
    pub password: PasswordChangeDto,
}

impl HostDraftDto {
    /// The engine's draft, with the key resolved by [`resolve_identity`].
    fn into_draft(self, identity_file: Option<PathBuf>) -> HostDraft {
        HostDraft {
            alias: self.alias,
            hostname: self.hostname,
            user: self.user.filter(|u| !u.trim().is_empty()),
            port: self.port,
            identity_file,
            proxy_jump: self.proxy_jump.filter(|j| !j.trim().is_empty()),
            tags: self.tags,
            group: self.group,
            favorite: self.favorite,
            notes: self.notes,
            password: self.password.into(),
        }
    }
}

/// The `IdentityFile` a choice stands for. `keys` is a fresh listing from
/// the engine, `current` the host's own `IdentityFile` (edit) read from the
/// inventory: no path ever comes from the webview.
pub(crate) fn resolve_identity(
    choice: &IdentityChoiceDto,
    keys: &[LocalKey],
    current: Option<PathBuf>,
) -> Result<Option<PathBuf>, ErrorSummary> {
    match choice {
        IdentityChoiceDto::None => Ok(None),
        IdentityChoiceDto::Keep => Ok(current),
        IdentityChoiceDto::Key { id } => private_key_path(keys, id).map(Some),
    }
}

async fn identity_for(
    engine: &Engine,
    choice: &IdentityChoiceDto,
    editing: Option<&str>,
) -> Result<Option<PathBuf>, ErrorSummary> {
    match choice {
        IdentityChoiceDto::None => Ok(None),
        IdentityChoiceDto::Keep => {
            let Some(alias) = editing else {
                return Ok(None);
            };
            // What hosts.conf holds, never the resolved config (an earlier
            // value elsewhere could hide it).
            let own = engine.managed_host(alias).await.map_err(summary)?;
            Ok(own.and_then(|h| h.identity_file))
        }
        IdentityChoiceDto::Key { .. } => {
            let keys = engine.local_keys().await.map_err(summary)?;
            resolve_identity(choice, &keys, None)
        }
    }
}

/// Creates a Portico host; returns its alias as written.
#[tauri::command]
pub(crate) async fn create_host(
    engine: State<'_, Engine>,
    draft: HostDraftDto,
) -> Result<String, ErrorSummary> {
    let identity = identity_for(&engine, &draft.identity, None).await?;
    let created = engine
        .create_host(draft.into_draft(identity))
        .await
        .map_err(summary)?;
    Ok(created.host.alias)
}

/// Replaces the Portico host `alias` (a rename included); returns the new alias.
#[tauri::command]
pub(crate) async fn update_host(
    engine: State<'_, Engine>,
    alias: String,
    draft: HostDraftDto,
) -> Result<String, ErrorSummary> {
    let identity = identity_for(&engine, &draft.identity, Some(&alias)).await?;
    let updated = engine
        .update_host(&alias, draft.into_draft(identity))
        .await
        .map_err(summary)?;
    Ok(updated.host.alias)
}

#[tauri::command]
pub(crate) async fn delete_host(
    engine: State<'_, Engine>,
    alias: String,
) -> Result<(), ErrorSummary> {
    engine.delete_host(&alias).await.map_err(summary)
}

/// Copies any host into a new Portico host `new_alias`; returns its alias.
#[tauri::command]
pub(crate) async fn duplicate_host(
    engine: State<'_, Engine>,
    alias: String,
    new_alias: String,
) -> Result<String, ErrorSummary> {
    let copy = engine
        .duplicate_as_managed(&alias, &new_alias)
        .await
        .map_err(summary)?;
    Ok(copy.host.alias)
}

/// The `Include` line that makes the Portico hosts visible to OpenSSH.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedIncludeDto {
    pub line: String,
    /// `~/.ssh/config` already has it.
    pub active: bool,
}

#[tauri::command]
pub(crate) async fn managed_include(
    engine: State<'_, Engine>,
) -> Result<ManagedIncludeDto, ErrorSummary> {
    Ok(ManagedIncludeDto {
        line: engine.managed_include_line(),
        active: engine.managed_include_active().await.map_err(summary)?,
    })
}

/// The values `hosts.conf` itself holds for a Portico host: the edit form
/// starts from them, never from the resolved config, where an earlier value
/// (an `Include` placed lower in `~/.ssh/config`) could hide one, and saving
/// would then erase it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManagedHostDto {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub proxy_jump: Option<String>,
    /// The key as a discovered key's id (`id_ed25519`), the form's choice.
    pub key_id: Option<String>,
    /// `~/…`, display only (the "(current)" choice when `key_id` is
    /// `None`); never sent back (see [`IdentityChoiceDto::Keep`]).
    pub identity_file: Option<String>,
}

impl ManagedHostDto {
    /// `keys`: a listing from the engine. The key maps to an id only when
    /// that id resolves back to the very same file.
    pub(crate) fn new(host: ManagedHost, keys: &[LocalKey]) -> Self {
        let key_id = host.identity_file.as_ref().and_then(|file| {
            let key = keys.iter().find(|k| &k.private_path == file)?;
            let id = key_id(key)?;
            (private_key_path(keys, &id).ok().as_ref() == Some(file)).then_some(id)
        });
        Self {
            alias: host.alias,
            hostname: host.hostname,
            user: host.user,
            port: host.port,
            proxy_jump: host.proxy_jump,
            key_id,
            identity_file: host.identity_file.as_deref().map(display_path),
        }
    }
}

/// The Portico host `alias` as `hosts.conf` holds it; `None` when it is
/// not a Portico host (any more).
#[tauri::command]
pub(crate) async fn managed_host(
    engine: State<'_, Engine>,
    alias: String,
) -> Result<Option<ManagedHostDto>, ErrorSummary> {
    managed_host_dto(&engine, &alias).await
}

async fn managed_host_dto(
    engine: &Engine,
    alias: &str,
) -> Result<Option<ManagedHostDto>, ErrorSummary> {
    let Some(host) = engine.managed_host(alias).await.map_err(summary)? else {
        return Ok(None);
    };
    // No key listing when there is no key (and a failed listing only
    // loses the preselection: the form offers "(current)").
    let keys = if host.identity_file.is_some() {
        engine.local_keys().await.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(Some(ManagedHostDto::new(host, &keys)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::path::Path;

    fn key(dir: &Path, name: &str, has_private: bool) -> LocalKey {
        LocalKey {
            private_path: dir.join(name),
            public_path: dir.join(format!("{name}.pub")),
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            comment: String::new(),
            has_private,
            certificate: None,
        }
    }

    fn draft(json: serde_json::Value) -> Result<HostDraftDto, serde_json::Error> {
        serde_json::from_value(json)
    }

    fn base() -> serde_json::Value {
        serde_json::json!({
            "alias": "web",
            "hostname": "10.0.0.1",
            "user": "deploy",
            "port": 2222,
            "identity": { "kind": "key", "id": "id_ed25519" },
            "proxyJump": null,
            "tags": ["prod"],
            "group": null,
            "favorite": true,
            "notes": null,
            "password": { "kind": "set", "value": "hunter2" }
        })
    }

    #[test]
    fn draft_password_is_a_redacted_secret() {
        let dto = draft(base()).unwrap();
        let printed = format!("{dto:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        let core = dto.into_draft(None);
        let PasswordChange::Set(secret) = &core.password else {
            panic!("expected a password");
        };
        assert_eq!(secret.expose_secret(), "hunter2");
        assert!(!format!("{core:?}").contains("hunter2"));
        assert_eq!(core.port, Some(2222));
        assert_eq!(core.tags, vec!["prod".to_owned()]);
    }

    #[test]
    fn draft_maps_blank_optionals_and_password_choices() {
        let mut json = base();
        json["user"] = serde_json::json!("  ");
        json["proxyJump"] = serde_json::json!("");
        json["password"] = serde_json::json!({ "kind": "forget" });
        let core = draft(json).unwrap().into_draft(None);
        assert_eq!(core.user, None);
        assert_eq!(core.proxy_jump, None);
        assert!(matches!(core.password, PasswordChange::Forget));
        let mut json = base();
        json["password"] = serde_json::json!({ "kind": "keep" });
        assert!(matches!(
            draft(json).unwrap().into_draft(None).password,
            PasswordChange::Keep
        ));
    }

    #[test]
    fn the_webview_cannot_send_a_path() {
        // No `identityFile` field, no `path` choice, no extra field in a choice.
        let mut json = base();
        json["identityFile"] = serde_json::json!("/etc/shadow");
        assert!(draft(json).is_err());
        let mut json = base();
        json["identity"] = serde_json::json!({ "kind": "path", "path": "/etc/shadow" });
        assert!(draft(json).is_err());
        let mut json = base();
        json["identity"] =
            serde_json::json!({ "kind": "key", "id": "id_ed25519", "path": "/etc/shadow" });
        assert!(draft(json).is_err());
    }

    #[test]
    fn key_choice_resolves_only_discovered_private_keys() {
        let dir = tempfile::tempdir().unwrap();
        let keys = vec![
            key(dir.path(), "id_ed25519", true),
            key(dir.path(), "github", false),
        ];
        let pick =
            |id: &str| resolve_identity(&IdentityChoiceDto::Key { id: id.into() }, &keys, None);
        assert_eq!(
            pick("id_ed25519").unwrap(),
            Some(dir.path().join("id_ed25519"))
        );
        // Ids are file names from the listing: a path, `..` or a stranger is unknown.
        for bad in [
            "/etc/shadow",
            "../id_ed25519",
            "~/.ssh/id_ed25519",
            "id_rsa",
            "",
        ] {
            assert_eq!(pick(bad).unwrap_err().code, "key_unknown", "{bad}");
        }
        // A public key without its private half cannot be an IdentityFile.
        assert_eq!(pick("github").unwrap_err().code, "key_public_only");
        assert_eq!(
            resolve_identity(&IdentityChoiceDto::None, &keys, Some("/x".into())).unwrap(),
            None
        );
        // Keep: the path read back from the inventory, never from the webview.
        assert_eq!(
            resolve_identity(
                &IdentityChoiceDto::Keep,
                &keys,
                Some(PathBuf::from("/home/u/k"))
            )
            .unwrap(),
            Some(PathBuf::from("/home/u/k"))
        );
    }

    fn engine_at(root: &Path) -> Engine {
        Engine::new(portico_core::EngineConfig {
            paths: portico_core::store::Paths::rooted(root),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: std::sync::Arc::new(portico_core::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: portico_core::engine::DEFAULT_CONNECT_TIMEOUT,
            keepalive: portico_core::engine::DEFAULT_KEEPALIVE,
        })
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// A real engine under a temporary root: the inventory shows the origin;
    /// the form's values are the host's own, its key mapped to a discovered
    /// key's id; "keep the key" reads it back from hosts.conf.
    #[test]
    fn managed_host_gives_own_values_and_the_key_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        std::fs::write(
            root.join(".ssh/config"),
            "Host legacy\n  HostName 10.0.0.9\n\nHost *\n  User other\n  IdentityFile ~/.ssh/other\n",
        )
        .unwrap();
        std::fs::write(root.join(".ssh/id_test"), "not a real key").unwrap();
        std::fs::write(
            root.join(".ssh/id_test.pub"),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFAFn0jtDPqkMZ2znTeNinwlBFedmDlUUWl2OGimwLau test\n",
        )
        .unwrap();
        block_on(async {
            let engine = engine_at(root);
            let mut json = base();
            json["user"] = serde_json::Value::Null;
            json["password"] = serde_json::json!({ "kind": "keep" });
            let dto = draft(json).unwrap();
            let created = engine
                .create_host(dto.into_draft(Some(root.join(".ssh/id_test"))))
                .await
                .unwrap();
            assert_eq!(created.host.alias, "web");

            let inventory = engine.load_hosts().await.unwrap();
            let web = inventory.hosts.iter().find(|h| h.alias == "web").unwrap();
            let shown = crate::dto::HostDto::from(web);
            assert_eq!(shown.origin, portico_core::hosts::HostOrigin::Portico);
            // Inherited from `Host *`: effective, but not the host's own.
            assert_eq!(shown.user.as_deref(), Some("other"));

            let own = managed_host_dto(&engine, "web").await.unwrap().unwrap();
            assert_eq!(own.user, None);
            assert_eq!(own.port, Some(2222));
            assert_eq!(own.hostname, "10.0.0.1");
            assert_eq!(own.proxy_jump, None);
            assert_eq!(own.key_id.as_deref(), Some("id_test"));
            assert!(own.identity_file.unwrap().ends_with(".ssh/id_test"));
            assert_eq!(
                identity_for(&engine, &IdentityChoiceDto::Keep, Some("web"))
                    .await
                    .unwrap(),
                Some(root.join(".ssh/id_test"))
            );
            let json =
                serde_json::to_value(managed_host_dto(&engine, "WEB").await.unwrap()).unwrap();
            assert_eq!(json["keyId"], "id_test", "alias matched without case");

            // Not a Portico host: nothing to prefill, nothing to keep.
            assert_eq!(managed_host_dto(&engine, "legacy").await.unwrap(), None);
            assert_eq!(
                identity_for(&engine, &IdentityChoiceDto::Keep, Some("legacy"))
                    .await
                    .unwrap(),
                None
            );
        });
    }

    /// The Include placed at the bottom of ~/.ssh/config, under a `Host
    /// web` block that sets `User early`: the resolved config says `early`,
    /// the form must show (and keep) the user hosts.conf holds.
    #[test]
    fn managed_host_is_not_hidden_by_an_earlier_value() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        let engine = engine_at(root);
        block_on(async {
            let mut json = base();
            json["identity"] = serde_json::json!({ "kind": "none" });
            json["password"] = serde_json::json!({ "kind": "keep" });
            engine
                .create_host(draft(json).unwrap().into_draft(None))
                .await
                .unwrap();
            let conf = engine.config().paths.managed_hosts();
            assert!(conf.exists(), "{}", conf.display());
            std::fs::write(
                root.join(".ssh/config"),
                format!("Host web\n  User early\n\nInclude \"{}\"\n", conf.display()),
            )
            .unwrap();

            let inventory = engine.load_hosts().await.unwrap();
            let web = inventory.hosts.iter().find(|h| h.alias == "web").unwrap();
            assert_eq!(
                web.resolved.user.as_deref(),
                Some("early"),
                "first value wins"
            );

            let own = managed_host_dto(&engine, "web").await.unwrap().unwrap();
            assert_eq!(
                own.user.as_deref(),
                Some("deploy"),
                "the value of hosts.conf"
            );
            assert_eq!(own.port, Some(2222));
        });
    }

    #[test]
    fn include_dto_is_camel_case() {
        let dto = ManagedIncludeDto {
            line: "Include ~/.config/portico/hosts.conf".into(),
            active: false,
        };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({ "line": "Include ~/.config/portico/hosts.conf", "active": false })
        );
    }
}
