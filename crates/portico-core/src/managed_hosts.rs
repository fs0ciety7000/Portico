//! Hosts created in Portico (phase 6b, ADR "Hôtes créés dans Portico").
//!
//! Portico never writes `~/.ssh/config`. The hosts created, edited or
//! deleted from the UIs live in **Portico's own `ssh_config` fragment**,
//! `<config_dir>/hosts.conf` ([`crate::store::Paths::managed_hosts`]).
//!
//! ## The file
//!
//! - Written atomically (`0600`, missing directories `0700`), never through a
//!   symlink (the write is refused), under an exclusive lock
//!   (`hosts.conf.lock`) held for the whole read-check-write cycle.
//! - Rendered deterministically by [`render`]: a header comment (with the
//!   `Include` line to add to `~/.ssh/config`), then one `Host` block per
//!   host, in file order, with only `HostName`, `User`, `Port`,
//!   `IdentityFile` (`~/…` under the home directory, double-quoted when it
//!   holds a space), `IdentitiesOnly yes` (with a key) and `ProxyJump`.
//! - Every value is checked against the same allowlists as the Termius
//!   import ([`crate::import`]) before it reaches the file: no space, line
//!   break, `#`, quote, `=` or `%` token where it could start a new
//!   directive or be expanded, no leading `-`.
//!
//! ## Hand edits
//!
//! A hand-edited file is still **read** by the inventory and by sessions,
//! with the lenient OpenSSH parser (exactly what `ssh` would do). It is
//! only **rewritten** when Portico can read every line back without losing
//! anything ([`parse_file`]): blank lines, the header, a `Host` with one
//! plain alias and the managed directives with values the allowlists
//! accept (keyword case, `=` and indentation are normalized, nothing else
//! changes). Anything else (another directive, `Match`, `Include`, a
//! comment of the user's, a repeated directive, a value Portico would not
//! write, a line that no longer parses) makes every edit fail with a typed
//! error naming the line ([`ManagedHostError::FileUnmanaged`],
//! [`ManagedHostError::FileInvalid`]), and the file is left untouched.
//! Unknown lines are never dropped nor kept silently.
//!
//! ## Loading
//!
//! [`load_config`] is the configuration every part of the engine reads: the
//! user's `~/.ssh/config`, plus `hosts.conf` read **first**, as if
//! `Include <hosts.conf>` were its first line (where the proposed line goes),
//! so first-value-wins is the same as for OpenSSH once the line is added.
//! When an `Include` of the user's already resolves to the file, it is read
//! there and only there (never twice).
//!
//! ## Passwords
//!
//! A password typed with a host is stored through [`SecretStore`], never in
//! the file. Before the first connection the server key is unknown, so it is
//! stored as [`SecretKey::UnboundHostPassword`] (user, `known_hosts` name,
//! port): sessions offer it only after the host key passed verification, and
//! the first login it opens binds it to that key
//! ([`SecretKey::HostPassword`], the usual remember-password scheme), unless
//! the key was accepted automatically (`StrictHostKeyChecking accept-new`):
//! it is then withheld. Deleting a host forgets both forms, unless another
//! host still reaches the same account: the bound ones are found through
//! `known_hosts` **and** the non-secret index of bound fingerprints in
//! `hosts.toml` (a password bound through a host certificate, or to a key
//! whose `known_hosts` line is gone). An edit that cannot write the file
//! restores the password it replaced.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use secrecy::SecretString;
use serde::Serialize;

use crate::engine::EngineConfig;
use crate::error::{Error, Result};
use crate::hosts::{HostEntry, HostMeta, HostOrigin, MetaChange};
use crate::import::{
    ImportSource, clean_hostname, clean_user, display_text, include_line, is_valid_alias,
};
use crate::known_hosts::KnownHosts;
use crate::secrets::{SecretKey, SecretStore};
use crate::ssh_config::lexer::tokenize;
use crate::ssh_config::{ConfigWarning, HostAlias, ResolveContext, Source, SshConfig};
use crate::store::{self, Paths, same_file};

/// Largest `hosts.conf` read or written, in bytes.
pub const MAX_MANAGED_FILE_BYTES: u64 = 1024 * 1024;
/// Longest `IdentityFile` path accepted, in bytes.
pub const MAX_IDENTITY_PATH_BYTES: usize = 1024;
/// Longest `ProxyJump` alias accepted (aliases of the user's config may be
/// longer than the ones Portico creates).
const MAX_JUMP_ALIAS_CHARS: usize = 255;
/// Deepest `ProxyJump` chain walked when looking for a cycle.
const MAX_JUMP_DEPTH: usize = 32;

/// Header lines written at the top of the file (the `Include` line follows).
const HEADER_LINES: [&str; 3] = [
    "# Hosts created in Portico. This file is managed by Portico: do not edit it by hand.",
    "# Portico never edits ~/.ssh/config. To use these hosts with ssh, scp and git too,",
    "# add this line at the top of ~/.ssh/config:",
];
/// Start of the header line showing the `Include` line.
const HEADER_INCLUDE_PREFIX: &str = "#   Include ";

// ── Public types ────────────────────────────────────────────────────────

/// What to do with the remembered password of a host.
#[derive(Debug, Default)]
pub enum PasswordChange {
    /// Leave it as it is (on creation: none).
    #[default]
    Keep,
    /// Remember this password (replaces any remembered one).
    Set(SecretString),
    /// Forget the remembered password.
    Forget,
}

/// A host as typed in a create or edit form. Validated by the engine
/// ([`crate::Engine::create_host`], [`crate::Engine::update_host`]).
#[derive(Debug, Default)]
pub struct HostDraft {
    /// `[a-z0-9_.-]`, at most 64 characters, not starting with `-` or `.`.
    pub alias: String,
    /// DNS name or IP address (`[A-Za-z0-9._:-]`, brackets removed).
    pub hostname: String,
    /// `None` (or blank): no `User` line, the usual default applies.
    pub user: Option<String>,
    /// 1–65535; `None`: no `Port` line.
    pub port: Option<u32>,
    /// A private key: `~/…` or an absolute path, an existing regular file
    /// under the home directory (a link may not lead outside it).
    pub identity_file: Option<PathBuf>,
    /// An existing host alias (any source), not leading back to this host.
    pub proxy_jump: Option<String>,
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    pub notes: Option<String>,
    pub password: PasswordChange,
}

/// A host of `hosts.conf`, as Portico wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedHost {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Absolute path (written `~/…` under the home directory).
    pub identity_file: Option<PathBuf>,
    pub proxy_jump: Option<String>,
}

/// What an edit form needs about a Portico host
/// ([`crate::Engine::managed_host_details`]): the values `hosts.conf` holds,
/// whether a password is remembered for its account, and the import it
/// comes from. Never holds a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedHostDetails {
    #[serde(flatten)]
    pub host: ManagedHost,
    /// A password is remembered for the account this host logs in to
    /// (`user@host:port`), unbound (typed in a form, not used yet) or bound
    /// to one of the server's keys. Only the presence is looked up, never
    /// the value. `false` when the secret store cannot tell (vault locked
    /// or missing, keyring unavailable).
    pub password_remembered: bool,
    /// The tool the host was imported from (`[hosts.<alias>.import]` of
    /// `hosts.toml`), still set once it was edited; `None` for a host
    /// created in Portico.
    pub imported_from: Option<ImportSource>,
}

