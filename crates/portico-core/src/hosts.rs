//! Host inventory: `~/.ssh/config` aliases enriched with Portico metadata.
//!
//! The ssh config stays the single source of truth for *how* to connect.
//! Portico only adds presentation metadata (tags, group, favorite, notes)
//! in `hosts.toml` (config dir), keyed by alias. `~/.ssh/config` is never
//! written.
//!
//! ## Editing `hosts.toml` (Phase 4)
//!
//! [`update_meta`] is a read-modify-write cycle under an exclusive
//! inter-process lock (`hosts.toml.lock`, see [`crate::store::lock_exclusive`]),
//! done on the TOML *document* (`toml_edit`): only the edited key of the
//! edited alias changes. Other aliases, unknown keys (at any level),
//! comments and layout of a hand-edited file are kept. A field set back to
//! its default is removed, and an alias left with no key at all is removed
//! from the file. The write is atomic (`0600` temp file + rename).
//!
//! Every value is validated and normalized first ([`MetaChange::normalized`]):
//! tags are `[A-Za-z0-9_.-]{1,32}` (a leading `#` is dropped, duplicates are
//! removed ignoring case, at most [`MAX_TAGS`]); a group is one line of at
//! most [`MAX_GROUP_CHARS`] characters; notes are at most
//! [`MAX_NOTES_CHARS`] characters where `\n` is the only control character.
//! Bidi controls and invisible format characters are refused everywhere.
//!
//! The same rules apply when `hosts.toml` is **read** ([`load_meta`]): a
//! hand-edited value that the editor would refuse is dropped (the alias'
//! other fields are kept), and an entry whose alias is invalid is dropped
//! whole. Each drop yields an inventory warning naming the file, the line
//! of the alias, the alias (when valid) and the field, never the value.
//! Normalization (trim, leading `#`, duplicate tags) is applied silently.
//!
//! `last_used` is **deprecated** (replaced by `state/recent.toml`, see
//! [`crate::recent`]): still read (leniently), never written by Portico.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;

use crate::error::Result;
use crate::ssh_config::{
    ConfigWarning, HostAlias, ResolveContext, ResolvedHost, Source, SshConfig,
};
use crate::store;

/// Most tags on one host.
pub const MAX_TAGS: usize = 16;
/// Longest tag, in characters (ASCII only).
pub const MAX_TAG_CHARS: usize = 32;
/// Longest group name, in characters.
pub const MAX_GROUP_CHARS: usize = 64;
/// Longest notes, in characters.
pub const MAX_NOTES_CHARS: usize = 2000;
/// Longest alias accepted as a metadata key, in bytes.
pub const MAX_ALIAS_LEN: usize = 255;
/// Largest `hosts.toml` read, in bytes.
pub const MAX_HOSTS_FILE_BYTES: u64 = 1024 * 1024;

/// Portico-side metadata for one alias.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostMeta {
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    pub notes: Option<String>,
    /// **Deprecated**: recent sessions live in `state/recent.toml`
    /// ([`crate::Engine::recent`]). Read for compatibility (an unparsable
    /// value is ignored), never serialized nor written.
    #[serde(skip_serializing, deserialize_with = "lenient_rfc3339")]
    pub last_used: Option<OffsetDateTime>,
}

/// Accepts an RFC 3339 string; anything else (or nothing) is `None`, so a
/// stale hand-edited `last_used` never makes `hosts.toml` unreadable.
fn lenient_rfc3339<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<OffsetDateTime>, D::Error> {
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| match v {
        toml::Value::String(s) => {
            OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339).ok()
        }
        _ => None,
    }))
}

/// Content of `hosts.toml` (read side; unknown keys are ignored here and
/// preserved by [`update_meta`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostMetaFile {
    pub hosts: BTreeMap<String, HostMeta>,
}

/// Which file declares a host, so the UIs know what they may edit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOrigin {
    /// The user's own `~/.ssh/config` (or a file it includes): read-only
    /// in Portico, which can duplicate it as a Portico host.
    #[default]
    SshConfig,
    /// Portico's `hosts.conf` ([`crate::managed_hosts`]): editable.
    Portico,
    /// The Termius import file ([`crate::import`]): replaced by each import.
    Imported,
}

/// A host as displayed by the UIs.
#[derive(Debug, Clone)]
pub struct HostEntry {
    pub alias: String,
    pub source: Source,
    /// The file `source` belongs to, classified (phase 6b).
    pub origin: HostOrigin,
    pub resolved: ResolvedHost,
    pub meta: HostMeta,
    /// For a [`HostOrigin::Portico`] host created by an import: the tool it
    /// was imported from (`[hosts.<alias>.import]` of `hosts.toml`), still
    /// set once the host was edited. `None` for every other host.
    pub imported_from: Option<crate::import::ImportSource>,
}

impl HostEntry {
    /// Text the fuzzy matcher searches: alias, hostname, user, tags, group.
    pub fn haystack(&self) -> String {
        let mut s = format!("{} {}", self.alias, self.resolved.hostname);
        if let Some(u) = &self.resolved.user {
            s.push(' ');
            s.push_str(u);
        }
        for t in &self.meta.tags {
            s.push_str(" #");
            s.push_str(t);
        }
        if let Some(g) = &self.meta.group {
            s.push(' ');
            s.push_str(g);
        }
        s
    }
}

/// Builds the host list: config aliases (declaration order) + metadata.
/// Every host is marked [`HostOrigin::SshConfig`]; the engine classifies
/// them afterwards (see `crate::managed_hosts::mark_origins`).
pub fn inventory(config: &SshConfig, meta: &HostMetaFile, ctx: &ResolveContext) -> Vec<HostEntry> {
    config
        .host_aliases()
        .into_iter()
        .map(|HostAlias { name, source }| HostEntry {
            resolved: config.resolve(&name, ctx),
            meta: meta.hosts.get(&name).cloned().unwrap_or_default(),
            alias: name,
            source,
            origin: HostOrigin::SshConfig,
            imported_from: None,
        })
        .collect()
}

