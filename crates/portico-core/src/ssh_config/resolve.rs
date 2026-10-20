//! Effective-option computation for one host (`ssh -G` equivalent).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::pattern::match_pattern_list;
use super::{Condition, ConfigWarning, MULTI_VALUED, MatchCriterion, Source, SshConfig};

/// Local facts needed to evaluate `Match` and expand `%` tokens.
#[derive(Debug, Clone)]
pub struct ResolveContext {
    /// Local user name (`%u`, `Match localuser`).
    pub local_user: String,
    /// Local home directory (`%d`, `~`).
    pub home: PathBuf,
    /// `-l user` / `user@` override given at connect time, if any.
    pub user_override: Option<String>,
}

/// `StrictHostKeyChecking` policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrictHostKeyChecking {
    /// Ask the user for unknown keys (OpenSSH default). Changed keys always fail.
    #[default]
    Ask,
    /// Refuse unknown keys.
    Yes,
    /// Accept and record unknown keys; changed keys still fail.
    AcceptNew,
    /// Treated like `AcceptNew` (unknown keys are accepted and recorded).
    /// Unlike OpenSSH, Portico still refuses changed keys.
    No,
}

/// One hop of a `ProxyJump` chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpHost {
    pub user: Option<String>,
    /// Alias or host name; aliases are resolved recursively by the session layer.
    pub host: String,
    pub port: Option<u16>,
}

/// The effective configuration of a host after applying every matching block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHost {
    /// Name as typed by the user (`%n`).
    pub alias: String,
    /// Real target (`HostName`, `%h`), defaults to the alias.
    pub hostname: String,
    pub port: u16,
    /// `None` means "use the local user name".
    pub user: Option<String>,
    pub identity_files: Vec<PathBuf>,
    pub certificate_files: Vec<PathBuf>,
    pub identities_only: bool,
    pub identity_agent: Option<String>,
    pub proxy_jump: Vec<JumpHost>,
    pub proxy_command: Option<String>,
    pub forward_agent: bool,
    pub local_forwards: Vec<String>,
    pub remote_forwards: Vec<String>,
    pub dynamic_forwards: Vec<String>,
    pub user_known_hosts_files: Vec<PathBuf>,
    pub strict_host_key_checking: StrictHostKeyChecking,
    pub server_alive_interval: Option<u32>,
    pub connect_timeout: Option<u32>,
    /// Every effective keyword (lowercase) with its values and origin, for the
    /// "effective config" debug view.
    pub options: BTreeMap<String, Vec<(Vec<String>, Source)>>,
    pub warnings: Vec<ConfigWarning>,
}

impl ResolvedHost {
    /// User name to log in with.
    pub fn effective_user<'a>(&'a self, ctx: &'a ResolveContext) -> &'a str {
        self.user.as_deref().unwrap_or(&ctx.local_user)
    }

    fn first(&self, key: &str) -> Option<&str> {
        self.options
            .get(key)
            .and_then(|v| v.first())
            .and_then(|(args, _)| args.first())
            .map(String::as_str)
    }
}

