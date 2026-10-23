//! "Effective config" view (Phase 4): the `ssh -G <alias>` equivalent for
//! the debug panel.
//!
//! Lists every option that applies to an alias after OpenSSH resolution
//! (first obtained value wins, multi-valued keywords accumulate), each with
//! its origin (`file:line`), then the values Portico applies by default when
//! the config is silent (origin `default`), and the resolved `ProxyJump`
//! route, hop by hop.
//!
//! Order: `host`, `hostname`, `user`, `port` first (like `ssh -G`), then the
//! other configured keywords in alphabetical order (lowercase, one line per
//! value of a multi-valued keyword, in config order), then the defaults.
//! Values are shown as written in the config, except `hostname`, `user` and
//! `port` which are the resolved ones (`%h` expanded, `-l` override
//! applied).
//!
//! ## Sensitive values
//!
//! `ssh_config` is not meant to hold secrets, but nothing stops a user from
//! putting one in an environment value or a command line. So:
//!
//! - `SetEnv NAME=value` is **masked here**, in the core: each value is
//!   shown as `NAME=•••` (the name stays, for debugging). The real value
//!   never leaves this module;
//! - commands that run locally or remotely ([`SENSITIVE_KEYWORDS`]:
//!   `ProxyCommand`, `LocalCommand`, `RemoteCommand`, `KnownHostsCommand`)
//!   are kept as written, because seeing them is the point of the view, but
//!   flagged [`EffectiveOption::sensitive`] so the UIs hide them until the
//!   user reveals them, and never copy them into logs. `PermitLocalCommand`
//!   is a yes/no switch, not sensitive.

use serde::Serialize;

use crate::engine::{DEFAULT_KEEPALIVE_MAX, EngineConfig};
use crate::error::ErrorSummary;
use crate::session::route;
use crate::ssh_config::{ResolvedHost, Source, SshConfig};

/// The effective configuration of one alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveConfig {
    pub alias: String,
    pub options: Vec<EffectiveOption>,
    /// Hops in connection order, the target last (empty when `route_error`).
    pub route: Vec<RouteHop>,
    /// Why the route cannot be used (`ProxyCommand`, cycle, too deep…).
    pub route_error: Option<ErrorSummary>,
    /// Non-fatal config issues (unsupported `Match`, invalid values…).
    pub warnings: Vec<EffectiveWarning>,
}

/// One `keyword value…` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveOption {
    /// Lowercase keyword, as `ssh -G` prints it.
    pub key: String,
    pub values: Vec<String>,
    pub origin: OptionOrigin,
    /// The values may hold a secret (a command line, see
    /// [`SENSITIVE_KEYWORDS`]): UIs mask them until the user asks.
    pub sensitive: bool,
}

/// Keywords whose values are commands that may embed tokens or passwords:
/// shown on demand only (see the module doc).
pub const SENSITIVE_KEYWORDS: [&str; 4] = [
    "proxycommand",
    "localcommand",
    "remotecommand",
    "knownhostscommand",
];

/// Shown instead of a `SetEnv` value.
const MASKED_VALUE: &str = "•••";

/// Where a value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OptionOrigin {
    /// A config file line.
    Config { file: String, line: usize },
    /// Portico's default (the config does not set it).
    Default,
    /// Given at connect time (alias typed, user override).
    Command,
}

/// One hop of the resolved route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteHop {
    pub alias: String,
    pub user: String,
    pub hostname: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveWarning {
    pub file: String,
    pub line: usize,
    pub message: String,
}

fn origin(source: &Source) -> OptionOrigin {
    OptionOrigin::Config {
        file: source.file.display().to_string(),
        line: source.line,
    }
}

fn option(key: &str, values: Vec<String>, origin: OptionOrigin) -> EffectiveOption {
    let values = if key == "setenv" {
        values.iter().map(|v| mask_setenv(v)).collect()
    } else {
        values
    };
    EffectiveOption {
        key: key.to_owned(),
        values,
        origin,
        sensitive: SENSITIVE_KEYWORDS.contains(&key),
    }
}