/// Reads `hosts.toml` (missing file: no metadata) and applies the editing
/// rules to what was read (see the module doc): invalid values are dropped
/// and reported as warnings.
pub(crate) fn load_meta(path: &Path) -> Result<(HostMetaFile, Vec<ConfigWarning>)> {
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok((HostMetaFile::default(), Vec::new()));
    };
    let mut file: HostMetaFile = store::parse_toml(path, &text)?;
    let warnings = sanitize_meta(path, &text, &mut file);
    Ok((file, warnings))
}

/// Normalizes every entry of `file` in place, dropping what the editor
/// would refuse. Returns one warning per drop (no value in the message).
fn sanitize_meta(path: &Path, text: &str, file: &mut HostMetaFile) -> Vec<ConfigWarning> {
    let document = toml_edit::Document::parse(text).ok();
    let line_of = |alias: &str| -> usize {
        let start = document
            .as_ref()
            .and_then(|d| d.get("hosts"))
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|hosts| hosts.get_key_value(alias))
            .and_then(|(key, _)| key.span())
            .map_or(0, |span| span.start);
        let before = text.get(..start).unwrap_or_default();
        before.matches('\n').count().saturating_add(1)
    };
    let mut warnings = Vec::new();
    let mut warn = |alias: &str, message: String| {
        warnings.push(ConfigWarning {
            source: Source {
                file: path.to_path_buf(),
                line: line_of(alias),
            },
            message,
        });
    };

    file.hosts.retain(|alias, meta| {
        if let Err(e) = check_alias(alias) {
            warn(alias, format!("ignored a hosts.toml entry: {e}"));
            return false;
        }
        match normalize_tags(&meta.tags) {
            Ok(tags) => meta.tags = tags,
            Err(e) => {
                warn(alias, format!("ignored `tags` of {alias}: {e}"));
                meta.tags.clear();
            }
        }
        match normalize_group(meta.group.as_deref()) {
            Ok(group) => meta.group = group,
            Err(e) => {
                warn(alias, format!("ignored `group` of {alias}: {e}"));
                meta.group = None;
            }
        }
        match normalize_notes(meta.notes.as_deref()) {
            Ok(notes) => meta.notes = notes,
            Err(e) => {
                warn(alias, format!("ignored `notes` of {alias}: {e}"));
                meta.notes = None;
            }
        }
        true
    });
    warnings
}

// ── Editing ─────────────────────────────────────────────────────────────

/// Invalid metadata. Messages name the field and position, never echo the
/// rejected text (it may hold invisible or bidi characters).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostMetaError {
    #[error("invalid tag {position}: {reason}")]
    Tag {
        /// 1-based position in the submitted list.
        position: usize,
        reason: &'static str,
    },
    #[error("too many tags (at most {max})")]
    TooManyTags { max: usize },
    #[error("invalid group: {reason}")]
    Group { reason: &'static str },
    #[error("invalid notes: {reason}")]
    Notes { reason: &'static str },
    #[error("invalid host alias: {reason}")]
    Alias { reason: &'static str },
}

impl HostMetaError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Tag { .. } => (
                "host_tag",
                Some("Tags use letters, digits, `_`, `.` and `-`, up to 32 characters."),
            ),
            Self::TooManyTags { .. } => ("host_tags", Some("Keep at most 16 tags per host.")),
            Self::Group { .. } => (
                "host_group",
                Some("A group is one line of at most 64 visible characters."),
            ),
            Self::Notes { .. } => (
                "host_notes",
                Some("Notes hold at most 2000 characters of plain text."),
            ),
            Self::Alias { .. } => ("host_alias", None),
        }
    }
}

/// One edit of an alias' metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaChange {
    Favorite(bool),
    Tags(Vec<String>),
    Group(Option<String>),
    Notes(Option<String>),
}

impl MetaChange {
    /// Validates and normalizes the value (see the module doc). Blank
    /// group or notes become `None`.
    pub fn normalized(self) -> std::result::Result<Self, HostMetaError> {
        Ok(match self {
            Self::Favorite(on) => Self::Favorite(on),
            Self::Tags(tags) => Self::Tags(normalize_tags(&tags)?),
            Self::Group(group) => Self::Group(normalize_group(group.as_deref())?),
            Self::Notes(notes) => Self::Notes(normalize_notes(notes.as_deref())?),
        })
    }

    fn apply(&self, meta: &mut HostMeta) {
        match self {
            Self::Favorite(on) => meta.favorite = *on,
            Self::Tags(tags) => meta.tags.clone_from(tags),
            Self::Group(group) => meta.group.clone_from(group),
            Self::Notes(notes) => meta.notes.clone_from(notes),
        }
    }
}

/// Trims, drops one leading `#`, skips blanks, removes duplicates ignoring
/// case (the first spelling wins) and checks each tag.
pub fn normalize_tags(tags: &[String]) -> std::result::Result<Vec<String>, HostMetaError> {
    let mut out: Vec<String> = Vec::new();
    for (index, raw) in tags.iter().enumerate() {
        let position = index.saturating_add(1);
        let trimmed = raw.trim();
        let tag = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
        if tag.is_empty() {
            continue;
        }
        if !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(HostMetaError::Tag {
                position,
                reason: "use only letters, digits, underscore, dot and dash",
            });
        }
        if tag.len() > MAX_TAG_CHARS {
            return Err(HostMetaError::Tag {
                position,
                reason: "longer than 32 characters",
            });
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            out.push(tag.to_owned());
        }
    }
    if out.len() > MAX_TAGS {
        return Err(HostMetaError::TooManyTags { max: MAX_TAGS });
    }
    Ok(out)
}

pub(crate) fn normalize_group(
    group: Option<&str>,
) -> std::result::Result<Option<String>, HostMetaError> {
    let Some(group) = group.map(str::trim).filter(|g| !g.is_empty()) else {
        return Ok(None);
    };
    if group.chars().count() > MAX_GROUP_CHARS {
        return Err(HostMetaError::Group {
            reason: "longer than 64 characters",
        });
    }
    if group
        .chars()
        .any(|c| c.is_control() || crate::snippets::is_hidden_format(c))
    {
        return Err(HostMetaError::Group {
            reason: "contains a control, bidi or invisible character",
        });
    }
    Ok(Some(group.to_owned()))
}