/// A host created, edited or duplicated in Portico, with its metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSummary {
    #[serde(flatten)]
    pub host: ManagedHost,
    pub meta: HostMeta,
    /// The file the host lives in (`hosts.conf`).
    pub file: PathBuf,
}

/// A host Portico cannot create, change or delete. Messages never echo a
/// refused value (it may hold a line break or invisible characters).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManagedHostError {
    #[error("invalid host alias: {reason}")]
    InvalidAlias { reason: &'static str },
    #[error("invalid host name: {reason}")]
    InvalidHostname { reason: &'static str },
    #[error("invalid user name: {reason}")]
    InvalidUser { reason: &'static str },
    #[error("invalid port {port}: use 1 to 65535")]
    InvalidPort { port: u32 },
    #[error("invalid identity file: {reason}")]
    InvalidIdentityFile { reason: &'static str },
    #[error("invalid jump host: {reason}")]
    InvalidProxyJump { reason: &'static str },
    #[error("jump host {alias} is not a known host")]
    UnknownJumpHost { alias: String },
    #[error("ProxyJump loop: {chain}")]
    ProxyJumpCycle { chain: String },
    #[error("{alias} is already defined in {}:{line}", file.display())]
    AliasTaken {
        alias: String,
        file: PathBuf,
        line: usize,
    },
    #[error("{alias} is defined in {}, which Portico does not edit", file.display())]
    ReadOnly { alias: String, file: PathBuf },
    #[error("no host named {alias}")]
    NotFound { alias: String },
    #[error("{alias} is the jump host of {by}")]
    InUse { alias: String, by: String },
    #[error("{alias} cannot become a Portico host: {reason}")]
    NotDuplicable { alias: String, reason: &'static str },
    #[error("{}:{line}: {reason}", path.display())]
    FileInvalid {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("{}:{line}: {reason}", path.display())]
    FileUnmanaged {
        path: PathBuf,
        line: usize,
        reason: &'static str,
    },
}

impl ManagedHostError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::InvalidAlias { .. } => (
                "host_invalid_alias",
                Some(
                    "Use lowercase letters, digits, `_`, `.` and `-` (64 at most), not starting with `-` or `.`.",
                ),
            ),
            Self::InvalidHostname { .. } => (
                "host_invalid_hostname",
                Some("Use a DNS name or an IP address: letters, digits, `.`, `-`, `_` and `:`."),
            ),
            Self::InvalidUser { .. } => (
                "host_invalid_user",
                Some(
                    "Use letters, digits, `.`, `-`, `_`, `@` and `+` (64 at most), not starting with `-`.",
                ),
            ),
            Self::InvalidPort { .. } => (
                "host_invalid_port",
                Some("A port is a number from 1 to 65535."),
            ),
            Self::InvalidIdentityFile { .. } => (
                "host_invalid_identity_file",
                Some(
                    "Pick one of your local private keys: a regular file under your home directory.",
                ),
            ),
            Self::InvalidProxyJump { .. } | Self::UnknownJumpHost { .. } => (
                "host_invalid_proxy_jump",
                Some("Pick an existing host as jump host."),
            ),
            Self::ProxyJumpCycle { .. } => (
                "host_proxy_jump_cycle",
                Some("A host cannot reach itself through its jump hosts: pick another jump host."),
            ),
            Self::AliasTaken { .. } => (
                "host_alias_taken",
                Some(
                    "Pick another alias: Portico never shadows a host defined elsewhere (aliases are compared ignoring case).",
                ),
            ),
            Self::ReadOnly { .. } => (
                "host_read_only",
                Some(
                    "Portico never rewrites your ssh config: duplicate it as a Portico host to change it here.",
                ),
            ),
            Self::NotFound { .. } => ("host_not_found", Some("Reload the host list.")),
            Self::InUse { .. } => (
                "host_in_use",
                Some("Change the jump host of those hosts first."),
            ),
            Self::NotDuplicable { .. } => (
                "host_not_duplicable",
                Some(
                    "Portico hosts only hold HostName, User, Port, one IdentityFile and one jump host: keep this host in ~/.ssh/config.",
                ),
            ),
            Self::FileInvalid { .. } => (
                "host_file_invalid",
                Some(
                    "hosts.conf was edited by hand and no longer parses: fix or undo the edit on that line. Portico does not rewrite it until then.",
                ),
            ),
            Self::FileUnmanaged { .. } => (
                "host_file_unmanaged",
                Some(
                    "hosts.conf was edited by hand with something Portico does not manage, and Portico never drops it silently: move that line (or that host) to ~/.ssh/config, or undo the edit, then retry.",
                ),
            ),
        }
    }
}

// ── Values ──────────────────────────────────────────────────────────────

/// A `Host` alias Portico writes ([`is_valid_alias`] of the import, and not
/// `none`, which `ProxyJump none` would read as "no jump host").
fn check_alias(raw: &str) -> std::result::Result<String, ManagedHostError> {
    let alias = raw.trim();
    if alias.is_empty() {
        return Err(ManagedHostError::InvalidAlias { reason: "empty" });
    }
    if !is_valid_alias(alias) {
        return Err(ManagedHostError::InvalidAlias {
            reason: "use lowercase letters, digits, underscore, dot and dash, not starting with a dash or a dot",
        });
    }
    if alias.eq_ignore_ascii_case("none") {
        return Err(ManagedHostError::InvalidAlias {
            reason: "`none` is a reserved word",
        });
    }
    Ok(alias.to_owned())
}

/// Syntax of a `ProxyJump` alias (the user's own aliases may hold
/// uppercase letters): `[A-Za-z0-9_.-]`, not starting with `-` or `.`.
fn check_jump_syntax(raw: &str) -> std::result::Result<(), ManagedHostError> {
    let ok = !raw.is_empty()
        && raw.chars().count() <= MAX_JUMP_ALIAS_CHARS
        && !raw.starts_with(['-', '.'])
        && !raw.eq_ignore_ascii_case("none")
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if ok {
        Ok(())
    } else {
        Err(ManagedHostError::InvalidProxyJump {
            reason: "use one host alias (letters, digits, underscore, dot and dash)",
        })
    }
}

/// Characters of an `IdentityFile` path Portico writes: printable, the
/// space as only blank (the value is then quoted), and none of the
/// characters OpenSSH quotes, expands or reads as a comment.
fn check_path_chars(text: &str) -> std::result::Result<(), &'static str> {
    if text.is_empty() {
        return Err("empty");
    }
    if text.len() > MAX_IDENTITY_PATH_BYTES {
        return Err("longer than 1024 bytes");
    }
    let bad = |c: char| {
        c.is_control()
            || crate::snippets::is_hidden_format(c)
            || (c.is_whitespace() && c != ' ')
            || matches!(c, '"' | '\'' | '\\' | '%' | '$' | '#' | '=' | '`')
    };
    if text.chars().any(bad) {
        return Err(
            "the path holds a quote, `%`, `$`, `#`, `=`, a backslash or a control character",
        );
    }
    Ok(())
}

/// Checks the key chosen for a host and returns its absolute path.
///
/// Accepted: `~/…` or an absolute path, without `.` or `..` components,
/// to an existing **regular file under the home directory**, not a `.pub`.
/// A symlink is followed only if its target stays under the (canonical)
/// home directory. Discovered local keys ([`crate::keys::discover`]) all
/// pass; any other private key under the home directory does too (keys
/// kept outside `~/.ssh`), but nothing outside it. Blocking.
pub(crate) fn check_identity_file(
    home: &Path,
    raw: &Path,
) -> std::result::Result<PathBuf, ManagedHostError> {
    let invalid = |reason| ManagedHostError::InvalidIdentityFile { reason };
    let text = raw
        .to_str()
        .ok_or_else(|| invalid("the path is not UTF-8"))?;
    // Windows: `C:\Users\me\.ssh\id` is checked (and resolved) with `/`
    // separators, the form Portico writes; a backslash stays refused
    // elsewhere (it is an escape for OpenSSH).
    #[cfg(windows)]
    let normalized = text.replace('\\', "/");
    #[cfg(windows)]
    let text = normalized.as_str();
    check_path_chars(text).map_err(invalid)?;
    let path = match text.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(text),
    };
    if !path.is_absolute() {
        return Err(invalid("use an absolute path or one starting with ~/"));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("the path holds `.` or `..`"));
    }
    if path.strip_prefix(home).is_err() {
        return Err(invalid("the key must be under your home directory"));
    }
    if path.extension().is_some_and(|e| e == "pub") {
        return Err(invalid("this is a public key: pick the private key"));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Err(invalid("not a regular file")),
        Err(_) => return Err(invalid("the file does not exist")),
    }
    let (Ok(canonical), Ok(canonical_home)) =
        (std::fs::canonicalize(&path), std::fs::canonicalize(home))
    else {
        return Err(invalid("the file cannot be resolved"));
    };
    if !canonical.starts_with(&canonical_home) {
        return Err(invalid("a link leads outside your home directory"));
    }
    Ok(path)
}

