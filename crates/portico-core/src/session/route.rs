//! `ProxyJump` route planning (OpenSSH semantics).
//!
//! `ProxyJump a,b` reaches `a` first (honouring `a`'s own `ProxyJump`, i.e.
//! recursively), then `b` through `a`, then the target through `b`. Later
//! hops' own `ProxyJump` is ignored, exactly like `ssh -J a,b`. `user@host:port`
//! in a jump spec overrides that hop's config.

use std::time::Duration;

use crate::engine::EngineConfig;
use crate::error::{Error, Result};
use crate::ssh_config::{JumpHost, ResolvedHost, SshConfig};

/// Upper bound on hops (including the target).
pub(crate) const MAX_HOPS: usize = 8;

/// One SSH connection to make, in order.
#[derive(Debug, Clone)]
pub(crate) struct Hop {
    pub(crate) alias: String,
    pub(crate) user: String,
    pub(crate) resolved: ResolvedHost,
}

/// Refuses hops Portico cannot reach faithfully (never silently go direct).
fn reachable(resolved: &ResolvedHost) -> Result<()> {
    if resolved.proxy_command.is_some() {
        return Err(Error::Unsupported(
            "ProxyCommand (not supported yet; use ProxyJump for this host)",
        ));
    }
    Ok(())
}

impl Hop {
    fn new(resolved: ResolvedHost, config: &EngineConfig) -> Self {
        Self {
            alias: resolved.alias.clone(),
            user: resolved
                .user
                .clone()
                .unwrap_or_else(|| config.local_user.clone()),
            resolved,
        }
    }

    pub(crate) fn describe(&self) -> String {
        format!(
            "{}@{}:{}",
            self.user, self.resolved.hostname, self.resolved.port
        )
    }

    pub(crate) fn connect_timeout(&self, default: Duration) -> Duration {
        self.resolved
            .connect_timeout
            .filter(|s| *s > 0)
            .map_or(default, |s| Duration::from_secs(u64::from(s)))
    }

    /// First value of a raw keyword.
    pub(crate) fn option_str(&self, key: &str) -> Option<&str> {
        self.resolved
            .options
            .get(key)
            .and_then(|v| v.first())
            .and_then(|(args, _)| args.first())
            .map(String::as_str)
    }

    /// First value of a raw keyword, parsed as `u32`.
    pub(crate) fn option_u32(&self, key: &str) -> Option<u32> {
        self.resolved
            .options
            .get(key)
            .and_then(|v| v.first())
            .and_then(|(args, _)| args.first())
            .and_then(|s| s.parse().ok())
    }
}

/// Computes the full chain of hops ending with `alias`.
pub(crate) fn plan(
    ssh: &SshConfig,
    config: &EngineConfig,
    alias: &str,
    user_override: Option<String>,
) -> Result<Vec<Hop>> {
    let target = ssh.resolve(alias, &config.resolve_context(user_override));
    reachable(&target)?;
    let mut visiting = vec![alias.to_owned()];
    let mut hops = jump_chain(ssh, config, &target.proxy_jump, &mut visiting)?;
    hops.push(Hop::new(target, config));
    if hops.len() > MAX_HOPS {
        return Err(Error::ProxyJumpTooDeep { max: MAX_HOPS });
    }
    Ok(hops)
}

fn jump_chain(
    ssh: &SshConfig,
    config: &EngineConfig,
    jumps: &[JumpHost],
    visiting: &mut Vec<String>,
) -> Result<Vec<Hop>> {
    let mut out = Vec::new();
    for (index, jump) in jumps.iter().enumerate() {
        if visiting.iter().any(|v| v == &jump.host) {
            let mut chain = visiting.clone();
            chain.push(jump.host.clone());
            return Err(Error::ProxyJumpCycle {
                chain: chain.join(" → "),
            });
        }
        if visiting.len() > MAX_HOPS {
            return Err(Error::ProxyJumpTooDeep { max: MAX_HOPS });
        }
        let mut resolved = ssh.resolve(&jump.host, &config.resolve_context(jump.user.clone()));
        if let Some(port) = jump.port {
            resolved.port = port;
        }
        reachable(&resolved)?;
        if index == 0 {
            visiting.push(jump.host.clone());
            let inner = jump_chain(ssh, config, &resolved.proxy_jump, visiting)?;
            visiting.pop();
            out.extend(inner);
        }
        out.push(Hop::new(resolved, config));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::secrets::MemoryStore;
    use crate::store::Paths;

    fn config() -> EngineConfig {
        EngineConfig {
            paths: Paths::rooted(Path::new("/home/alice")),
            local_user: "alice".into(),
            event_capacity: 16,
            secrets: Arc::new(MemoryStore::default()),
            agent_socket: None,
            connect_timeout: Duration::from_secs(1),
            keepalive: Duration::from_secs(1),
        }
    }

    fn route(cfg: &str, alias: &str) -> Result<Vec<String>> {
        let ssh = SshConfig::parse_str(cfg, Path::new("cfg"), Path::new("/home/alice/.ssh"))
            .expect("valid config");
        Ok(plan(&ssh, &config(), alias, None)?
            .iter()
            .map(Hop::describe)
            .collect())
    }

    #[test]
    fn direct() {
        assert_eq!(route("", "example.com").unwrap(), ["alice@example.com:22"]);
    }

    #[test]
    fn nested_and_listed_jumps() {
        let cfg = "\
Host outer\n  HostName outer.net\n  User ops\n\
Host inner\n  HostName inner.lan\n  ProxyJump outer\n\
Host second\n  HostName second.lan\n  ProxyJump ignored\n\
Host target\n  HostName 10.0.0.9\n  ProxyJump inner,root@second:2200\n";
        assert_eq!(
            route(cfg, "target").unwrap(),
            [
                "ops@outer.net:22",
                "alice@inner.lan:22",
                "root@second.lan:2200",
                "alice@10.0.0.9:22"
            ]
        );
    }

    #[test]
    fn proxy_command_is_refused_not_bypassed() {
        let err = route("Host t\n  ProxyCommand nc %h %p\n", "t").unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
        let via = "Host j\n  ProxyCommand nc %h %p\nHost t\n  ProxyJump j\n";
        assert!(matches!(route(via, "t"), Err(Error::Unsupported(_))));
    }

    #[test]
    fn cycles_are_detected() {
        let cfg = "Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n";
        let err = route(cfg, "a").unwrap_err();
        assert!(matches!(err, Error::ProxyJumpCycle { .. }), "{err}");
        let self_jump = "Host s\n  ProxyJump s\n";
        assert!(matches!(
            route(self_jump, "s"),
            Err(Error::ProxyJumpCycle { .. })
        ));
    }
}