fn normalize_notes(notes: Option<&str>) -> std::result::Result<Option<String>, HostMetaError> {
    let Some(notes) = notes else { return Ok(None) };
    let notes = notes.replace("\r\n", "\n");
    let notes = notes.trim();
    if notes.is_empty() {
        return Ok(None);
    }
    if notes.chars().count() > MAX_NOTES_CHARS {
        return Err(HostMetaError::Notes {
            reason: "longer than 2000 characters",
        });
    }
    if notes
        .chars()
        .any(|c| (c.is_control() && c != '\n') || crate::snippets::is_hidden_format(c))
    {
        return Err(HostMetaError::Notes {
            reason: "contains a control, bidi or invisible character",
        });
    }
    Ok(Some(notes.to_owned()))
}

/// Checks an alias used as a metadata (or recent-sessions) key.
pub(crate) fn check_alias(alias: &str) -> std::result::Result<(), HostMetaError> {
    if alias.is_empty() {
        return Err(HostMetaError::Alias { reason: "empty" });
    }
    if alias.len() > MAX_ALIAS_LEN {
        return Err(HostMetaError::Alias {
            reason: "longer than 255 bytes",
        });
    }
    if alias
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || crate::snippets::is_hidden_format(c))
    {
        return Err(HostMetaError::Alias {
            reason: "contains a space, control, bidi or invisible character",
        });
    }
    Ok(())
}

/// Applies `change` to `alias` in `hosts.toml` (see the module doc) and
/// returns the alias' metadata as the next load will read it. Blocking.
pub(crate) fn update_meta(path: &Path, alias: &str, change: MetaChange) -> Result<HostMeta> {
    replace_meta(path, alias, None, vec![change])
}

/// Applies every change of `changes` to `alias` in one read-modify-write
/// cycle (same lock, document editing and atomic write as
/// [`update_meta`]). With `rename_from`, the entry of that alias (unknown
/// keys and comments included) is first moved to `alias`, replacing any
/// stale entry there. Returns what the next load reads. Blocking.
pub(crate) fn replace_meta(
    path: &Path,
    alias: &str,
    rename_from: Option<&str>,
    changes: Vec<MetaChange>,
) -> Result<HostMeta> {
    check_alias(alias)?;
    if let Some(from) = rename_from {
        check_alias(from)?;
    }
    let changes = changes
        .into_iter()
        .map(MetaChange::normalized)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let text = store::read_capped(path, MAX_HOSTS_FILE_BYTES)?.unwrap_or_default();
    // The typed view must parse too: never write a file `load_hosts` rejects.
    let mut current: HostMetaFile = store::parse_toml(path, &text)?;
    // The returned metadata is what `load_meta` will see: sanitized.
    let _ = sanitize_meta(path, &text, &mut current);
    let mut doc = store::parse_document(path, &text)?;

    let hosts = store::child_table(doc.as_table_mut(), "hosts", path)?;
    let mut meta = current.hosts.get(alias).cloned().unwrap_or_default();
    if let Some(from) = rename_from.filter(|from| *from != alias) {
        hosts.remove(alias);
        if let Some(moved) = hosts.remove(from) {
            hosts.insert(alias, moved);
        }
        meta = current.hosts.get(from).cloned().unwrap_or_default();
    }
    let entry = store::child_table(hosts, alias, path)?;
    entry.set_implicit(false);
    for change in &changes {
        apply_to_table(entry, change);
        change.apply(&mut meta);
    }
    if entry.is_empty() {
        hosts.remove(alias);
    }
    if hosts.is_empty() {
        doc.as_table_mut().remove("hosts");
    }
    store::write_atomic_capped(path, doc.to_string().as_bytes(), MAX_HOSTS_FILE_BYTES)?;
    Ok(meta)
}

/// Writes one (normalized) change into the alias' table.
fn apply_to_table(entry: &mut toml_edit::Table, change: &MetaChange) {
    match change {
        MetaChange::Favorite(true) => {
            entry.insert("favorite", toml_edit::value(true));
        }
        MetaChange::Favorite(false) => {
            entry.remove("favorite");
        }
        MetaChange::Tags(tags) if tags.is_empty() => {
            entry.remove("tags");
        }
        MetaChange::Tags(tags) => {
            let array: toml_edit::Array = tags.iter().map(String::as_str).collect();
            entry.insert("tags", toml_edit::value(array));
        }
        MetaChange::Group(value) | MetaChange::Notes(value) => {
            let key = if matches!(change, MetaChange::Group(_)) {
                "group"
            } else {
                "notes"
            };
            match value {
                Some(v) => {
                    entry.insert(key, toml_edit::value(v.as_str()));
                }
                None => {
                    entry.remove(key);
                }
            }
        }
    }
}

/// Removes the whole entry of `alias` (unknown keys included) from
/// `hosts.toml`; `false` when there was none (nothing is written).
/// Blocking.
pub(crate) fn remove_meta(path: &Path, alias: &str) -> Result<bool> {
    check_alias(alias)?;
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok(false);
    };
    let _: HostMetaFile = store::parse_toml(path, &text)?;
    let mut doc = store::parse_document(path, &text)?;
    let Some(hosts) = doc
        .as_table_mut()
        .get_mut("hosts")
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Ok(false);
    };
    if hosts.remove(alias).is_none() {
        return Ok(false);
    }
    if hosts.is_empty() {
        doc.as_table_mut().remove("hosts");
    }
    store::write_atomic_capped(path, doc.to_string().as_bytes(), MAX_HOSTS_FILE_BYTES)?;
    Ok(true)
}