/// `~/…` under the home directory (`/` separators on every platform),
/// double-quoted when it holds a space.
fn identity_value(home: &Path, path: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", crate::store::slash_relative(rest)),
        Err(_) => path.display().to_string(),
    };
    if shown.contains(' ') {
        format!("\"{shown}\"")
    } else {
        shown
    }
}

/// Validates a draft into the host to write and the metadata to set.
fn checked_draft(
    home: &Path,
    draft: &HostDraft,
) -> std::result::Result<(ManagedHost, Vec<MetaChange>), Error> {
    let alias = check_alias(&draft.alias)?;
    let hostname = clean_hostname(&draft.hostname).ok_or(ManagedHostError::InvalidHostname {
        reason: "use letters, digits, dot, dash, underscore and colon, not starting with a dash or a dot",
    })?;
    let user = match draft.user.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(clean_user(raw).ok_or(ManagedHostError::InvalidUser {
            reason: "use letters, digits, dot, dash, underscore, @ and +, not starting with a dash",
        })?),
    };
    let port = match draft.port {
        None => None,
        Some(port) => Some(
            u16::try_from(port)
                .ok()
                .filter(|p| *p != 0)
                .ok_or(ManagedHostError::InvalidPort { port })?,
        ),
    };
    let identity_file = match &draft.identity_file {
        Some(path) if !path.as_os_str().is_empty() => Some(check_identity_file(home, path)?),
        _ => None,
    };
    let proxy_jump = match draft.proxy_jump.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            check_jump_syntax(raw)?;
            Some(raw.to_owned())
        }
    };
    let changes = vec![
        MetaChange::Tags(draft.tags.clone()),
        MetaChange::Group(draft.group.clone()),
        MetaChange::Favorite(draft.favorite),
        MetaChange::Notes(draft.notes.clone()),
    ]
    .into_iter()
    .map(MetaChange::normalized)
    .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((
        ManagedHost {
            alias,
            hostname,
            user,
            port,
            identity_file,
            proxy_jump,
        },
        changes,
    ))
}

// ── The file ────────────────────────────────────────────────────────────

/// The text of `hosts.conf` for `hosts` (values already checked).
pub(crate) fn render(paths: &Paths, hosts: &[ManagedHost]) -> String {
    use std::fmt::Write as _;

    let mut text = String::new();
    for line in HEADER_LINES {
        let _ = writeln!(text, "{line}");
    }
    let _ = writeln!(text, "#   {}", include_line(paths, &paths.managed_hosts()));
    for host in hosts {
        let _ = writeln!(text, "\nHost {}", host.alias);
        let _ = writeln!(text, "    HostName {}", host.hostname);
        if let Some(user) = &host.user {
            let _ = writeln!(text, "    User {user}");
        }
        if let Some(port) = host.port {
            let _ = writeln!(text, "    Port {port}");
        }
        if let Some(identity) = &host.identity_file {
            let _ = writeln!(
                text,
                "    IdentityFile {}",
                identity_value(&paths.home, identity)
            );
            let _ = writeln!(text, "    IdentitiesOnly yes");
        }
        if let Some(jump) = &host.proxy_jump {
            let _ = writeln!(text, "    ProxyJump {jump}");
        }
    }
    text
}

/// A `Host` block being read by [`parse_file`].
struct BlockReader {
    host: ManagedHost,
    line: usize,
    has_hostname: bool,
    identities_only: bool,
    seen: Vec<String>,
}

impl BlockReader {
    /// Reads one directive of the block; `Err` says what Portico would not
    /// write.
    fn read(
        &mut self,
        keyword: &str,
        value: &str,
        home: &Path,
    ) -> std::result::Result<(), &'static str> {
        match keyword {
            "hostname" => {
                if clean_hostname(value).as_deref() != Some(value) {
                    return Err("a HostName Portico does not write");
                }
                value.clone_into(&mut self.host.hostname);
                self.has_hostname = true;
            }
            "user" => {
                if clean_user(value).as_deref() != Some(value) {
                    return Err("a User Portico does not write");
                }
                self.host.user = Some(value.to_owned());
            }
            "port" => {
                let port = value
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0 && p.to_string() == value)
                    .ok_or("a Port Portico does not write")?;
                self.host.port = Some(port);
            }
            "identityfile" => {
                let refused = "an IdentityFile Portico does not write";
                check_path_chars(value).map_err(|_| refused)?;
                let path = match value.strip_prefix("~/") {
                    Some(rest) => home.join(rest),
                    None => PathBuf::from(value),
                };
                if !path.is_absolute()
                    || path
                        .components()
                        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
                {
                    return Err(refused);
                }
                self.host.identity_file = Some(path);
            }
            "identitiesonly" => {
                if value != "yes" {
                    return Err("an IdentitiesOnly Portico does not write");
                }
                self.identities_only = true;
            }
            "proxyjump" => {
                check_jump_syntax(value).map_err(|_| "a ProxyJump Portico does not write")?;
                self.host.proxy_jump = Some(value.to_owned());
            }
            _ => return Err("a directive Portico does not manage"),
        }
        Ok(())
    }

    fn finish(self, path: &Path) -> std::result::Result<ManagedHost, ManagedHostError> {
        let unmanaged = |reason| ManagedHostError::FileUnmanaged {
            path: path.to_path_buf(),
            line: self.line,
            reason,
        };
        if !self.has_hostname {
            return Err(unmanaged("a Host block without HostName"));
        }
        if self.identities_only != self.host.identity_file.is_some() {
            return Err(unmanaged(
                "IdentityFile and IdentitiesOnly yes go together in a Portico host",
            ));
        }
        Ok(self.host)
    }
}