impl SshConfig {
    /// Computes the effective configuration for `alias`.
    #[allow(clippy::too_many_lines)] // linear keyword mapping; splitting hurts readability
    pub fn resolve(&self, alias: &str, ctx: &ResolveContext) -> ResolvedHost {
        let mut options: BTreeMap<String, Vec<(Vec<String>, Source)>> = BTreeMap::new();
        let mut warnings = Vec::new();

        for block in &self.blocks {
            // Re-evaluated per block: `Match host` sees the HostName set so far.
            let hostname = current_hostname(&options, alias);
            let user = ctx
                .user_override
                .clone()
                .or_else(|| first_arg(&options, "user").map(str::to_owned))
                .unwrap_or_else(|| ctx.local_user.clone());

            let applies = block.conditions.iter().all(|c| match c {
                Condition::Host(patterns) => match_pattern_list(patterns, alias),
                Condition::Match(criteria) => criteria.iter().all(|m| match m {
                    MatchCriterion::All => true,
                    MatchCriterion::Host(p, neg) => match_pattern_list(p, &hostname) != *neg,
                    MatchCriterion::OriginalHost(p, neg) => match_pattern_list(p, alias) != *neg,
                    MatchCriterion::User(p, neg) => match_pattern_list(p, &user) != *neg,
                    MatchCriterion::LocalUser(p, neg) => {
                        match_pattern_list(p, &ctx.local_user) != *neg
                    }
                    MatchCriterion::Unsupported(_) => false,
                }),
            });
            if !applies {
                continue;
            }

            for d in block.directives.iter().filter(|d| !d.key.starts_with('#')) {
                let entry = options.entry(d.key.clone()).or_default();
                if entry.is_empty() || MULTI_VALUED.contains(&d.key.as_str()) {
                    entry.push((d.args.clone(), d.source.clone()));
                }
            }
        }

        let mut host = ResolvedHost {
            alias: alias.to_owned(),
            hostname: String::new(),
            port: 22,
            user: ctx.user_override.clone(),
            identity_files: Vec::new(),
            certificate_files: Vec::new(),
            identities_only: false,
            identity_agent: None,
            proxy_jump: Vec::new(),
            proxy_command: None,
            forward_agent: false,
            local_forwards: Vec::new(),
            remote_forwards: Vec::new(),
            dynamic_forwards: Vec::new(),
            user_known_hosts_files: Vec::new(),
            strict_host_key_checking: StrictHostKeyChecking::default(),
            server_alive_interval: None,
            connect_timeout: None,
            options,
            warnings: Vec::new(),
        };

        host.hostname = host.first("hostname").map_or_else(
            || alias.to_owned(),
            |h| h.replace("%h", alias).replace("%%", "%"),
        );
        if let Some(p) = host.first("port") {
            match p.parse::<u16>() {
                Ok(port) if port != 0 => host.port = port,
                _ => warnings.push(warn(&host, "port", format!("invalid Port `{p}`"))),
            }
        }
        if host.user.is_none() {
            host.user = host.first("user").map(str::to_owned);
        }

        let tokens = Tokens::new(&host, ctx);
        host.identity_files = path_values(&host, "identityfile", &tokens, ctx);
        host.certificate_files = path_values(&host, "certificatefile", &tokens, ctx);
        host.user_known_hosts_files = host
            .options
            .get("userknownhostsfile")
            .and_then(|v| v.first())
            .map(|(args, _)| args.iter().map(|a| expand_path(a, &tokens, ctx)).collect())
            .unwrap_or_default();
        host.identities_only = host.first("identitiesonly").is_some_and(is_yes);
        host.forward_agent = host.first("forwardagent").is_some_and(is_yes);
        host.identity_agent = host.first("identityagent").map(|s| tokens.expand(s));
        host.proxy_command = host
            .first("proxycommand")
            .filter(|s| !s.eq_ignore_ascii_case("none"))
            .map(|_| {
                // ProxyCommand is the whole rest of the line.
                let args = host
                    .options
                    .get("proxycommand")
                    .and_then(|v| v.first())
                    .map(|(a, _)| a.join(" "))
                    .unwrap_or_default();
                tokens.expand(&args)
            });
        if let Some(raw) = host.first("proxyjump") {
            match parse_proxy_jump(raw) {
                Ok(hops) => host.proxy_jump = hops,
                Err(msg) => warnings.push(warn(&host, "proxyjump", msg)),
            }
        }
        host.local_forwards = joined_values(&host, "localforward");
        host.remote_forwards = joined_values(&host, "remoteforward");
        host.dynamic_forwards = joined_values(&host, "dynamicforward");
        host.strict_host_key_checking = match host.first("stricthostkeychecking") {
            Some(v) if is_yes(v) => StrictHostKeyChecking::Yes,
            Some(v) if v.eq_ignore_ascii_case("accept-new") => StrictHostKeyChecking::AcceptNew,
            Some(v) if v.eq_ignore_ascii_case("no") || v.eq_ignore_ascii_case("off") => {
                StrictHostKeyChecking::No
            }
            _ => StrictHostKeyChecking::Ask,
        };
        host.server_alive_interval = host
            .first("serveraliveinterval")
            .and_then(|v| v.parse().ok());
        host.connect_timeout = host.first("connecttimeout").and_then(|v| v.parse().ok());
        host.warnings = warnings;
        host
    }
}

fn current_hostname(options: &BTreeMap<String, Vec<(Vec<String>, Source)>>, alias: &str) -> String {
    first_arg(options, "hostname").map_or_else(|| alias.to_owned(), |h| h.replace("%h", alias))
}