/// Where an imported host comes from, kept in `hosts.toml` as an `import`
/// table under its alias (`[hosts.<alias>.import]`): the source (`termius`,
/// `termix`), a stable id of the record in the export, and a hash of the
/// values Portico wrote to `hosts.conf` (see
/// `crate::managed_hosts::values_hash`). A re-import only updates a host
/// whose provenance names the same source **and** whose current values
/// still hash to `values`: anything else was created or edited by hand.
/// Not part of [`HostMeta`] (the UIs never see it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportProvenance {
    pub(crate) source: String,
    pub(crate) id: String,
    pub(crate) values: String,
}

/// Longest provenance value read back (longer ones are ignored).
const MAX_PROVENANCE_CHARS: usize = 256;

impl ImportProvenance {
    fn from_item(item: &toml_edit::Item) -> Option<Self> {
        let table = item.as_table_like()?;
        let text = |key: &str| {
            table
                .get(key)
                .and_then(toml_edit::Item::as_str)
                .filter(|v| !v.is_empty() && v.chars().count() <= MAX_PROVENANCE_CHARS)
                .map(str::to_owned)
        };
        Some(Self {
            source: text("source")?,
            id: text("id")?,
            values: text("values")?,
        })
    }

    fn to_table(&self) -> toml_edit::Table {
        let mut table = toml_edit::Table::new();
        table.insert("source", toml_edit::value(self.source.as_str()));
        table.insert("id", toml_edit::value(self.id.as_str()));
        table.insert("values", toml_edit::value(self.values.as_str()));
        table
    }
}

/// The provenance of every imported alias of `hosts.toml` (exact alias
/// spelling). A malformed `import` table is ignored: the host then counts
/// as created by hand, never overwritten. Blocking.
pub(crate) fn load_provenance(
    path: &Path,
) -> Result<std::collections::HashMap<String, ImportProvenance>> {
    let mut out = std::collections::HashMap::new();
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok(out);
    };
    let doc = store::parse_document(path, &text)?;
    let Some(hosts) = doc.get("hosts").and_then(toml_edit::Item::as_table_like) else {
        return Ok(out);
    };
    for (alias, entry) in hosts.iter() {
        let provenance = entry
            .as_table_like()
            .and_then(|t| t.get("import"))
            .and_then(ImportProvenance::from_item);
        if let Some(provenance) = provenance {
            out.insert(alias.to_owned(), provenance);
        }
    }
    Ok(out)
}

/// Forgets the import provenance of `alias` (a host created by hand under
/// an alias an import once used is never taken for an imported one).
/// Nothing is written when there is none. Blocking.
pub(crate) fn remove_provenance(path: &Path, alias: &str) -> Result<()> {
    check_alias(alias)?;
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok(());
    };
    let mut doc = store::parse_document(path, &text)?;
    let removed = doc
        .as_table_mut()
        .get_mut("hosts")
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|hosts| hosts.get_mut(alias))
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|entry| entry.remove("import"))
        .is_some();
    if removed {
        store::write_atomic_capped(path, doc.to_string().as_bytes(), MAX_HOSTS_FILE_BYTES)?;
    }
    Ok(())
}

// ── Index of bound passwords ────────────────────────────────────────────

/// Top-level table of `hosts.toml` listing, per account
/// (`"<user>@<known_hosts name>:<port>"`), the host key fingerprints a
/// remembered password was bound to (`SecretKey::HostPassword`). Not a
/// secret: fingerprints are public. It lets "forget" find a password bound
/// through a host certificate or a `known_hosts` line that has since
/// disappeared.
const BOUND_PASSWORDS: &str = "bound_passwords";
/// Most fingerprints kept per account (the oldest is dropped first).
pub(crate) const MAX_BOUND_FINGERPRINTS: usize = 32;
/// Longest fingerprint recorded (`SHA256:` + 43 base64 characters).
const MAX_FINGERPRINT_CHARS: usize = 128;

/// The index key of an account.
fn account_key(user: &str, host: &str, port: u16) -> String {
    format!("{user}@{host}:{port}")
}

/// Records that the password of `user@host:port` was bound to the host key
/// `fingerprint`. Blocking.
pub(crate) fn record_bound_password(
    path: &Path,
    user: &str,
    host: &str,
    port: u16,
    fingerprint: &str,
) -> Result<()> {
    if fingerprint.is_empty()
        || fingerprint.chars().count() > MAX_FINGERPRINT_CHARS
        || fingerprint.chars().any(char::is_control)
    {
        return Ok(());
    }
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let text = store::read_capped(path, MAX_HOSTS_FILE_BYTES)?.unwrap_or_default();
    let _: HostMetaFile = store::parse_toml(path, &text)?;
    let mut doc = store::parse_document(path, &text)?;
    let index = store::child_table(doc.as_table_mut(), BOUND_PASSWORDS, path)?;
    let key = account_key(user, host, port);
    let mut fingerprints = bound_fingerprints_in(index, &key);
    if fingerprints.iter().any(|f| f == fingerprint) {
        return Ok(());
    }
    fingerprints.push(fingerprint.to_owned());
    let excess = fingerprints.len().saturating_sub(MAX_BOUND_FINGERPRINTS);
    fingerprints.drain(..excess);
    let array: toml_edit::Array = fingerprints.iter().map(String::as_str).collect();
    index.insert(&key, toml_edit::value(array));
    store::write_atomic_capped(path, doc.to_string().as_bytes(), MAX_HOSTS_FILE_BYTES)
}

/// The fingerprints recorded for `user@host:port` (none when the file or
/// the entry is missing or malformed). Blocking.
pub(crate) fn bound_password_fingerprints(
    path: &Path,
    user: &str,
    host: &str,
    port: u16,
) -> Result<Vec<String>> {
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok(Vec::new());
    };
    let doc = store::parse_document(path, &text)?;
    let Some(index) = doc
        .get(BOUND_PASSWORDS)
        .and_then(toml_edit::Item::as_table_like)
    else {
        return Ok(Vec::new());
    };
    Ok(bound_fingerprints_in(index, &account_key(user, host, port)))
}