/// Reads `hosts.conf` back, refusing anything Portico would not write
/// (see the module doc): the returned hosts render to an equivalent file,
/// so rewriting it loses nothing.
pub(crate) fn parse_file(
    path: &Path,
    home: &Path,
    text: &str,
) -> std::result::Result<Vec<ManagedHost>, ManagedHostError> {
    let mut hosts: Vec<ManagedHost> = Vec::new();
    let mut current: Option<BlockReader> = None;
    let mut in_header = true;
    for (index, raw) in text.lines().enumerate() {
        let line = index.saturating_add(1);
        let unmanaged = |reason| ManagedHostError::FileUnmanaged {
            path: path.to_path_buf(),
            line,
            reason,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            let header =
                HEADER_LINES.contains(&trimmed) || trimmed.starts_with(HEADER_INCLUDE_PREFIX);
            if in_header && header {
                continue;
            }
            return Err(unmanaged("a comment Portico would not keep"));
        }
        in_header = false;
        if raw.contains('#') {
            return Err(unmanaged("a trailing comment Portico would not keep"));
        }
        let parsed = tokenize(raw).map_err(|reason| ManagedHostError::FileInvalid {
            path: path.to_path_buf(),
            line,
            reason,
        })?;
        let Some(parsed) = parsed else { continue };
        let [value] = parsed.args.as_slice() else {
            return Err(unmanaged("a line Portico writes with exactly one value"));
        };
        if parsed.keyword == "host" {
            if !is_valid_alias(value) || value.eq_ignore_ascii_case("none") {
                return Err(unmanaged(
                    "a Host line Portico does not write (one plain alias)",
                ));
            }
            if let Some(done) = current.take() {
                hosts.push(done.finish(path)?);
            }
            if hosts.iter().any(|h| h.alias.eq_ignore_ascii_case(value)) {
                return Err(unmanaged("an alias declared twice"));
            }
            current = Some(BlockReader {
                host: ManagedHost {
                    alias: value.clone(),
                    hostname: String::new(),
                    user: None,
                    port: None,
                    identity_file: None,
                    proxy_jump: None,
                },
                line,
                has_hostname: false,
                identities_only: false,
                seen: Vec::new(),
            });
            continue;
        }
        let Some(block) = current.as_mut() else {
            return Err(unmanaged("a directive outside a Host block"));
        };
        if block.seen.contains(&parsed.keyword) {
            return Err(unmanaged("a directive repeated in one Host block"));
        }
        block.seen.push(parsed.keyword.clone());
        block
            .read(&parsed.keyword, value, home)
            .map_err(unmanaged)?;
    }
    if let Some(done) = current.take() {
        hosts.push(done.finish(path)?);
    }
    Ok(hosts)
}

/// Reads the hosts of `hosts.conf` (none when it is missing). Blocking.
pub(crate) fn read_hosts(paths: &Paths) -> Result<Vec<ManagedHost>> {
    let target = paths.managed_hosts();
    match store::read_capped(&target, MAX_MANAGED_FILE_BYTES)? {
        None => Ok(Vec::new()),
        Some(text) => Ok(parse_file(&target, &paths.home, &text)?),
    }
}

// ── Loading ─────────────────────────────────────────────────────────────

/// The configuration the engine reads (inventory, sessions, effective
/// config): `~/.ssh/config`, with `hosts.conf` read first unless an
/// `Include` of the user's already resolves to it (see the module doc). A
/// `hosts.conf` that cannot be read or parsed is skipped with a warning:
/// the user's own hosts still load. Blocking.
pub(crate) fn load_config(paths: &Paths) -> Result<SshConfig> {
    let user = SshConfig::load(&paths.ssh_config(), &paths.ssh_dir)?;
    let target = paths.managed_hosts();
    if crate::import::includes(&user, &target) {
        return Ok(user);
    }
    let warning = |line: usize, message: String| ConfigWarning {
        source: Source {
            file: target.clone(),
            line,
        },
        message: format!("Portico hosts not loaded: {message}"),
    };
    let text = match store::read_capped(&target, MAX_MANAGED_FILE_BYTES) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(user),
        Err(e) => {
            let mut user = user;
            user.push_warning(warning(0, e.summary().message));
            return Ok(user);
        }
    };
    match SshConfig::parse_str(&text, &target, &paths.ssh_dir) {
        Ok(prelude) => Ok(user.with_prelude(prelude)),
        Err(e) => {
            let line = match &e {
                crate::ssh_config::ConfigError::Syntax { line, .. }
                | crate::ssh_config::ConfigError::IncludeDepth { line, .. } => *line,
                _ => 0,
            };
            let mut user = user;
            user.push_warning(warning(line, e.to_string()));
            Ok(user)
        }
    }
}

/// Marks each host with the file it comes from: `hosts.conf` is
/// [`HostOrigin::Portico`], the Termius import file
/// [`HostOrigin::Imported`], anything else [`HostOrigin::SshConfig`]; and
/// each Portico host created by an import with its source
/// ([`HostEntry::imported_from`]; an unreadable `hosts.toml` leaves it
/// `None`). Blocking (paths are compared canonicalized).
pub(crate) fn mark_origins(paths: &Paths, hosts: &mut [HostEntry]) {
    let managed = paths.managed_hosts();
    let imported = crate::import::termius_target(paths);
    let mut known: HashMap<PathBuf, HostOrigin> = HashMap::new();
    for host in hosts.iter_mut() {
        let file = &host.source.file;
        let origin = *known.entry(file.clone()).or_insert_with(|| {
            if same_file(file, &managed) {
                HostOrigin::Portico
            } else if same_file(file, &imported) {
                HostOrigin::Imported
            } else {
                HostOrigin::SshConfig
            }
        });
        host.origin = origin;
        host.imported_from = None;
    }
    if !hosts.iter().any(|h| h.origin == HostOrigin::Portico) {
        return;
    }
    let Ok(provenance) = crate::hosts::load_provenance(&paths.hosts_meta()) else {
        return;
    };
    for host in hosts.iter_mut().filter(|h| h.origin == HostOrigin::Portico) {
        host.imported_from = provenance
            .get(&host.alias)
            .and_then(|p| ImportSource::from_name(&p.source));
    }
}

/// The line to add at the top of `~/.ssh/config` so OpenSSH sees the
/// hosts created in Portico.
pub fn include_line_for(paths: &Paths) -> String {
    include_line(paths, &paths.managed_hosts())
}

// ── Editing ─────────────────────────────────────────────────────────────