/// `NAME=value` becomes `NAME=•••`; an argument without `=` (invalid for
/// OpenSSH) is masked whole.
fn mask_setenv(arg: &str) -> String {
    match arg.split_once('=') {
        Some((name, _)) => format!("{name}={MASKED_VALUE}"),
        None => MASKED_VALUE.to_owned(),
    }
}

/// Computes the view (pure: the caller loads the config).
pub(crate) fn compute(
    ssh: &SshConfig,
    config: &EngineConfig,
    alias: &str,
    user_override: Option<String>,
) -> EffectiveConfig {
    let ctx = config.resolve_context(user_override.clone());
    let host = ssh.resolve(alias, &ctx);
    let first_source = |key: &str| {
        host.options
            .get(key)
            .and_then(|v| v.first())
            .map(|(_, source)| origin(source))
    };

    let mut options = vec![option(
        "host",
        vec![alias.to_owned()],
        OptionOrigin::Command,
    )];
    options.push(option(
        "hostname",
        vec![host.hostname.clone()],
        first_source("hostname").unwrap_or(OptionOrigin::Default),
    ));
    let user_origin = if user_override.is_some() {
        OptionOrigin::Command
    } else {
        first_source("user").unwrap_or(OptionOrigin::Default)
    };
    options.push(option(
        "user",
        vec![host.effective_user(&ctx).to_owned()],
        user_origin,
    ));
    // An invalid `Port` falls back to 22 (and is reported as a warning).
    let configured_port = host
        .options
        .get("port")
        .and_then(|v| v.first())
        .filter(|(args, _)| args.first().and_then(|p| p.parse::<u16>().ok()) == Some(host.port));
    options.push(option(
        "port",
        vec![host.port.to_string()],
        configured_port.map_or(OptionOrigin::Default, |(_, source)| origin(source)),
    ));

    for (key, values) in &host.options {
        if matches!(key.as_str(), "hostname" | "user" | "port") {
            continue;
        }
        for (args, source) in values {
            options.push(option(key, args.clone(), origin(source)));
        }
    }
    options.extend(defaults(&host, config));

    let (route, route_error) = match route::plan(ssh, config, alias, user_override) {
        Ok(hops) => (
            hops.iter()
                .map(|hop| RouteHop {
                    alias: hop.alias.clone(),
                    user: hop.user.clone(),
                    hostname: hop.resolved.hostname.clone(),
                    port: hop.resolved.port,
                })
                .collect(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e.summary())),
    };

    let warnings = ssh
        .warnings()
        .iter()
        .chain(&host.warnings)
        .map(|w| EffectiveWarning {
            file: w.source.file.display().to_string(),
            line: w.source.line,
            message: w.message.clone(),
        })
        .collect();

    EffectiveConfig {
        alias: alias.to_owned(),
        options,
        route,
        route_error,
        warnings,
    }
}