/// Removes the entry of `user@host:port` from the index (its passwords
/// were forgotten). Nothing is written when there is none. Blocking.
pub(crate) fn remove_bound_passwords(path: &Path, user: &str, host: &str, port: u16) -> Result<()> {
    let _lock = store::lock_exclusive(&store::lock_path(path))?;
    let Some(text) = store::read_capped(path, MAX_HOSTS_FILE_BYTES)? else {
        return Ok(());
    };
    let mut doc = store::parse_document(path, &text)?;
    let Some(index) = doc
        .as_table_mut()
        .get_mut(BOUND_PASSWORDS)
        .and_then(toml_edit::Item::as_table_like_mut)
    else {
        return Ok(());
    };
    if index.remove(&account_key(user, host, port)).is_none() {
        return Ok(());
    }
    if index.is_empty() {
        doc.as_table_mut().remove(BOUND_PASSWORDS);
    }
    store::write_atomic_capped(path, doc.to_string().as_bytes(), MAX_HOSTS_FILE_BYTES)
}

/// The valid fingerprints listed under `key` of the index table.
fn bound_fingerprints_in(index: &dyn toml_edit::TableLike, key: &str) -> Vec<String> {
    index
        .get(key)
        .and_then(toml_edit::Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .filter(|f| !f.is_empty() && f.chars().count() <= MAX_FINGERPRINT_CHARS)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Metadata brought by an import (see [`crate::import`]), merged into
/// what `hosts.toml` already holds for `alias`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedMeta {
    pub(crate) alias: String,
    pub(crate) tags: Vec<String>,
    pub(crate) group: Option<String>,
    pub(crate) notes: Option<String>,
    /// Mark the host as a favorite (set, never cleared: the caller passes
    /// it for the hosts an import creates only).
    pub(crate) favorite: bool,
    /// Replaces the alias' `import` table when given.
    pub(crate) provenance: Option<ImportProvenance>,
}

/// What [`merge_imported_meta`] changed.
#[derive(Debug, Default)]
pub(crate) struct MergeOutcome {
    /// Aliases whose metadata changed, with what the next load reads.
    pub(crate) changed: Vec<(String, HostMeta)>,
    /// Aliases that already had [`MAX_TAGS`] tags: some imported tags were
    /// left out.
    pub(crate) tags_truncated: Vec<String>,
}

/// A merge computed under the `hosts.toml` lock, not written yet: the
/// caller writes its other files, then [`Self::commit`]s (the lock is held
/// until then).
#[derive(Debug)]
pub(crate) struct PreparedMerge {
    _lock: std::fs::File,
    path: std::path::PathBuf,
    /// The new file text; `None` when nothing changes.
    text: Option<String>,
    pub(crate) outcome: MergeOutcome,
}

impl PreparedMerge {
    /// Writes the merged file (nothing when unchanged).
    pub(crate) fn commit(self) -> Result<MergeOutcome> {
        if let Some(text) = &self.text {
            store::write_atomic_capped(&self.path, text.as_bytes(), MAX_HOSTS_FILE_BYTES)?;
        }
        Ok(self.outcome)
    }
}

/// Merges imported metadata in one read-modify-write cycle (same lock,
/// document editing and atomic write as [`update_meta`]): tags are the
/// union of the existing and the imported ones (existing first, at most
/// [`MAX_TAGS`]); a group or notes are only set when the alias has none,
/// a favorite only set (never cleared), so nothing the user wrote is
/// overwritten; the provenance is replaced. Running it twice with the same
/// input changes nothing the second time (and writes nothing). Blocking.
#[cfg(test)]
pub(crate) fn merge_imported_meta(path: &Path, entries: &[ImportedMeta]) -> Result<MergeOutcome> {
    prepare_imported_meta(path, entries)?.commit()
}

/// The merge of [`merge_imported_meta`] without the write: the lock is
/// taken, the new text built and its size checked
/// ([`crate::Error::WriteTooLarge`]), so a later [`PreparedMerge::commit`]
/// only fails on an I/O error.
#[allow(clippy::too_many_lines)] // one linear merge per field; splitting hides the lock scope
pub(crate) fn prepare_imported_meta(
    path: &Path,
    entries: &[ImportedMeta],
) -> Result<PreparedMerge> {
    let lock = store::lock_exclusive(&store::lock_path(path))?;
    let mut prepared = PreparedMerge {
        _lock: lock,
        path: path.to_path_buf(),
        text: None,
        outcome: MergeOutcome::default(),
    };
    if entries.is_empty() {
        return Ok(prepared);
    }
    let outcome = &mut prepared.outcome;
    let text = store::read_capped(path, MAX_HOSTS_FILE_BYTES)?.unwrap_or_default();
    let mut current: HostMetaFile = store::parse_toml(path, &text)?;
    let _ = sanitize_meta(path, &text, &mut current);
    let mut doc = store::parse_document(path, &text)?;
    let mut written = false;

    for entry in entries {
        check_alias(&entry.alias)?;
        let mut meta = current.hosts.get(&entry.alias).cloned().unwrap_or_default();
        let mut changes: Vec<MetaChange> = Vec::new();

        let mut union = meta.tags.clone();
        union.extend(entry.tags.iter().cloned());
        let mut tags = match normalize_tags(&union) {
            Ok(tags) => tags,
            Err(HostMetaError::TooManyTags { .. }) => {
                outcome.tags_truncated.push(entry.alias.clone());
                let mut kept = meta.tags.clone();
                for tag in normalize_tags_lenient(&entry.tags) {
                    if kept.len() >= MAX_TAGS {
                        break;
                    }
                    if !kept.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                        kept.push(tag);
                    }
                }
                kept
            }
            Err(e) => return Err(e.into()),
        };
        tags.truncate(MAX_TAGS);
        if tags != meta.tags {
            changes.push(MetaChange::Tags(tags));
        }
        if meta.group.is_none() {
            let group = normalize_group(entry.group.as_deref())?;
            if group.is_some() {
                changes.push(MetaChange::Group(group));
            }
        }
        if meta.notes.is_none() {
            let notes = normalize_notes(entry.notes.as_deref())?;
            if notes.is_some() {
                changes.push(MetaChange::Notes(notes));
            }
        }
        if entry.favorite && !meta.favorite {
            changes.push(MetaChange::Favorite(true));
        }
        let stored = doc
            .get("hosts")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|h| h.get(&entry.alias))
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|t| t.get("import"))
            .and_then(ImportProvenance::from_item);
        let provenance = entry
            .provenance
            .as_ref()
            .filter(|wanted| stored.as_ref() != Some(*wanted));
        if changes.is_empty() && provenance.is_none() {
            continue;
        }
        let hosts = store::child_table(doc.as_table_mut(), "hosts", path)?;
        let table = store::child_table(hosts, &entry.alias, path)?;
        table.set_implicit(false);
        for change in &changes {
            match change {
                MetaChange::Tags(tags) => {
                    let array: toml_edit::Array = tags.iter().map(String::as_str).collect();
                    table.insert("tags", toml_edit::value(array));
                }
                MetaChange::Group(Some(group)) => {
                    table.insert("group", toml_edit::value(group.as_str()));
                }
                MetaChange::Notes(Some(notes)) => {
                    table.insert("notes", toml_edit::value(notes.as_str()));
                }
                MetaChange::Favorite(true) => {
                    table.insert("favorite", toml_edit::value(true));
                }
                MetaChange::Group(None) | MetaChange::Notes(None) | MetaChange::Favorite(false) => {
                }
            }
            change.apply(&mut meta);
        }
        if let Some(provenance) = provenance {
            table.insert("import", toml_edit::Item::Table(provenance.to_table()));
        }
        written = true;
        if !changes.is_empty() {
            current.hosts.insert(entry.alias.clone(), meta.clone());
            outcome.changed.push((entry.alias.clone(), meta));
        }
    }
    if written {
        let text = doc.to_string();
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_HOSTS_FILE_BYTES {
            return Err(crate::Error::WriteTooLarge {
                path: path.to_path_buf(),
                max: MAX_HOSTS_FILE_BYTES,
            });
        }
        prepared.text = Some(text);
    }
    Ok(prepared)
}