/// Where a remembered password belongs: the account Portico logs in to
/// (the same fields sessions use, see `session::auth`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct PasswordTarget {
    user: String,
    /// `known_hosts` name.
    host: String,
    port: u16,
    /// `known_hosts` files holding the keys the password may be bound to.
    known_hosts_files: Vec<PathBuf>,
    /// `hosts.toml`, whose index lists the keys passwords were bound to.
    meta_path: PathBuf,
}

impl PasswordTarget {
    fn same_account(&self, other: &Self) -> bool {
        self.user == other.user && self.host == other.host && self.port == other.port
    }

    fn unbound_key(&self) -> SecretKey {
        SecretKey::UnboundHostPassword {
            user: self.user.clone(),
            host: self.host.clone(),
            port: self.port,
        }
    }

    /// `true` when a password is remembered for this account: the unbound
    /// one, or one bound to a key `known_hosts` or the index of
    /// `hosts.toml` lists for it. Only presence is checked. A store that
    /// cannot tell (locked vault) counts as none. Blocking.
    fn remembered(&self, secrets: &dyn SecretStore) -> bool {
        if secrets.contains(&self.unbound_key()).unwrap_or(false) {
            return true;
        }
        let mut fingerprints = KnownHosts::load(&self.known_hosts_files)
            .map(|known| known.fingerprints(&self.host, self.port))
            .unwrap_or_default();
        fingerprints.extend(
            crate::hosts::bound_password_fingerprints(
                &self.meta_path,
                &self.user,
                &self.host,
                self.port,
            )
            .unwrap_or_default(),
        );
        fingerprints.sort();
        fingerprints.dedup();
        fingerprints.into_iter().any(|fingerprint| {
            secrets
                .contains(&SecretKey::HostPassword {
                    user: self.user.clone(),
                    host: self.host.clone(),
                    port: self.port,
                    host_key: fingerprint,
                })
                .unwrap_or(false)
        })
    }

    /// Forgets the unbound password and every one bound to a key that
    /// `known_hosts` records for this host. Blocking.
    fn forget(&self, secrets: &dyn SecretStore) -> Result<()> {
        secrets.delete(&self.unbound_key())?;
        self.forget_bound(secrets)
    }

    /// Forgets the passwords bound to a key of this host (the unbound one is
    /// kept): the keys `known_hosts` records for it, and every key the
    /// index of `hosts.toml` lists for this account (a password bound
    /// through a host certificate, or to a key whose `known_hosts` line is
    /// gone). Blocking.
    fn forget_bound(&self, secrets: &dyn SecretStore) -> Result<()> {
        let known = KnownHosts::load(&self.known_hosts_files)?;
        let mut fingerprints = known.fingerprints(&self.host, self.port);
        fingerprints.extend(crate::hosts::bound_password_fingerprints(
            &self.meta_path,
            &self.user,
            &self.host,
            self.port,
        )?);
        fingerprints.sort();
        fingerprints.dedup();
        for fingerprint in fingerprints {
            secrets.delete(&SecretKey::HostPassword {
                user: self.user.clone(),
                host: self.host.clone(),
                port: self.port,
                host_key: fingerprint,
            })?;
        }
        crate::hosts::remove_bound_passwords(&self.meta_path, &self.user, &self.host, self.port)
    }
}

/// Everything an edit needs, read under the `hosts.conf` lock.
struct Editor<'a> {
    config: &'a EngineConfig,
    target: PathBuf,
    /// The hosts of `hosts.conf` as read (strictly).
    hosts: Vec<ManagedHost>,
    /// `~/.ssh/config` has an `Include` resolving to `hosts.conf`.
    explicit: bool,
    ctx: ResolveContext,
    /// `None` for a read-only look (an import plan): never written then.
    lock: Option<std::fs::File>,
}

impl<'a> Editor<'a> {
    fn open(config: &'a EngineConfig) -> Result<Self> {
        let target = config.paths.managed_hosts();
        let lock = store::lock_exclusive(&store::lock_path(&target))?;
        Self::read(config, Some(lock))
    }

    /// Reads everything an edit needs; `lock` is held until dropped.
    fn read(config: &'a EngineConfig, lock: Option<std::fs::File>) -> Result<Self> {
        let paths = &config.paths;
        let target = paths.managed_hosts();
        let hosts = read_hosts(paths)?;
        let user = SshConfig::load(&paths.ssh_config(), &paths.ssh_dir)?;
        let explicit = crate::import::includes(&user, &target);
        Ok(Self {
            ctx: config.resolve_context(None),
            config,
            target,
            hosts,
            explicit,
            lock,
        })
    }

    fn paths(&self) -> &Paths {
        &self.config.paths
    }

    /// The configuration as the engine will load it once `hosts.conf`
    /// holds `hosts`.
    fn config_with(&self, hosts: &[ManagedHost]) -> Result<SshConfig> {
        let paths = self.paths();
        let text = render(paths, hosts);
        if self.explicit {
            return Ok(SshConfig::load_replacing(
                &paths.ssh_config(),
                &paths.ssh_dir,
                &self.target,
                &text,
            )?);
        }
        let user = SshConfig::load(&paths.ssh_config(), &paths.ssh_dir)?;
        let prelude = SshConfig::parse_str(&text, &self.target, &paths.ssh_dir)?;
        Ok(user.with_prelude(prelude))
    }

    /// Every concrete alias the engine loads (every occurrence), plus those
    /// of the Termius import file when it is not included (they appear as
    /// soon as the user adds its `Include` line).
    fn every_alias(&self, config: &SshConfig) -> Vec<HostAlias> {
        let mut out = config.all_concrete_aliases();
        let paths = self.paths();
        let termius = crate::import::termius_target(paths);
        if !out.iter().any(|a| same_file(&a.source.file, &termius))
            && let Ok(imported) = SshConfig::load(&termius, &paths.ssh_dir)
        {
            out.extend(imported.all_concrete_aliases());
        }
        out
    }

    /// Refuses `alias` when another host already uses it, ignoring case:
    /// in any other file, or another host of `hosts.conf` than `current`
    /// (the host being edited).
    fn check_free(&self, config: &SshConfig, alias: &str, current: Option<&str>) -> Result<()> {
        let found = self.every_alias(config).into_iter().find(|a| {
            a.name.eq_ignore_ascii_case(alias)
                && !(current.is_some_and(|c| a.name == c)
                    && same_file(&a.source.file, &self.target))
        });
        match found {
            Some(found) => Err(ManagedHostError::AliasTaken {
                alias: alias.to_owned(),
                file: found.source.file,
                line: found.source.line,
            }
            .into()),
            None => Ok(()),
        }
    }

    /// The jump host spelled as declared, when it is a known alias.
    fn known_jump(config: &SshConfig, jump: &str) -> Result<String> {
        config
            .all_concrete_aliases()
            .into_iter()
            .find(|a| a.name.eq_ignore_ascii_case(jump))
            .map(|a| a.name)
            .ok_or_else(|| {
                ManagedHostError::UnknownJumpHost {
                    alias: jump.to_owned(),
                }
                .into()
            })
    }