fn first_arg<'a>(
    options: &'a BTreeMap<String, Vec<(Vec<String>, Source)>>,
    key: &str,
) -> Option<&'a str> {
    options
        .get(key)
        .and_then(|v| v.first())
        .and_then(|(a, _)| a.first())
        .map(String::as_str)
}

fn is_yes(v: &str) -> bool {
    v.eq_ignore_ascii_case("yes") || v.eq_ignore_ascii_case("true")
}

fn warn(host: &ResolvedHost, key: &str, message: String) -> ConfigWarning {
    let source = host.options.get(key).and_then(|v| v.first()).map_or_else(
        || Source {
            file: PathBuf::new(),
            line: 0,
        },
        |(_, s)| s.clone(),
    );
    ConfigWarning { source, message }
}

fn joined_values(host: &ResolvedHost, key: &str) -> Vec<String> {
    host.options
        .get(key)
        .map(|v| v.iter().map(|(args, _)| args.join(" ")).collect())
        .unwrap_or_default()
}

fn path_values(
    host: &ResolvedHost,
    key: &str,
    tokens: &Tokens,
    ctx: &ResolveContext,
) -> Vec<PathBuf> {
    host.options
        .get(key)
        .map(|v| {
            v.iter()
                .filter_map(|(args, _)| args.first())
                .filter(|a| !a.eq_ignore_ascii_case("none"))
                .map(|a| expand_path(a, tokens, ctx))
                .collect()
        })
        .unwrap_or_default()
}

fn expand_path(raw: &str, tokens: &Tokens, ctx: &ResolveContext) -> PathBuf {
    expand_tilde(&tokens.expand(raw), Some(&ctx.home))
}

/// Expands a leading `~` or `~/` with `home`. `~otheruser` is left untouched.
pub(crate) fn expand_tilde(raw: &str, home: Option<&Path>) -> PathBuf {
    match (raw, home) {
        ("~", Some(h)) => h.to_path_buf(),
        (r, Some(h)) if r.starts_with("~/") => h.join(r.get(2..).unwrap_or_default()),
        (r, _) => PathBuf::from(r),
    }
}

/// `%`-token expansion (subset of OpenSSH TOKENS).
struct Tokens {
    host: String,
    alias: String,
    port: String,
    remote_user: String,
    local_user: String,
    home: String,
}

impl Tokens {
    fn new(host: &ResolvedHost, ctx: &ResolveContext) -> Self {
        Self {
            host: host.hostname.clone(),
            alias: host.alias.clone(),
            port: host.port.to_string(),
            remote_user: host.effective_user(ctx).to_owned(),
            local_user: ctx.local_user.clone(),
            home: ctx.home.to_string_lossy().into_owned(),
        }
    }

    fn expand(&self, raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('%') | None => out.push('%'),
                Some('h') => out.push_str(&self.host),
                Some('n') => out.push_str(&self.alias),
                Some('p') => out.push_str(&self.port),
                Some('r') => out.push_str(&self.remote_user),
                Some('u') => out.push_str(&self.local_user),
                Some('d') => out.push_str(&self.home),
                // Unknown token: keep verbatim so the user can spot it.
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
            }
        }
        out
    }
}

/// Parses `none` or `[user@]host[:port]`, `ssh://[user@]host[:port]`, comma-separated.
pub(crate) fn parse_proxy_jump(raw: &str) -> Result<Vec<JumpHost>, String> {
    if raw.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|hop| {
            let hop = hop.trim();
            let hop = hop.strip_prefix("ssh://").unwrap_or(hop);
            let (user, rest) = match hop.rsplit_once('@') {
                Some((u, r)) => (Some(u.to_owned()), r),
                None => (None, hop),
            };
            let (host, port) = split_host_port(rest)?;
            if host.is_empty() {
                return Err(format!("empty host in ProxyJump `{raw}`"));
            }
            Ok(JumpHost { user, host, port })
        })
        .collect()
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>), String> {
    // [v6::addr]:port
    if let Some(rest) = s.strip_prefix('[') {
        let (addr, tail) = rest
            .split_once(']')
            .ok_or_else(|| format!("unterminated IPv6 literal in `{s}`"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(parse_port(p)?),
            None => None,
        };
        return Ok((addr.to_owned(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Ok((h.to_owned(), Some(parse_port(p)?))),
        _ => Ok((s.to_owned(), None)),
    }
}

fn parse_port(p: &str) -> Result<u16, String> {
    match p.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(format!("invalid port `{p}`")),
    }
}