/// What Portico uses when the config is silent (the session code applies
/// exactly these: see `session::auth`, `session::handler`, `session::run`).
fn defaults(host: &ResolvedHost, config: &EngineConfig) -> Vec<EffectiveOption> {
    let paths = &config.paths;
    let set = |key: &str| host.options.contains_key(key);
    let path = |p: &std::path::Path| p.display().to_string();
    let mut out = Vec::new();
    let mut default = |key: &str, values: Vec<String>| {
        if !set(key) {
            out.push(option(key, values, OptionOrigin::Default));
        }
    };
    default(
        "connecttimeout",
        vec![config.connect_timeout.as_secs().to_string()],
    );
    default(
        "globalknownhostsfile",
        paths.global_known_hosts.iter().map(|p| path(p)).collect(),
    );
    default(
        "identityfile",
        paths.default_identities().iter().map(|p| path(p)).collect(),
    );
    default("identitiesonly", vec!["no".into()]);
    default(
        "serveralivecountmax",
        vec![DEFAULT_KEEPALIVE_MAX.to_string()],
    );
    default(
        "serveraliveinterval",
        vec![config.keepalive.as_secs().to_string()],
    );
    default("stricthostkeychecking", vec!["ask".into()]);
    default(
        "userknownhostsfile",
        vec![
            path(&paths.known_hosts()),
            path(&paths.ssh_dir.join("known_hosts2")),
        ],
    );
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::store::Paths;

    fn config(root: &Path) -> EngineConfig {
        EngineConfig {
            paths: Paths::rooted(root),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: Arc::new(crate::secrets::MemoryStore::default()),
            agent_socket: None,
            connect_timeout: Duration::from_secs(15),
            keepalive: Duration::from_secs(15),
            update_fetcher: crate::update::no_network(),
        }
    }

    fn line(view: &EffectiveConfig, key: &str) -> Vec<(Vec<String>, OptionOrigin)> {
        view.options
            .iter()
            .filter(|o| o.key == key)
            .map(|o| (o.values.clone(), o.origin.clone()))
            .collect()
    }

    fn at(file: &Path, line: usize) -> OptionOrigin {
        OptionOrigin::Config {
            file: file.display().to_string(),
            line,
        }
    }

    #[test]
    fn options_carry_their_origin_across_includes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let ssh = root.join(".ssh");
        std::fs::create_dir_all(&ssh).expect("mkdir");
        let main = ssh.join("config");
        let extra = ssh.join("extra.conf");
        std::fs::write(
            &main,
            "Include extra.conf\nHost web\n  HostName %h.internal\n  ProxyJump bastion\n  IdentityFile ~/.ssh/id_web\nHost *\n  IdentityFile ~/.ssh/id_all\n  User fallback\n",
        )
        .expect("write");
        std::fs::write(
            &extra,
            "Host bastion\n  HostName b.example.com\n  Port 2222\n  User ops\nHost web\n  Port nope\n  ServerAliveInterval 30\n",
        )
        .expect("write");
        let parsed = SshConfig::load(&main, &ssh).expect("load");
        let view = compute(&parsed, &config(root), "web", None);

        let keys: Vec<&str> = view.options.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            keys.get(..4),
            Some(&["host", "hostname", "user", "port"][..])
        );
        assert_eq!(
            line(&view, "hostname"),
            [(vec!["web.internal".to_owned()], at(&main, 3))]
        );
        assert_eq!(
            line(&view, "user"),
            [(vec!["fallback".to_owned()], at(&main, 8))]
        );
        // Invalid port: 22 by default, with a warning naming the line.
        assert_eq!(
            line(&view, "port"),
            [(vec!["22".to_owned()], OptionOrigin::Default)]
        );
        assert!(
            view.warnings
                .iter()
                .any(|w| w.line == 6 && w.file.ends_with("extra.conf")),
            "{:?}",
            view.warnings
        );
        assert_eq!(
            line(&view, "identityfile"),
            [
                (vec!["~/.ssh/id_web".to_owned()], at(&main, 5)),
                (vec!["~/.ssh/id_all".to_owned()], at(&main, 7)),
            ]
        );
        assert_eq!(
            line(&view, "serveraliveinterval"),
            [(vec!["30".to_owned()], at(&extra, 7))]
        );
        assert_eq!(
            line(&view, "stricthostkeychecking"),
            [(vec!["ask".to_owned()], OptionOrigin::Default)]
        );
        assert_eq!(
            line(&view, "connecttimeout"),
            [(vec!["15".to_owned()], OptionOrigin::Default)]
        );
        assert_eq!(line(&view, "identitiesonly").len(), 1);
        assert_eq!(
            view.route,
            [
                RouteHop {
                    alias: "bastion".into(),
                    user: "ops".into(),
                    hostname: "b.example.com".into(),
                    port: 2222
                },
                RouteHop {
                    alias: "web".into(),
                    user: "fallback".into(),
                    hostname: "web.internal".into(),
                    port: 22
                },
            ]
        );
        assert_eq!(view.route_error, None);
        let json = serde_json::to_string(&view).expect("json");
        assert!(json.contains(r#""origin":{"kind":"default"}"#), "{json}");
    }

    #[test]
    fn defaults_and_overrides() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = config(dir.path());
        let parsed = SshConfig::parse_str(
            "Host p\n  Port 2200\n  User bob\n",
            Path::new("cfg"),
            Path::new("/x"),
        )
        .expect("parse");
        let view = compute(&parsed, &cfg, "p", Some("root".into()));
        assert_eq!(
            line(&view, "user"),
            [(vec!["root".to_owned()], OptionOrigin::Command)]
        );
        assert_eq!(
            line(&view, "port"),
            [(vec!["2200".to_owned()], at(Path::new("cfg"), 2))]
        );
        assert_eq!(
            line(&view, "hostname"),
            [(vec!["p".to_owned()], OptionOrigin::Default)]
        );
        let ids = line(&view, "identityfile");
        assert_eq!(ids.len(), 1);
        assert_eq!(ids.first().map(|(v, _)| v.len()), Some(3));

        let unknown = compute(&parsed, &cfg, "other.example", None);
        assert_eq!(
            line(&unknown, "user"),
            [(vec!["alice".to_owned()], OptionOrigin::Default)]
        );
    }

    #[test]
    fn unusable_routes_are_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parsed = SshConfig::parse_str(
            "Host a\n  ProxyJump b\nHost b\n  ProxyJump a\nHost c\n  ProxyCommand nc %h %p\n",
            Path::new("cfg"),
            Path::new("/x"),
        )
        .expect("parse");
        let view = compute(&parsed, &config(dir.path()), "a", None);
        assert!(view.route.is_empty());
        assert_eq!(
            view.route_error.as_ref().map(|e| e.code.as_str()),
            Some("proxy_jump_cycle")
        );
        let view = compute(&parsed, &config(dir.path()), "c", None);
        assert_eq!(
            view.route_error.as_ref().map(|e| e.code.as_str()),
            Some("unsupported")
        );
        assert_eq!(
            line(&view, "proxycommand"),
            [(
                vec!["nc".to_owned(), "%h".to_owned(), "%p".to_owned()],
                at(Path::new("cfg"), 6)
            )]
        );
    }

    #[test]
    fn setenv_values_are_masked() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parsed = SshConfig::parse_str(
            "Host s\n  SetEnv API_TOKEN=hunter2-secret LANG=\"fr hunter2-secret\"\n  SetEnv OTHER=hunter2-secret\n",
            Path::new("cfg"),
            Path::new("/x"),
        )
        .expect("parse");
        let view = compute(&parsed, &config(dir.path()), "s", None);
        let setenv = line(&view, "setenv");
        let values: Vec<&String> = setenv.iter().flat_map(|(v, _)| v).collect();
        assert!(!values.is_empty(), "{setenv:?}");
        assert!(values.iter().all(|v| v.ends_with("=•••")), "{values:?}");
        assert!(
            values.iter().any(|v| v.as_str() == "API_TOKEN=•••"),
            "{values:?}"
        );
        let json = serde_json::to_string(&view).expect("json");
        assert!(!json.contains("hunter2"), "{json}");
        assert_eq!(mask_setenv("novalue"), "•••");
    }

    #[test]
    fn commands_are_flagged_sensitive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parsed = SshConfig::parse_str(
            "Host s\n  ProxyCommand nc %h %p\n  LocalCommand echo tok\n  RemoteCommand run --token x\n  KnownHostsCommand /bin/kh %H\n  PermitLocalCommand yes\n  User bob\n",
            Path::new("cfg"),
            Path::new("/x"),
        )
        .expect("parse");
        let view = compute(&parsed, &config(dir.path()), "s", None);
        let sensitive = |key: &str| {
            view.options
                .iter()
                .find(|o| o.key == key)
                .map(|o| o.sensitive)
        };
        for key in SENSITIVE_KEYWORDS {
            assert_eq!(sensitive(key), Some(true), "{key}");
        }
        for key in ["permitlocalcommand", "user", "host", "identityfile"] {
            assert_eq!(sensitive(key), Some(false), "{key}");
        }
        // Commands stay readable on demand.
        assert_eq!(
            line(&view, "remotecommand")
                .first()
                .map(|(v, _)| v.join(" ")),
            Some("run --token x".to_owned())
        );
        let json = serde_json::to_string(&view).expect("json");
        assert!(json.contains(r#""sensitive":true"#), "{json}");
    }
}