    /// The managed host named `alias` (ignoring case), or why there is none.
    fn position(&self, alias: &str) -> Result<usize> {
        if let Some(index) = self
            .hosts
            .iter()
            .position(|h| h.alias.eq_ignore_ascii_case(alias))
        {
            return Ok(index);
        }
        let config = self.config_with(&self.hosts)?;
        let found = config
            .all_concrete_aliases()
            .into_iter()
            .find(|a| a.name.eq_ignore_ascii_case(alias));
        Err(match found {
            Some(found) => ManagedHostError::ReadOnly {
                alias: found.name,
                file: found.source.file,
            },
            None => ManagedHostError::NotFound {
                alias: display_text(alias, 64),
            },
        }
        .into())
    }

    /// The account `alias` logs in to under `config`.
    fn password_target(&self, config: &SshConfig, alias: &str) -> PasswordTarget {
        let resolved = config.resolve(alias, &self.ctx);
        let (host, port) = crate::session::handler::known_hosts_name(&resolved);
        let paths = self.paths();
        let mut known_hosts_files = resolved.user_known_hosts_files.clone();
        if known_hosts_files.is_empty() {
            known_hosts_files = vec![paths.known_hosts(), paths.ssh_dir.join("known_hosts2")];
        }
        known_hosts_files.extend(paths.global_known_hosts.iter().cloned());
        PasswordTarget {
            user: resolved
                .user
                .clone()
                .unwrap_or_else(|| self.config.local_user.clone()),
            host,
            port,
            known_hosts_files,
            meta_path: paths.hosts_meta(),
        }
    }

    /// `true` when a host of `config` other than `except` logs in to the
    /// same account as `target`.
    fn account_shared(&self, config: &SshConfig, target: &PasswordTarget, except: &str) -> bool {
        config
            .host_aliases()
            .iter()
            .filter(|a| !a.name.eq_ignore_ascii_case(except))
            .any(|a| self.password_target(config, &a.name).same_account(target))
    }

    /// Hosts of `config` (other than `alias`) whose `ProxyJump` names
    /// `alias`, split into those of `hosts.conf` and the others.
    fn referrers(&self, config: &SshConfig, alias: &str) -> (Vec<String>, Vec<String>) {
        let mut managed = Vec::new();
        let mut foreign = Vec::new();
        for host in config.host_aliases() {
            if host.name.eq_ignore_ascii_case(alias) {
                continue;
            }
            let resolved = config.resolve(&host.name, &self.ctx);
            if resolved
                .proxy_jump
                .iter()
                .any(|j| j.host.eq_ignore_ascii_case(alias))
            {
                if same_file(&host.source.file, &self.target) {
                    managed.push(host.name);
                } else {
                    foreign.push(host.name);
                }
            }
        }
        (managed, foreign)
    }

    /// Refuses a `ProxyJump` chain from `start` that leads back to it.
    fn check_cycle(&self, config: &SshConfig, start: &str) -> Result<()> {
        let mut walk = JumpWalk {
            config,
            ctx: &self.ctx,
            start,
            path: vec![start.to_owned()],
            visited: HashSet::new(),
        };
        match walk.from(start, 0) {
            Some(chain) => Err(ManagedHostError::ProxyJumpCycle {
                chain: chain.join(" → "),
            }
            .into()),
            None => Ok(()),
        }
    }

    fn write(&self, hosts: &[ManagedHost]) -> Result<()> {
        if self.lock.is_none() {
            // Read without the lock: writing would race another editor.
            return Err(Error::io(
                &self.target,
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ));
        }
        let text = render(self.paths(), hosts);
        store::write_atomic_capped(&self.target, text.as_bytes(), MAX_MANAGED_FILE_BYTES)
    }

    fn summary(&self, host: ManagedHost, meta: HostMeta) -> HostSummary {
        HostSummary {
            host,
            meta,
            file: self.target.clone(),
        }
    }

    /// Validates `draft` as a new host and writes it (see [`create`]).
    fn create(&self, draft: &HostDraft) -> Result<HostSummary> {
        let paths = self.paths();
        let (mut host, changes) = checked_draft(&paths.home, draft)?;
        let before = self.config_with(&self.hosts)?;
        self.check_free(&before, &host.alias, None)?;
        if let Some(jump) = &host.proxy_jump {
            if jump.eq_ignore_ascii_case(&host.alias) {
                return Err(ManagedHostError::ProxyJumpCycle {
                    chain: format!("{0} → {0}", host.alias),
                }
                .into());
            }
            host.proxy_jump = Some(Self::known_jump(&before, jump)?);
        }
        let mut hosts = self.hosts.clone();
        hosts.push(host.clone());
        let after = self.config_with(&hosts)?;
        self.check_cycle(&after, &host.alias)?;

        let secrets = Arc::clone(&self.config.secrets);
        let account = self.password_target(&after, &host.alias);
        let stored = match &draft.password {
            PasswordChange::Set(password) => {
                secrets.set(&account.unbound_key(), password)?;
                true
            }
            PasswordChange::Keep | PasswordChange::Forget => false,
        };
        if let Err(e) = self.write(&hosts) {
            if stored {
                let _ = secrets.delete(&account.unbound_key());
            }
            return Err(e);
        }
        // Another host may reach the same account with a password bound to
        // its key: the one just typed replaces it (it would be tried first).
        let forgotten = if stored {
            account.forget_bound(secrets.as_ref())
        } else {
            Ok(())
        };
        let meta = crate::hosts::replace_meta(&paths.hosts_meta(), &host.alias, None, changes)?;
        // Created by hand: never taken for a host an import may update.
        crate::hosts::remove_provenance(&paths.hosts_meta(), &host.alias)?;
        forgotten?;
        Ok(self.summary(host, meta))
    }