/// The valid tags of `tags`, normalized one by one (invalid ones skipped).
fn normalize_tags_lenient(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter_map(|t| normalize_tags(std::slice::from_ref(t)).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn tags(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn imported_metadata_is_merged_without_overwriting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        let full: Vec<String> = (0..MAX_TAGS).map(|i| format!("t{i}")).collect();
        let mut doc = String::from("[hosts.full]\ntags = [");
        doc.push_str(
            &full
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", "),
        );
        doc.push_str("]\n[hosts.mine]\ngroup = \"Mine\"\nnotes = \"keep\"\n");
        std::fs::write(&path, &doc).expect("write");
        let entry = |alias: &str, tags: &[&str]| ImportedMeta {
            alias: alias.to_owned(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            group: Some("Prod/EU".to_owned()),
            notes: Some("Termius: x".to_owned()),
            favorite: false,
            provenance: None,
        };
        let entries = [
            entry("full", &["T1", "new"]),
            entry("mine", &["a"]),
            entry("fresh", &["b", "B"]),
        ];
        let outcome = merge_imported_meta(&path, &entries).expect("merge");
        assert_eq!(outcome.tags_truncated, ["full"]);
        assert_eq!(outcome.changed.len(), 3);
        let (file, _) = load_meta(&path).expect("load");
        let full_meta = file.hosts.get("full").expect("full");
        assert_eq!(full_meta.tags, full, "already at the limit");
        assert_eq!(full_meta.group.as_deref(), Some("Prod/EU"));
        let mine = file.hosts.get("mine").expect("mine");
        assert_eq!(
            (mine.group.as_deref(), mine.notes.as_deref()),
            (Some("Mine"), Some("keep"))
        );
        assert_eq!(mine.tags, ["a"]);
        assert_eq!(file.hosts.get("fresh").expect("fresh").tags, ["b"]);

        // Same input again: nothing changes, nothing is written.
        let before = std::fs::read(&path).expect("read");
        let outcome = merge_imported_meta(&path, &entries).expect("merge");
        assert!(outcome.changed.is_empty());
        assert_eq!(std::fs::read(&path).expect("read"), before);
    }

    #[test]
    fn tags_are_normalized() {
        assert_eq!(
            normalize_tags(&tags(&[
                " #Prod ",
                "eu-west.1",
                "prod",
                "",
                "#",
                "PROD",
                "db_1"
            ]))
            .expect("valid"),
            ["Prod", "eu-west.1", "db_1"]
        );
    }

    #[test]
    fn invalid_tags_are_refused_with_their_position() {
        let long = "z".repeat(33);
        for (bad, position) in [
            (vec!["ok", "has space"], 2),
            (vec!["été"], 1),
            (vec!["a\u{202e}b"], 1),
            (vec!["x", "y", long.as_str()], 3),
        ] {
            let bad: Vec<String> = bad.iter().map(|s| (*s).to_owned()).collect();
            let err = normalize_tags(&bad).expect_err("invalid");
            assert!(
                matches!(err, HostMetaError::Tag { position: p, .. } if p == position),
                "{err:?}"
            );
        }
        assert!(normalize_tags(&tags(&[&"z".repeat(32)])).is_ok());
        let many: Vec<String> = (0..17).map(|i| format!("t{i}")).collect();
        assert_eq!(
            normalize_tags(&many),
            Err(HostMetaError::TooManyTags { max: MAX_TAGS })
        );
        // Duplicates do not count towards the limit.
        let mut sixteen: Vec<String> = (0..16).map(|i| format!("t{i}")).collect();
        sixteen.push("T0".into());
        assert_eq!(normalize_tags(&sixteen).expect("valid").len(), 16);
    }

    #[test]
    fn group_and_notes_are_checked() {
        assert_eq!(
            normalize_group(Some("  Prod EU ")),
            Ok(Some("Prod EU".into()))
        );
        assert_eq!(normalize_group(Some("   ")), Ok(None));
        assert!(normalize_group(Some(&"g".repeat(65))).is_err());
        assert!(normalize_group(Some(&"é".repeat(64))).is_ok());
        assert!(normalize_group(Some("a\nb")).is_err());
        assert!(normalize_group(Some("a\u{2067}b")).is_err());

        assert_eq!(
            normalize_notes(Some("\r\nline 1\r\nline 2\n")),
            Ok(Some("line 1\nline 2".into()))
        );
        assert_eq!(normalize_notes(Some(" \n ")), Ok(None));
        assert!(normalize_notes(Some(&"n".repeat(2001))).is_err());
        assert!(normalize_notes(Some(&"n".repeat(2000))).is_ok());
        assert!(normalize_notes(Some("bell\u{7}")).is_err());
        assert!(normalize_notes(Some("tab\there")).is_err());
        assert!(normalize_notes(Some("zero\u{200b}width")).is_err());
    }

    #[test]
    fn aliases_are_checked() {
        assert!(check_alias("web-01.example.com").is_ok());
        assert!(check_alias("").is_err());
        assert!(check_alias("a b").is_err());
        assert!(check_alias(&"a".repeat(256)).is_err());
    }

    const HAND_EDITED: &str = r#"# My hosts, edited by hand.
version = 7

[hosts.prod]
tags = ["prod"] # keep me
color = "red"
last_used = "2026-01-02T03:04:05Z"

[hosts."web.example.com"]
favorite = true

[hosts.dev]
favorite = true
when = 2026-05-04T10:00:00Z
"#;

    #[test]
    fn edits_keep_unknown_keys_comments_and_other_aliases() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, HAND_EDITED).expect("write");

        let meta = update_meta(&path, "prod", MetaChange::Favorite(true)).expect("edit");
        assert!(meta.favorite);
        assert_eq!(meta.tags, ["prod"]);
        let meta = update_meta(
            &path,
            "prod",
            MetaChange::Tags(tags(&["#Prod", "eu", "PROD"])),
        )
        .expect("edit");
        assert_eq!(meta.tags, ["Prod", "eu"]);
        update_meta(&path, "dev", MetaChange::Favorite(false)).expect("edit");

        let text = std::fs::read_to_string(&path).expect("read");
        for kept in [
            "# My hosts, edited by hand.",
            "version = 7",
            "color = \"red\"",
            "last_used = \"2026-01-02T03:04:05Z\"",
            "when = 2026-05-04T10:00:00Z",
            "[hosts.\"web.example.com\"]",
        ] {
            assert!(text.contains(kept), "lost {kept:?} in:\n{text}");
        }
        let file: HostMetaFile = toml::from_str(&text).expect("parse");
        let prod = file.hosts.get("prod").expect("prod");
        assert!(prod.favorite);
        assert_eq!(prod.tags, ["Prod", "eu"]);
        assert!(prod.last_used.is_some(), "deprecated field still read");
        assert!(file.hosts.get("web.example.com").expect("web").favorite);
        // `dev` keeps its unknown key, so it stays (without `favorite`).
        assert!(!file.hosts.get("dev").expect("dev").favorite);
        assert!(!text.contains("[hosts.dev]\nfavorite"), "{text}");
    }

    #[test]
    fn emptied_entries_are_removed_and_files_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config/hosts.toml");
        let meta = update_meta(&path, "db", MetaChange::Group(Some(" Data ".into()))).expect("new");
        assert_eq!(meta.group.as_deref(), Some("Data"));
        update_meta(
            &path,
            "db",
            MetaChange::Notes(Some("line 1\nline 2".into())),
        )
        .expect("notes");
        update_meta(&path, "api", MetaChange::Favorite(true)).expect("api");
        let (file, _) = load_meta(&path).expect("load");
        assert_eq!(
            file.hosts.get("db").expect("db").notes.as_deref(),
            Some("line 1\nline 2")
        );

        update_meta(&path, "db", MetaChange::Group(None)).expect("clear");
        update_meta(&path, "db", MetaChange::Notes(Some("   ".into()))).expect("clear");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(!text.contains("db"), "{text}");
        update_meta(&path, "api", MetaChange::Tags(Vec::new())).expect("noop");
        update_meta(&path, "api", MetaChange::Favorite(false)).expect("clear");
        assert_eq!(std::fs::read_to_string(&path).expect("read").trim(), "");
        assert!(store::lock_path(&path).exists(), "lock file left in place");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn invalid_edits_leave_the_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, HAND_EDITED).expect("write");
        let err =
            update_meta(&path, "prod", MetaChange::Tags(tags(&["bad tag"]))).expect_err("bad");
        assert_eq!(err.summary().code, "host_tag");
        let err = update_meta(&path, "", MetaChange::Favorite(true)).expect_err("bad");
        assert_eq!(err.summary().code, "host_alias");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), HAND_EDITED);

        // A file the reader rejects is never rewritten.
        std::fs::write(&path, "[hosts.prod]\ntags = \"not a list\"\n").expect("write");
        let err = update_meta(&path, "prod", MetaChange::Favorite(true)).expect_err("bad");
        assert_eq!(err.summary().code, "serde");
        std::fs::write(&path, "hosts = 3\n").expect("write");
        assert!(update_meta(&path, "prod", MetaChange::Favorite(true)).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "hosts = 3\n");
    }

    #[test]
    fn inline_tables_are_edited_in_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, "hosts = { prod = { tags = [\"a\"], x = 1 } }\n").expect("write");
        update_meta(&path, "prod", MetaChange::Favorite(true)).expect("edit");
        let (file, _) = load_meta(&path).expect("load");
        let prod = file.hosts.get("prod").expect("prod");
        assert!(prod.favorite);
        assert_eq!(prod.tags, ["a"]);
        assert!(
            std::fs::read_to_string(&path)
                .expect("read")
                .contains("x = 1")
        );
    }

    #[test]
    fn concurrent_edits_are_all_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    update_meta(&path, &format!("h{i}"), MetaChange::Favorite(true)).expect("edit");
                    update_meta(&path, "shared", MetaChange::Tags(vec![format!("t{i}")]))
                        .expect("edit");
                })
            })
            .collect();
        for t in threads {
            t.join().expect("thread");
        }
        let (file, _) = load_meta(&path).expect("load");
        for i in 0..8 {
            assert!(file.hosts.get(&format!("h{i}")).expect("alias").favorite);
        }
        assert_eq!(file.hosts.get("shared").expect("shared").tags.len(), 1);
    }

    #[test]
    fn last_used_is_read_leniently_and_never_serialized() {
        let file: HostMetaFile =
            toml::from_str("[hosts.a]\nlast_used = 12\n[hosts.b]\nlast_used = \"garbage\"\n")
                .expect("lenient");
        assert!(file.hosts.values().all(|m| m.last_used.is_none()));
        let meta = HostMeta {
            last_used: Some(OffsetDateTime::UNIX_EPOCH),
            ..HostMeta::default()
        };
        let json = serde_json::to_string(&meta).expect("json");
        assert!(!json.contains("last_used"), "{json}");
    }

    #[test]
    fn hand_edited_invalid_meta_is_dropped_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        let long_notes = "n".repeat(MAX_NOTES_CHARS + 1);
        std::fs::write(
            &path,
            format!(
                "[hosts.good]\ntags = [\"#Ops\", \"ops\", \" eu \"]\ngroup = \" Team \"\nfavorite = true\n\n\
                 [hosts.web]\ntags = [\"ok\", \"bad tag SECRET1\"]\ngroup = \"a\\u202eSECRET2\"\nnotes = \"{long_notes}\"\nfavorite = true\n\n\
                 [hosts.\"bad alias SECRET3\"]\nfavorite = true\n\n\
                 [hosts.db]\nnotes = \"bell \\u0007 SECRET4\"\ntags = [\"fine\"]\n"
            ),
        )
        .expect("write");
        let (file, warnings) = load_meta(&path).expect("load");

        // Valid values are normalized like an edit would.
        let good = file.hosts.get("good").expect("good");
        assert_eq!(good.tags, ["Ops", "eu"]);
        assert_eq!(good.group.as_deref(), Some("Team"));
        assert!(good.favorite);
        // Invalid fields are dropped, the rest of the alias is kept.
        let web = file.hosts.get("web").expect("web");
        assert!(web.tags.is_empty());
        assert_eq!(web.group, None);
        assert_eq!(web.notes, None);
        assert!(web.favorite);
        let db = file.hosts.get("db").expect("db");
        assert_eq!(db.notes, None);
        assert_eq!(db.tags, ["fine"]);
        // An invalid alias drops the whole entry.
        assert_eq!(file.hosts.len(), 3, "{:?}", file.hosts.keys());

        let described: Vec<(usize, &str)> = warnings
            .iter()
            .map(|w| (w.source.line, w.message.as_str()))
            .collect();
        assert_eq!(warnings.len(), 5, "{described:?}");
        assert!(warnings.iter().all(|w| w.source.file == path));
        for w in &warnings {
            assert!(!w.message.contains("SECRET"), "value echoed: {w:?}");
            assert!(!w.message.contains('\u{202e}'), "{w:?}");
        }
        let has = |line: usize, needle: &str| {
            warnings
                .iter()
                .any(|w| w.source.line == line && w.message.contains(needle))
        };
        assert!(has(6, "`tags` of web"), "{described:?}");
        assert!(has(6, "`group` of web"), "{described:?}");
        assert!(has(6, "`notes` of web"), "{described:?}");
        assert!(has(12, "invalid host alias"), "{described:?}");
        assert!(has(15, "`notes` of db"), "{described:?}");

        // An edit returns what the next load reads (sanitized).
        let meta = update_meta(&path, "web", MetaChange::Favorite(false)).expect("edit");
        assert_eq!(
            &meta,
            file.hosts
                .get("web")
                .map(|m| HostMeta {
                    favorite: false,
                    ..m.clone()
                })
                .as_ref()
                .expect("web")
        );
    }

    #[test]
    fn edit_never_writes_a_file_the_reader_rejects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hosts.toml");
        // A hand-edited file just under the read limit.
        let filler = usize::try_from(MAX_HOSTS_FILE_BYTES).expect("usize") - 100;
        let original = format!("#{}\n[hosts.a]\nfavorite = true\n", "x".repeat(filler));
        std::fs::write(&path, &original).expect("write");
        assert!(load_meta(&path).is_ok());

        let err = update_meta(&path, "a", MetaChange::Notes(Some("n".repeat(500))))
            .expect_err("too large");
        let summary = err.summary();
        assert_eq!(summary.code, "file_too_large");
        assert!(summary.hint.is_some());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);
        assert!(load_meta(&path).is_ok());
        // A smaller change still fits.
        update_meta(&path, "a", MetaChange::Group(Some("g".into()))).expect("fits");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_or_special_files_are_never_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("dotfiles-hosts.toml");
        std::fs::write(&target, "[hosts.a]\nfavorite = true\n").expect("write");
        let path = dir.path().join("hosts.toml");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");

        // Reading through a link to a regular file is fine.
        let (file, _) = load_meta(&path).expect("load");
        assert!(file.hosts.get("a").expect("a").favorite);
        // Writing would replace the link: refused, link and target intact.
        let err = update_meta(&path, "a", MetaChange::Favorite(false)).expect_err("symlink");
        let summary = err.summary();
        assert_eq!(summary.code, "not_regular_file");
        assert!(
            summary
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("symlink")),
            "{summary:?}"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&target)
                .expect("read")
                .contains("favorite = true")
        );

        // A FIFO is refused on read without blocking.
        let fifo = dir.path().join("fifo-hosts.toml");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success());
        if made {
            let err = load_meta(&fifo).expect_err("fifo");
            assert_eq!(err.summary().code, "not_regular_file");
            let err = update_meta(&fifo, "a", MetaChange::Favorite(true)).expect_err("fifo");
            assert_eq!(err.summary().code, "not_regular_file");
        }
        // A directory too.
        let err = load_meta(dir.path()).expect_err("dir");
        assert_eq!(err.summary().code, "not_regular_file");
    }
}