    /// See [`update`].
    fn update(&self, alias: &str, draft: &HostDraft) -> Result<HostSummary> {
        let paths = self.paths();
        let index = self.position(alias)?;
        let old = self
            .hosts
            .get(index)
            .cloned()
            .ok_or_else(|| ManagedHostError::NotFound {
                alias: display_text(alias, 64),
            })?;
        let (mut host, changes) = checked_draft(&paths.home, draft)?;
        let before = self.config_with(&self.hosts)?;
        self.check_free(&before, &host.alias, Some(&old.alias))?;
        if let Some(jump) = &host.proxy_jump {
            if jump.eq_ignore_ascii_case(&host.alias) || jump.eq_ignore_ascii_case(&old.alias) {
                return Err(ManagedHostError::ProxyJumpCycle {
                    chain: format!("{0} → {0}", host.alias),
                }
                .into());
            }
            host.proxy_jump = Some(Self::known_jump(&before, jump)?);
        }
        let renamed = host.alias != old.alias;
        let mut hosts = self.hosts.clone();
        if renamed {
            let (_, foreign) = self.referrers(&before, &old.alias);
            if !foreign.is_empty() {
                return Err(ManagedHostError::InUse {
                    alias: old.alias.clone(),
                    by: foreign.join(", "),
                }
                .into());
            }
            // Hosts of this file that jump through it follow the rename.
            for other in &mut hosts {
                if other
                    .proxy_jump
                    .as_deref()
                    .is_some_and(|j| j.eq_ignore_ascii_case(&old.alias))
                {
                    other.proxy_jump = Some(host.alias.clone());
                }
            }
        }
        if let Some(slot) = hosts.get_mut(index) {
            *slot = host.clone();
        }
        let after = self.config_with(&hosts)?;
        self.check_cycle(&after, &host.alias)?;

        let secrets = Arc::clone(&self.config.secrets);
        let old_account = self.password_target(&before, &old.alias);
        let new_account = self.password_target(&after, &host.alias);
        // The value the new password replaces, restored should the file
        // not be written (`create` deletes it instead: there was none).
        let replaced = match &draft.password {
            PasswordChange::Set(password) => {
                let unbound = new_account.unbound_key();
                let previous = secrets.get(&unbound)?;
                secrets.set(&unbound, password)?;
                Some(previous)
            }
            PasswordChange::Keep | PasswordChange::Forget => None,
        };
        if let Err(e) = self.write(&hosts) {
            if let Some(previous) = replaced {
                let unbound = new_account.unbound_key();
                let _ = match previous {
                    Some(old) => secrets.set(&unbound, &old),
                    None => secrets.delete(&unbound),
                };
            }
            return Err(e);
        }

        let mut first_error: Option<Error> = None;
        let mut keep = |result: Result<()>| {
            if let Err(e) = result {
                first_error.get_or_insert(e);
            }
        };
        match &draft.password {
            // The new password replaces any bound one (tried first otherwise).
            PasswordChange::Set(_) => keep(new_account.forget_bound(secrets.as_ref())),
            PasswordChange::Forget => keep(new_account.forget(secrets.as_ref())),
            PasswordChange::Keep => {}
        }
        // A password never follows a host to another server or account.
        if !old_account.same_account(&new_account)
            && !self.account_shared(&after, &old_account, &host.alias)
        {
            keep(old_account.forget(secrets.as_ref()));
        }
        let meta = crate::hosts::replace_meta(
            &paths.hosts_meta(),
            &host.alias,
            renamed.then_some(old.alias.as_str()),
            changes,
        );
        if let Some(e) = first_error {
            return Err(e);
        }
        Ok(self.summary(host, meta?))
    }

    /// See [`details`].
    fn details(&self, alias: &str) -> Result<Option<ManagedHostDetails>> {
        let Some(host) = self
            .hosts
            .iter()
            .find(|h| h.alias.eq_ignore_ascii_case(alias))
            .cloned()
        else {
            return Ok(None);
        };
        let config = self.config_with(&self.hosts)?;
        let account = self.password_target(&config, &host.alias);
        let password_remembered = account.remembered(self.config.secrets.as_ref());
        let imported_from = crate::hosts::load_provenance(&self.paths().hosts_meta())?
            .get(&host.alias)
            .and_then(|p| ImportSource::from_name(&p.source));
        Ok(Some(ManagedHostDetails {
            host,
            password_remembered,
            imported_from,
        }))
    }

    /// See [`delete`].
    fn delete(&self, alias: &str) -> Result<String> {
        let paths = self.paths();
        let index = self.position(alias)?;
        let mut hosts = self.hosts.clone();
        let removed = hosts.remove(index);
        let before = self.config_with(&self.hosts)?;
        let (managed, foreign) = self.referrers(&before, &removed.alias);
        if !managed.is_empty() || !foreign.is_empty() {
            let mut by = managed;
            by.extend(foreign);
            return Err(ManagedHostError::InUse {
                alias: removed.alias,
                by: by.join(", "),
            }
            .into());
        }
        let account = self.password_target(&before, &removed.alias);
        self.write(&hosts)?;
        let after = self.config_with(&hosts)?;

        let meta = crate::hosts::remove_meta(&paths.hosts_meta(), &removed.alias);
        if !self.account_shared(&after, &account, &removed.alias) {
            account.forget(self.config.secrets.as_ref())?;
        }
        meta?;
        Ok(removed.alias)
    }

    /// See [`duplicate`].
    fn duplicate(&self, alias: &str, new_alias: &str) -> Result<HostSummary> {
        let config = self.config_with(&self.hosts)?;
        let found = config
            .all_concrete_aliases()
            .into_iter()
            .find(|a| a.name.eq_ignore_ascii_case(alias))
            .ok_or_else(|| ManagedHostError::NotFound {
                alias: display_text(alias, 64),
            })?;
        let not_duplicable = |reason| ManagedHostError::NotDuplicable {
            alias: found.name.clone(),
            reason,
        };
        let resolved = config.resolve(&found.name, &self.ctx);
        if resolved.proxy_command.is_some() {
            return Err(not_duplicable("it uses ProxyCommand").into());
        }
        let proxy_jump = match resolved.proxy_jump.as_slice() {
            [] => None,
            [jump] if jump.user.is_none() && jump.port.is_none() => Some(jump.host.clone()),
            _ => {
                return Err(
                    not_duplicable("it uses a ProxyJump chain or a user@host:port jump").into(),
                );
            }
        };
        let home = &self.paths().home;
        let identity_file = resolved
            .identity_files
            .iter()
            .find_map(|path| check_identity_file(home, path).ok());
        // Only values set by the config: an unset one keeps resolving the
        // same way (the defaults, or the user's `Host *`).
        let port = resolved
            .options
            .contains_key("port")
            .then_some(u32::from(resolved.port));
        let (meta, _) = crate::hosts::load_meta(&self.paths().hosts_meta())?;
        let source_meta = meta.hosts.get(&found.name).cloned().unwrap_or_default();
        let draft = HostDraft {
            alias: new_alias.to_owned(),
            hostname: resolved.hostname.clone(),
            user: resolved.user.clone(),
            port,
            identity_file,
            proxy_jump,
            tags: source_meta.tags,
            group: source_meta.group,
            favorite: false,
            notes: None,
            password: PasswordChange::Keep,
        };
        self.create(&draft)
    }
}

// ── Imports ─────────────────────────────────────────────────────────────

/// Hash of the values Portico writes for `host` (alias, `HostName`,
/// `User`, `Port`, `IdentityFile`, `ProxyJump`), as lowercase hex SHA-256.
/// Stored with an imported host (`crate::hosts::ImportProvenance`): a host
/// whose values no longer hash to it was edited by hand, and a re-import
/// leaves it alone. Metadata (tags, group, favorite, notes) is not part of
/// it: editing them does not stop later imports from updating the host.
pub(crate) fn values_hash(home: &Path, host: &ManagedHost) -> String {
    use std::fmt::Write as _;

    let mut text = String::from("portico-host-v1");
    for field in [
        host.alias.clone(),
        host.hostname.clone(),
        host.user.clone().unwrap_or_default(),
        host.port.map(|p| p.to_string()).unwrap_or_default(),
        host.identity_file
            .as_deref()
            .map(|p| identity_value(home, p))
            .unwrap_or_default(),
        host.proxy_jump.clone().unwrap_or_default(),
    ] {
        text.push('\0');
        text.push_str(&field);
    }
    let digest = russh::keys::HashAlg::Sha256.digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// `hosts.conf` held under its lock for an import ([`crate::import`]): the
/// same lock, strict reading and allowlists as the editing commands.
pub(crate) struct ImportTarget<'a> {
    editor: Editor<'a>,
}

impl<'a> ImportTarget<'a> {
    /// Takes the lock and reads `hosts.conf` strictly (a hand-edited file
    /// Portico would not rewrite fails with `host_file_unmanaged`).
    pub(crate) fn open(config: &'a EngineConfig) -> Result<Self> {
        Ok(Self {
            editor: Editor::open(config)?,
        })
    }

    /// Reads `hosts.conf` strictly, without taking the lock nor creating
    /// anything (an import plan writes nothing); [`Self::write`] refuses.
    pub(crate) fn read_only(config: &'a EngineConfig) -> Result<Self> {
        Ok(Self {
            editor: Editor::read(config, None)?,
        })
    }

    /// The hosts of `hosts.conf`, in file order.
    pub(crate) fn hosts(&self) -> &[ManagedHost] {
        &self.editor.hosts
    }

    /// `hosts.conf` itself.
    pub(crate) fn file(&self) -> &Path {
        &self.editor.target
    }

    /// Every concrete alias occurrence the engine loads, plus those of the
    /// old Termius import file when it is not included (the aliases an
    /// import must never shadow, its own updatable hosts aside).
    pub(crate) fn every_alias(&self) -> Result<Vec<HostAlias>> {
        let config = self.editor.config_with(&self.editor.hosts)?;
        Ok(self.editor.every_alias(&config))
    }

    /// The `ProxyJump` hosts and the `HostName` values (lowercased) of
    /// every host of the configuration as the engine loads it; the
    /// `HostName`s of the aliases in `skip` (lowercased) are left out.
    pub(crate) fn routing_names(
        &self,
        skip: &HashSet<String>,
    ) -> Result<(HashSet<String>, HashSet<String>)> {
        let config = self.editor.config_with(&self.editor.hosts)?;
        let mut jumps = HashSet::new();
        let mut hostnames = HashSet::new();
        for alias in config.host_aliases() {
            let resolved = config.resolve(&alias.name, &self.editor.ctx);
            for jump in &resolved.proxy_jump {
                jumps.insert(jump.host.to_lowercase());
            }
            if !skip.contains(&alias.name.to_lowercase()) {
                hostnames.insert(resolved.hostname.to_lowercase());
            }
        }
        Ok((jumps, hostnames))
    }

    /// Checks every host of `hosts` that differs from the file (same
    /// allowlists as [`create`]; an `IdentityFile` must be an existing
    /// regular file under the home directory), checks the result still
    /// loads, then writes it. Nothing is written when a check fails.
    pub(crate) fn write(&self, hosts: &[ManagedHost]) -> Result<()> {
        let home = &self.editor.paths().home;
        for host in hosts {
            if self.editor.hosts.contains(host) {
                continue;
            }
            check_imported(home, host)?;
        }
        let _ = self.editor.config_with(hosts)?;
        self.editor.write(hosts)
    }
}

/// The checks of [`checked_draft`] on a host built by an import.
fn check_imported(home: &Path, host: &ManagedHost) -> Result<()> {
    let alias = check_alias(&host.alias)?;
    if alias != host.alias {
        return Err(ManagedHostError::InvalidAlias {
            reason: "surrounding blanks",
        }
        .into());
    }
    if clean_hostname(&host.hostname).as_deref() != Some(host.hostname.as_str()) {
        return Err(ManagedHostError::InvalidHostname {
            reason: "use letters, digits, dot, dash, underscore and colon, not starting with a dash or a dot",
        }
        .into());
    }
    if let Some(user) = &host.user
        && clean_user(user).as_deref() != Some(user.as_str())
    {
        return Err(ManagedHostError::InvalidUser {
            reason: "use letters, digits, dot, dash, underscore, @ and +, not starting with a dash",
        }
        .into());
    }
    if host.port == Some(0) {
        return Err(ManagedHostError::InvalidPort { port: 0 }.into());
    }
    if let Some(path) = &host.identity_file
        && check_identity_file(home, path)? != *path
    {
        return Err(ManagedHostError::InvalidIdentityFile {
            reason: "use an absolute path",
        }
        .into());
    }
    if host.proxy_jump.is_some() {
        return Err(ManagedHostError::InvalidProxyJump {
            reason: "an import never sets a jump host",
        }
        .into());
    }
    Ok(())
}

/// Depth-first walk of `ProxyJump` hops looking for `start` again.
struct JumpWalk<'a> {
    config: &'a SshConfig,
    ctx: &'a ResolveContext,
    start: &'a str,
    path: Vec<String>,
    visited: HashSet<String>,
}

impl JumpWalk<'_> {
    fn from(&mut self, current: &str, depth: usize) -> Option<Vec<String>> {
        if depth > MAX_JUMP_DEPTH {
            return None;
        }
        let resolved = self.config.resolve(current, self.ctx);
        for jump in &resolved.proxy_jump {
            if jump.host.eq_ignore_ascii_case(self.start) {
                let mut chain = self.path.clone();
                chain.push(self.start.to_owned());
                return Some(chain);
            }
            if !self.visited.insert(jump.host.to_ascii_lowercase()) {
                continue;
            }
            self.path.push(jump.host.clone());
            if let Some(chain) = self.from(&jump.host, depth.saturating_add(1)) {
                return Some(chain);
            }
            self.path.pop();
        }
        None
    }
}

/// Creates a host in `hosts.conf` (plus its metadata and, when given, its
/// password). Nothing is written when a check fails. Blocking.
pub(crate) fn create(config: &EngineConfig, draft: &HostDraft) -> Result<HostSummary> {
    Editor::open(config)?.create(draft)
}

/// Replaces the host `alias` of `hosts.conf` by `draft`. A rename moves its
/// metadata, and hosts of `hosts.conf` jumping through it follow; a host of
/// another file jumping through it makes the rename fail
/// ([`ManagedHostError::InUse`]). When the host now logs in to another
/// account, the password of the old one is forgotten (unless another host
/// still uses it): a password never follows a host to another server.
/// Blocking.
pub(crate) fn update(config: &EngineConfig, alias: &str, draft: &HostDraft) -> Result<HostSummary> {
    Editor::open(config)?.update(alias, draft)
}

/// Deletes the host `alias` of `hosts.conf`, its metadata and its
/// remembered password (unless another host still logs in to the same
/// account). Refused while another host jumps through it. Returns the
/// alias as it was declared. Blocking.
pub(crate) fn delete(config: &EngineConfig, alias: &str) -> Result<String> {
    Editor::open(config)?.delete(alias)
}

/// The Portico host `alias` (ignoring case) with what an edit form needs
/// besides its values ([`ManagedHostDetails`]); `None` when `hosts.conf`
/// has no such host. Reads only (no lock, nothing created). Blocking.
pub(crate) fn details(config: &EngineConfig, alias: &str) -> Result<Option<ManagedHostDetails>> {
    Editor::read(config, None)?.details(alias)
}

/// Copies the effective `HostName`, `User`, `Port`, first usable
/// `IdentityFile` and `ProxyJump` of any host into a new host of
/// `hosts.conf` named `new_alias`, with its tags and group. Blocking.
pub(crate) fn duplicate(
    config: &EngineConfig,
    alias: &str,
    new_alias: &str,
) -> Result<HostSummary> {
    Editor::open(config)?.duplicate(alias, new_alias)
}

#[cfg(test)]
mod tests;
