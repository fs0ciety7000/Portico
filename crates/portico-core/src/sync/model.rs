//! The synced state and its merge (S1a).
//!
//! A [`SyncState`] is a global causal context ([`VersionVector`]) plus one
//! multi-value register per [`RecordKey`]:
//!
//! | Key | Value | Granularity |
//! |---|---|---|
//! | `Host { alias }` | [`HostValue`] (connection tuple) | whole tuple |
//! | `HostMeta { alias, field }` | tags / group / favorite / notes | per field |
//! | `Snippet { id }` | [`SnippetValue`] | whole snippet |
//! | `Setting { key }` | text (whitelist [`SettingKey`]) | per key, per keymap action |
//! | `Secret { account }` | sealed blob, **reserved for S2** | whole record |
//!
//! Semantics (`docs/SYNC.md` §4, security review F6, F15, F18):
//!
//! - **Deletion** is a [`RecordValue::Tombstone`] written with a new dot, so
//!   a deletion concurrent with an edit is a visible conflict. A cleared
//!   metadata field is a tombstone too.
//! - **Global context, never collected** (F6): a record absent from one
//!   state whose dots that state's context covers was deleted (and its
//!   tombstone collected) there. The merge drops it instead of
//!   resurrecting it, even when it comes back from an old snapshot.
//! - **Conflicts** (more than one distinct value) are never resolved
//!   automatically; [`SyncState::resolve`] is an explicit user decision
//!   written as a new dot. Concurrent tag edits are conflicts too, with
//!   [`Resolution::Union`] as an offered choice (owner decision #10).
//! - **Tombstone collection** only drops a register whose every value is a
//!   tombstone covered by the causally stable context ([`SyncState::collect_tombstones`]).
//! - **Opaque passthrough** (F15): a record this version does not
//!   understand (unknown kind, unknown setting key, unknown value shape) is
//!   kept verbatim and re-published, up to [`MAX_OPAQUE_BYTES`] in total;
//!   it is never applied locally.
//! - **Settings whitelist** (F18): `theme`, `dark_theme`, `keymap.tui.*`,
//!   `keymap.gui.*`. `gui.copy_on_select` and `updates.check` (a network
//!   consent) are never synced.
//!
//! Values are checked structurally here ([`validate_record`]: lengths,
//! character classes, home-relative identity paths F10). The semantic
//! checks against the local files (shadowed aliases, `ProxyJump`, snippet
//! safety rules) run at apply time (S1b) with the existing validators.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::SyncError;
use super::clock::{Dot, Entry, LocalClock, MvRegister, VersionVector};

/// Most records in one state (hosts + metadata fields + snippets + settings).
pub(crate) const MAX_RECORDS: usize = 20_000;
/// Most surviving dots in one register (concurrent writers of one key).
pub(crate) const MAX_ENTRIES_PER_REGISTER: usize = 64;
/// Most devices in a context.
pub(crate) const MAX_CONTEXT_DEVICES: usize = 1024;
/// Total size of opaque (not understood) records kept for passthrough.
pub(crate) const MAX_OPAQUE_BYTES: usize = 256 * 1024;

const MAX_HOSTNAME_CHARS: usize = 255;
const MAX_USER_CHARS: usize = 255;
const MAX_SETTING_ACTION_CHARS: usize = crate::settings::MAX_SETTING_CHARS;

/// Field of a host's metadata synced as its own register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MetaField {
    Tags,
    Group,
    Favorite,
    Notes,
}

/// Whitelisted synced setting (F18). Serialized as its `settings.toml`
/// path: `theme`, `dark_theme`, `keymap.tui.<action>`, `keymap.gui.<action>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum SettingKey {
    Theme,
    DarkTheme,
    KeymapTui(String),
    KeymapGui(String),
}

impl SettingKey {
    /// `None` for every key outside the whitelist, including
    /// `gui.copy_on_select` and `updates.check`.
    pub(crate) fn parse(path: &str) -> Option<Self> {
        let action_ok = |action: &str| {
            !action.is_empty()
                && action.chars().count() <= MAX_SETTING_ACTION_CHARS
                && !action.chars().any(forbidden_char)
        };
        match path {
            "theme" => Some(Self::Theme),
            "dark_theme" => Some(Self::DarkTheme),
            _ => {
                if let Some(action) = path.strip_prefix("keymap.tui.") {
                    action_ok(action).then(|| Self::KeymapTui(action.to_owned()))
                } else if let Some(action) = path.strip_prefix("keymap.gui.") {
                    action_ok(action).then(|| Self::KeymapGui(action.to_owned()))
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn path(&self) -> String {
        match self {
            Self::Theme => "theme".to_owned(),
            Self::DarkTheme => "dark_theme".to_owned(),
            Self::KeymapTui(action) => format!("keymap.tui.{action}"),
            Self::KeymapGui(action) => format!("keymap.gui.{action}"),
        }
    }
}

impl Serialize for SettingKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.path())
    }
}

impl<'de> Deserialize<'de> for SettingKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&path).ok_or_else(|| D::Error::custom("setting not synced"))
    }
}

/// What a register is about. Strings are the natural keys of the local
/// files; they only ever appear inside ciphertext.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RecordKey {
    /// A host of `hosts.conf`, by alias.
    Host { alias: String },
    /// One metadata field of `hosts.toml`, for any alias (including the
    /// user's own `~/.ssh/config` hosts, owner decision #7).
    HostMeta { alias: String, field: MetaField },
    /// A snippet of `snippets.toml`, by slug.
    Snippet { id: String },
    /// A whitelisted setting.
    Setting { key: SettingKey },
    /// A remembered password (S2, bound passwords only, F3). S1 never
    /// writes one; received ones merge like any record and are never
    /// applied by S1.
    Secret { account: String },
}

impl RecordKey {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Host { .. } => "host",
            Self::HostMeta { .. } => "host_meta",
            Self::Snippet { .. } => "snippet",
            Self::Setting { .. } => "setting",
            Self::Secret { .. } => "secret",
        }
    }
}

/// A `~/…` identity path with `/` separators and only normal components
/// (F10): no `..`, `.`, root, drive, `:`, `\`, UNC or control character.
/// Portable between `/home/a`, `/Users/a` and `C:\Users\a`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct HomeRelPath(String);

impl HomeRelPath {
    pub(crate) fn parse(text: &str) -> Result<Self, SyncError> {
        let invalid = |reason| SyncError::InvalidRecord {
            kind: "host",
            field: "identity_file",
            reason,
        };
        if text.len() > crate::managed_hosts::MAX_IDENTITY_PATH_BYTES {
            return Err(invalid("is too long"));
        }
        let rest = text
            .strip_prefix("~/")
            .ok_or_else(|| invalid("must start with ~/"))?;
        if rest.is_empty() {
            return Err(invalid("names no file"));
        }
        for component in rest.split('/') {
            match component {
                "" => return Err(invalid("has an empty component")),
                "." | ".." => return Err(invalid("has a . or .. component")),
                _ if component.contains([':', '\\']) => {
                    return Err(invalid("contains : or \\"));
                }
                _ if component.chars().any(forbidden_char) => {
                    return Err(invalid("contains a control or invisible character"));
                }
                _ => {}
            }
        }
        Ok(Self(text.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for HomeRelPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&text).map_err(|_| D::Error::custom("invalid identity path"))
    }
}

/// The connection tuple of a Portico host (one register: a conflict on two
/// fields is acceptable and safer for integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostValue {
    pub(crate) hostname: String,
    #[serde(default)]
    pub(crate) user: Option<String>,
    #[serde(default)]
    pub(crate) port: Option<u16>,
    #[serde(default)]
    pub(crate) identity_file: Option<HomeRelPath>,
    #[serde(default)]
    pub(crate) proxy_jump: Option<String>,
    /// Set on the new record of a rename (`docs/SYNC.md` §4.5).
    #[serde(default)]
    pub(crate) renamed_from: Option<String>,
}

/// Scope of a synced snippet (mirror of `snippets::SnippetScope`; the wire
/// format does not follow internal renames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SnippetScopeValue {
    Global,
    Hosts(Vec<String>),
    Tags(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnippetValue {
    pub(crate) name: String,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    pub(crate) scope: SnippetScopeValue,
    pub(crate) run: bool,
}

/// A sealed secret (S2: inner AEAD under the secret subkey, plus the
/// comparison HMAC F11). Opaque bytes, base64 on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SealedValue {
    pub(crate) nonce: String,
    pub(crate) ciphertext: String,
    pub(crate) compare: String,
}

/// Value of a register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "v",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum RecordValue {
    /// Deleted (or, for a metadata field, cleared).
    Tombstone,
    Host(HostValue),
    Tags(BTreeSet<String>),
    Text(String),
    Bool(bool),
    Snippet(SnippetValue),
    Sealed(SealedValue),
}

impl RecordValue {
    pub(crate) fn is_tombstone(&self) -> bool {
        matches!(self, Self::Tombstone)
    }
}

fn forbidden_char(c: char) -> bool {
    c.is_control() || crate::snippets::is_hidden_format(c)
}

/// Structural check of a record before it is written locally or applied:
/// the value's type matches the key, lengths and character classes are
/// bounded. Messages name the kind and the field, never the value.
// One arm per (key, value) kind: splitting it would scatter the rules.
#[allow(clippy::too_many_lines)]
pub(crate) fn validate_record(key: &RecordKey, value: &RecordValue) -> Result<(), SyncError> {
    let kind = key.kind();
    let invalid = |field, reason| SyncError::InvalidRecord {
        kind,
        field,
        reason,
    };
    let text = |field, value: &str, max_chars: usize, allow_newlines: bool| {
        if value.chars().count() > max_chars {
            return Err(invalid(field, "is too long"));
        }
        let bad = value
            .chars()
            .any(|c| !(allow_newlines && matches!(c, '\n' | '\t')) && forbidden_char(c));
        if bad {
            return Err(invalid(field, "contains a control or invisible character"));
        }
        Ok(())
    };
    let name = |field, value: &str, max_chars: usize| {
        if value.trim().is_empty() {
            return Err(invalid(field, "is empty"));
        }
        if value.chars().any(char::is_whitespace) {
            return Err(invalid(field, "contains whitespace"));
        }
        text(field, value, max_chars, false)
    };
    match key {
        RecordKey::Host { alias } | RecordKey::HostMeta { alias, .. } => {
            name("alias", alias, crate::hosts::MAX_ALIAS_LEN)?;
        }
        RecordKey::Snippet { id } => {
            let slug = !id.is_empty()
                && id.len() <= crate::snippets::MAX_ID_LEN
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !slug {
                return Err(invalid("id", "is not a slug ([a-z0-9-])"));
            }
        }
        RecordKey::Setting { key } => {
            // Constructed through the whitelist; re-checked for literals.
            if SettingKey::parse(&key.path()).as_ref() != Some(key) {
                return Err(invalid("key", "is not a synced setting"));
            }
        }
        RecordKey::Secret { .. } => {
            return Err(invalid("account", "passwords are not synced before S2"));
        }
    }
    if value.is_tombstone() {
        return Ok(());
    }
    match (key, value) {
        (RecordKey::Host { .. }, RecordValue::Host(host)) => {
            name("hostname", &host.hostname, MAX_HOSTNAME_CHARS)?;
            if let Some(user) = &host.user {
                name("user", user, MAX_USER_CHARS)?;
            }
            if host.port == Some(0) {
                return Err(invalid("port", "is 0"));
            }
            if let Some(path) = &host.identity_file {
                HomeRelPath::parse(path.as_str())?;
            }
            for (field, alias) in [
                ("proxy_jump", &host.proxy_jump),
                ("renamed_from", &host.renamed_from),
            ] {
                if let Some(alias) = alias {
                    name(field, alias, crate::hosts::MAX_ALIAS_LEN)?;
                }
            }
            Ok(())
        }
        (
            RecordKey::HostMeta {
                field: MetaField::Tags,
                ..
            },
            RecordValue::Tags(tags),
        ) => {
            if tags.len() > crate::hosts::MAX_TAGS {
                return Err(invalid("tags", "has too many tags"));
            }
            tags.iter()
                .try_for_each(|tag| name("tags", tag, crate::hosts::MAX_TAG_CHARS))
        }
        (
            RecordKey::HostMeta {
                field: MetaField::Group,
                ..
            },
            RecordValue::Text(group),
        ) => text("group", group, crate::hosts::MAX_GROUP_CHARS, false),
        (
            RecordKey::HostMeta {
                field: MetaField::Notes,
                ..
            },
            RecordValue::Text(notes),
        ) => text("notes", notes, crate::hosts::MAX_NOTES_CHARS, true),
        (
            RecordKey::HostMeta {
                field: MetaField::Favorite,
                ..
            },
            RecordValue::Bool(_),
        ) => Ok(()),
        (RecordKey::Snippet { .. }, RecordValue::Snippet(snippet)) => {
            if snippet.name.trim().is_empty() {
                return Err(invalid("name", "is empty"));
            }
            text(
                "name",
                &snippet.name,
                crate::snippets::MAX_NAME_CHARS,
                false,
            )?;
            if snippet.command.len() > crate::snippets::MAX_COMMAND_BYTES {
                return Err(invalid("command", "is too long"));
            }
            text("command", &snippet.command, usize::MAX, true)?;
            if let Some(description) = &snippet.description {
                text(
                    "description",
                    description,
                    crate::snippets::MAX_DESCRIPTION_CHARS,
                    true,
                )?;
            }
            match &snippet.scope {
                SnippetScopeValue::Global => Ok(()),
                SnippetScopeValue::Hosts(entries) | SnippetScopeValue::Tags(entries) => {
                    if entries.len() > crate::snippets::MAX_SCOPE_ENTRIES {
                        return Err(invalid("scope", "has too many entries"));
                    }
                    entries
                        .iter()
                        .try_for_each(|entry| name("scope", entry, crate::hosts::MAX_ALIAS_LEN))
                }
            }
        }
        (RecordKey::Setting { .. }, RecordValue::Text(setting)) => {
            if setting.trim().is_empty() {
                return Err(invalid("value", "is empty"));
            }
            text("value", setting, crate::settings::MAX_SETTING_CHARS, false)
        }
        _ => Err(invalid("value", "does not match the record kind")),
    }
}

/// A user decision on a conflict, written as a new dot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// Keep the sibling at this index of [`MvRegister::siblings`].
    Pick(usize),
    /// Tags only: the union of every sibling's tags (tombstones count as
    /// no tag). Offered, preselected, never applied automatically.
    Union,
}

/// One unresolved conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict<'a> {
    pub(crate) key: &'a RecordKey,
    pub(crate) siblings: Vec<super::clock::Sibling<'a, RecordValue>>,
}

/// What a merge could not keep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MergeReport {
    /// Opaque records left out because [`MAX_OPAQUE_BYTES`] was reached.
    pub(crate) opaque_dropped: usize,
    /// Dots received with a value different from the local one for the
    /// same dot: a misbehaving device holding the key (alert, S1b).
    pub(crate) divergent_dots: usize,
}

/// The synced state: global context, registers, opaque passthrough.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SyncState {
    context: VersionVector,
    records: BTreeMap<RecordKey, MvRegister<RecordValue>>,
    /// Raw JSON of records this version does not understand.
    opaque: BTreeSet<String>,
}

impl SyncState {
    pub(crate) fn context(&self) -> &VersionVector {
        &self.context
    }

    pub(crate) fn records(&self) -> impl Iterator<Item = (&RecordKey, &MvRegister<RecordValue>)> {
        self.records.iter()
    }

    pub(crate) fn get(&self, key: &RecordKey) -> Option<&MvRegister<RecordValue>> {
        self.records.get(key)
    }

    pub(crate) fn opaque_len(&self) -> usize {
        self.opaque.len()
    }

    /// "Purge" action of the UI (F15): forget every opaque record.
    pub(crate) fn purge_opaque(&mut self) {
        self.opaque.clear();
    }

    /// Joins `other` into `self` (commutative, associative, idempotent on
    /// registers and contexts).
    pub(crate) fn merge(&mut self, other: &Self) -> MergeReport {
        let empty = MvRegister::default();
        let mut divergent_dots: usize = 0;
        let keys: BTreeSet<RecordKey> = self
            .records
            .keys()
            .chain(other.records.keys())
            .cloned()
            .collect();
        for key in keys {
            let theirs = other.records.get(&key).unwrap_or(&empty);
            let ours = self.records.entry(key).or_default();
            divergent_dots =
                divergent_dots.saturating_add(ours.merge(&self.context, theirs, &other.context));
        }
        self.records.retain(|_, register| !register.is_empty());
        self.context.join(&other.context);
        let mut report = MergeReport {
            divergent_dots,
            ..MergeReport::default()
        };
        let mut opaque_bytes: usize = self.opaque.iter().map(String::len).sum();
        for raw in &other.opaque {
            if self.opaque.contains(raw) {
                continue;
            }
            let next = opaque_bytes.saturating_add(raw.len());
            if next > MAX_OPAQUE_BYTES {
                report.opaque_dropped = report.opaque_dropped.saturating_add(1);
                continue;
            }
            opaque_bytes = next;
            self.opaque.insert(raw.clone());
        }
        report
    }

    /// A local write of `value` under a new dot (validated first).
    pub(crate) fn write(
        &mut self,
        clock: &mut LocalClock,
        key: RecordKey,
        value: RecordValue,
        at: i64,
    ) -> Result<Dot, SyncError> {
        validate_record(&key, &value)?;
        if !self.records.contains_key(&key) && self.records.len() >= MAX_RECORDS {
            return Err(SyncError::TooLarge {
                what: "sync state",
                max: MAX_RECORDS,
            });
        }
        let dot = clock.next()?;
        self.context.insert(dot);
        self.records.entry(key).or_default().write(dot, at, value);
        Ok(dot)
    }

    /// A local deletion: a tombstone under a new dot. `None` when there is
    /// nothing to delete (absent, or already a lone tombstone).
    pub(crate) fn delete(
        &mut self,
        clock: &mut LocalClock,
        key: RecordKey,
        at: i64,
    ) -> Result<Option<Dot>, SyncError> {
        match self.records.get(&key) {
            None => Ok(None),
            Some(register) if register.single().is_some_and(RecordValue::is_tombstone) => Ok(None),
            Some(_) => self.write(clock, key, RecordValue::Tombstone, at).map(Some),
        }
    }

    /// Every register with more than one distinct value.
    pub(crate) fn conflicts(&self) -> Vec<Conflict<'_>> {
        self.records
            .iter()
            .filter(|(_, register)| register.is_conflict())
            .map(|(key, register)| Conflict {
                key,
                siblings: register.siblings(),
            })
            .collect()
    }

    /// Applies a user's resolution of the conflict on `key` as a new dot
    /// that covers every sibling.
    pub(crate) fn resolve(
        &mut self,
        clock: &mut LocalClock,
        key: &RecordKey,
        resolution: &Resolution,
        at: i64,
    ) -> Result<Dot, SyncError> {
        let register = self.records.get(key).ok_or(SyncError::Resolution {
            reason: "no such record",
        })?;
        if !register.is_conflict() {
            return Err(SyncError::Resolution {
                reason: "the record is not in conflict",
            });
        }
        let siblings = register.siblings();
        let value = match resolution {
            Resolution::Pick(index) => siblings
                .get(*index)
                .map(|sibling| sibling.value.clone())
                .ok_or(SyncError::Resolution {
                    reason: "no such side",
                })?,
            Resolution::Union => {
                let is_tags = matches!(
                    key,
                    RecordKey::HostMeta {
                        field: MetaField::Tags,
                        ..
                    }
                );
                if !is_tags {
                    return Err(SyncError::Resolution {
                        reason: "union only applies to tags",
                    });
                }
                let mut union = BTreeSet::new();
                for sibling in &siblings {
                    match sibling.value {
                        RecordValue::Tags(tags) => union.extend(tags.iter().cloned()),
                        RecordValue::Tombstone => {}
                        _ => {
                            return Err(SyncError::Resolution {
                                reason: "union only applies to tags",
                            });
                        }
                    }
                }
                if union.is_empty() {
                    RecordValue::Tombstone
                } else {
                    RecordValue::Tags(union)
                }
            }
        };
        self.write(clock, key.clone(), value, at)
    }

    /// Drops registers whose every surviving dot is a tombstone covered by
    /// `stable` (the meet of the contexts of every non-revoked device,
    /// computed by the engine). The global context keeps them deleted
    /// everywhere (F6). Returns how many registers were collected.
    pub(crate) fn collect_tombstones(&mut self, stable: &VersionVector) -> usize {
        let before = self.records.len();
        self.records
            .retain(|_, register| !register.collect_if(stable, RecordValue::is_tombstone));
        before.saturating_sub(self.records.len())
    }

    /// Structural invariants of a state read from a snapshot: bounded
    /// counts, counters ≥ 1, every dot covered by the context.
    pub(crate) fn check_invariants(&self) -> Result<(), SyncError> {
        let corrupt = |reason| SyncError::Corrupt {
            what: "snapshot",
            reason,
        };
        if self.records.len() > MAX_RECORDS {
            return Err(SyncError::TooLarge {
                what: "sync state",
                max: MAX_RECORDS,
            });
        }
        if self.context.len() > MAX_CONTEXT_DEVICES {
            return Err(corrupt("too many devices in the context"));
        }
        if self.opaque.iter().map(String::len).sum::<usize>() > MAX_OPAQUE_BYTES {
            return Err(SyncError::TooLarge {
                what: "unknown sync records",
                max: MAX_OPAQUE_BYTES,
            });
        }
        if self.context.iter().any(|(_, &counter)| counter == 0) {
            return Err(corrupt("zero counter in the context"));
        }
        for register in self.records.values() {
            if register.is_empty() {
                return Err(corrupt("empty register"));
            }
            if register.entries().len() > MAX_ENTRIES_PER_REGISTER {
                return Err(corrupt("too many concurrent values"));
            }
            for dot in register.entries().keys() {
                if dot.counter == 0 || !self.context.covers(dot) {
                    return Err(corrupt("a value is outside the causal context"));
                }
            }
        }
        Ok(())
    }
}

// ── Wire format ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryWire {
    dot: Dot,
    at: i64,
    value: RecordValue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWire {
    key: RecordKey,
    entries: Vec<EntryWire>,
}

#[derive(Serialize)]
struct StateWireOut<'a> {
    context: &'a VersionVector,
    records: Vec<Box<RawValue>>,
}

#[derive(Deserialize)]
struct StateWireIn {
    context: VersionVector,
    records: Vec<Box<RawValue>>,
}

impl Serialize for SyncState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut records = Vec::with_capacity(self.records.len().saturating_add(self.opaque.len()));
        for (key, register) in &self.records {
            let wire = RecordWire {
                key: key.clone(),
                entries: register
                    .entries()
                    .iter()
                    .map(|(dot, entry)| EntryWire {
                        dot: *dot,
                        at: entry.at,
                        value: entry.value.clone(),
                    })
                    .collect(),
            };
            records.push(serde_json::value::to_raw_value(&wire).map_err(S::Error::custom)?);
        }
        for raw in &self.opaque {
            records.push(RawValue::from_string(raw.clone()).map_err(S::Error::custom)?);
        }
        StateWireOut {
            context: &self.context,
            records,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SyncState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = StateWireIn::deserialize(deserializer)?;
        if wire.records.len() > MAX_RECORDS.saturating_add(MAX_RECORDS) {
            return Err(D::Error::custom("too many records"));
        }
        let mut state = Self {
            context: wire.context,
            ..Self::default()
        };
        for raw in wire.records {
            // Anything this version cannot read exactly is kept verbatim.
            let Ok(record) = serde_json::from_str::<RecordWire>(raw.get()) else {
                state.opaque.insert(raw.get().to_owned());
                continue;
            };
            if record.entries.len() > MAX_ENTRIES_PER_REGISTER {
                return Err(D::Error::custom("too many concurrent values"));
            }
            let mut entries = BTreeMap::new();
            for entry in record.entries {
                let value = Entry {
                    at: entry.at,
                    value: entry.value,
                };
                if entries.insert(entry.dot, value).is_some() {
                    return Err(D::Error::custom("duplicate dot"));
                }
            }
            if state
                .records
                .insert(record.key, MvRegister::from_entries(entries))
                .is_some()
            {
                return Err(D::Error::custom("duplicate record key"));
            }
        }
        Ok(state)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::super::clock::DeviceId;
    use super::super::crypto::ID_LEN;
    use super::*;

    pub(crate) fn device(n: u8) -> DeviceId {
        DeviceId::from_bytes([n; ID_LEN])
    }

    pub(crate) fn host_key(alias: &str) -> RecordKey {
        RecordKey::Host {
            alias: alias.to_owned(),
        }
    }

    pub(crate) fn host(hostname: &str) -> RecordValue {
        RecordValue::Host(HostValue {
            hostname: hostname.to_owned(),
            user: None,
            port: None,
            identity_file: None,
            proxy_jump: None,
            renamed_from: None,
        })
    }

    fn tags_key(alias: &str) -> RecordKey {
        RecordKey::HostMeta {
            alias: alias.to_owned(),
            field: MetaField::Tags,
        }
    }

    fn tags(list: &[&str]) -> RecordValue {
        RecordValue::Tags(list.iter().map(|t| (*t).to_owned()).collect())
    }

    /// A device: its state and clock.
    #[derive(Debug, Clone)]
    pub(crate) struct Replica {
        pub(crate) state: SyncState,
        pub(crate) clock: LocalClock,
    }

    impl Replica {
        pub(crate) fn new(n: u8) -> Self {
            Self {
                state: SyncState::default(),
                clock: LocalClock::new(device(n), 0),
            }
        }

        pub(crate) fn write(&mut self, key: RecordKey, value: RecordValue) -> Dot {
            self.state
                .write(&mut self.clock, key, value, 0)
                .expect("write")
        }

        pub(crate) fn delete(&mut self, key: RecordKey) -> Option<Dot> {
            self.state.delete(&mut self.clock, key, 0).expect("delete")
        }

        pub(crate) fn pull(&mut self, other: &Self) {
            self.clock
                .observe(other.state.context())
                .expect("no restore");
            self.state.merge(&other.state);
        }
    }

    fn merged(a: &SyncState, b: &SyncState) -> SyncState {
        let mut out = a.clone();
        out.merge(b);
        out
    }

    #[test]
    fn concurrent_writes_are_siblings_never_resolved_automatically() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("10.0.0.1"));
        b.write(host_key("prod"), host("10.0.0.2"));
        a.pull(&b);
        let conflicts = a.state.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].siblings.len(), 2);
        // Pulling again or the other way round keeps the conflict.
        b.pull(&a);
        a.pull(&b);
        assert_eq!(a.state, b.state);
        assert_eq!(b.state.conflicts().len(), 1);
    }

    #[test]
    fn equal_concurrent_values_collapse() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("10.0.0.1"));
        b.write(host_key("prod"), host("10.0.0.1"));
        a.pull(&b);
        assert!(a.state.conflicts().is_empty());
        let register = a.state.get(&host_key("prod")).expect("record");
        assert_eq!(register.single(), Some(&host("10.0.0.1")));
        assert_eq!(register.entries().len(), 2, "both dots kept, one value");
    }

    #[test]
    fn tombstone_vs_edit_is_a_conflict() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("10.0.0.1"));
        b.pull(&a);
        a.delete(host_key("prod")).expect("deleted");
        b.write(host_key("prod"), host("10.0.0.9"));
        a.pull(&b);
        let conflicts = a.state.conflicts();
        assert_eq!(conflicts.len(), 1);
        let values: Vec<&RecordValue> = conflicts[0].siblings.iter().map(|s| s.value).collect();
        assert!(values.contains(&&RecordValue::Tombstone));
        assert!(values.contains(&&host("10.0.0.9")));
    }

    #[test]
    fn sequential_delete_wins_without_conflict() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("10.0.0.1"));
        b.pull(&a);
        b.delete(host_key("prod"));
        a.pull(&b);
        assert!(a.state.conflicts().is_empty());
        assert_eq!(
            a.state.get(&host_key("prod")).and_then(MvRegister::single),
            Some(&RecordValue::Tombstone)
        );
        assert_eq!(a.delete(host_key("prod")), None, "already deleted");
        assert_eq!(a.delete(host_key("other")), None, "absent");
    }

    #[test]
    fn same_dot_with_another_value_is_reported_not_merged() {
        let mut a = Replica::new(1);
        a.write(host_key("prod"), host("10.0.0.1"));
        // A device holding the key re-uses A's dot for another value.
        let mut forged = a.state.clone();
        let register = forged.records.get_mut(&host_key("prod")).expect("reg");
        let dot = *register.entries().keys().next().expect("dot");
        register.write(dot, 0, host("6.6.6.6"));
        let mut b = Replica::new(2);
        b.pull(&a);
        let report = b.state.merge(&forged);
        assert_eq!(report.divergent_dots, 1);
        assert_eq!(
            b.state.get(&host_key("prod")).and_then(MvRegister::single),
            Some(&host("10.0.0.1")),
            "the value already verified is kept"
        );
        assert_eq!(a.state.merge(&a.state.clone()).divergent_dots, 0);
    }

    #[test]
    fn metadata_fields_merge_independently() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(tags_key("prod"), tags(&["db"]));
        b.write(
            RecordKey::HostMeta {
                alias: "prod".into(),
                field: MetaField::Notes,
            },
            RecordValue::Text("primary".into()),
        );
        a.pull(&b);
        assert!(a.state.conflicts().is_empty());
        assert_eq!(a.state.records().count(), 2);
    }

    #[test]
    fn tag_conflict_offers_union_and_resolution_converges() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(tags_key("prod"), tags(&["db"]));
        b.write(tags_key("prod"), tags(&["eu", "db"]));
        let mut c = Replica::new(3);
        c.write(tags_key("prod"), RecordValue::Tombstone);
        a.pull(&b);
        a.pull(&c);
        assert_eq!(a.state.conflicts().len(), 1, "never auto-union");
        let mut clock = a.clock.clone();
        a.state
            .resolve(&mut clock, &tags_key("prod"), &Resolution::Union, 5)
            .expect("union");
        a.clock = clock;
        assert_eq!(
            a.state.get(&tags_key("prod")).and_then(MvRegister::single),
            Some(&tags(&["db", "eu"]))
        );
        b.pull(&a);
        c.pull(&a);
        assert_eq!(b.state, a.state);
        assert_eq!(c.state, a.state);
    }

    #[test]
    fn resolutions_are_checked() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("a"));
        let mut clock = a.clock.clone();
        let error = a
            .state
            .resolve(&mut clock, &host_key("prod"), &Resolution::Pick(0), 0)
            .expect_err("no conflict");
        assert!(matches!(error, SyncError::Resolution { .. }));
        assert!(
            a.state
                .resolve(&mut clock, &host_key("nope"), &Resolution::Pick(0), 0)
                .is_err()
        );
        b.write(host_key("prod"), host("b"));
        a.pull(&b);
        assert!(
            a.state
                .resolve(&mut clock, &host_key("prod"), &Resolution::Union, 0)
                .is_err(),
            "union is for tags only"
        );
        assert!(
            a.state
                .resolve(&mut clock, &host_key("prod"), &Resolution::Pick(2), 0)
                .is_err()
        );
        let picked = a.state.get(&host_key("prod")).expect("reg").siblings()[1]
            .value
            .clone();
        a.state
            .resolve(&mut clock, &host_key("prod"), &Resolution::Pick(1), 0)
            .expect("pick");
        assert_eq!(
            a.state.get(&host_key("prod")).and_then(MvRegister::single),
            Some(&picked)
        );
    }

    #[test]
    fn gc_tombstone_not_resurrected_by_new_joiner_on_rolled_back_folder() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("old"), host("10.0.0.1"));
        a.write(host_key("kept"), host("10.0.0.2"));
        // The folder will later be rolled back to this snapshot of A.
        let old_snapshot_of_a = a.clone();
        b.pull(&a);
        b.delete(host_key("old"));
        a.pull(&b);
        // Every device has seen the tombstone: it is collected everywhere.
        let stable = VersionVector::meet([a.state.context(), b.state.context()]);
        assert_eq!(a.state.collect_tombstones(&stable), 1);
        assert_eq!(b.state.collect_tombstones(&stable), 1);
        assert!(a.state.get(&host_key("old")).is_none());
        // A new device joins a folder that only holds A's old snapshot.
        let mut joiner = Replica::new(3);
        joiner.pull(&old_snapshot_of_a);
        assert!(joiner.state.get(&host_key("old")).is_some());
        joiner.write(host_key("new"), host("10.0.0.3"));
        // The old devices merge the joiner: "old" stays deleted.
        a.pull(&joiner);
        b.pull(&joiner);
        assert!(a.state.get(&host_key("old")).is_none());
        assert!(b.state.get(&host_key("old")).is_none());
        assert!(a.state.get(&host_key("new")).is_some());
        // And the joiner learns the deletion as soon as it sees them.
        joiner.pull(&a);
        assert!(joiner.state.get(&host_key("old")).is_none());
        assert!(joiner.state.get(&host_key("kept")).is_some());
        assert_eq!(joiner.state, a.state);
    }

    #[test]
    fn collection_waits_for_stability_and_keeps_conflicts() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("x"), host("1"));
        b.pull(&a);
        a.delete(host_key("x"));
        // B has not seen the tombstone: not stable.
        let stable = VersionVector::meet([a.state.context(), b.state.context()]);
        assert_eq!(a.state.collect_tombstones(&stable), 0);
        // Concurrent edit on B: conflict, not collectable even when stable.
        b.write(host_key("x"), host("2"));
        a.pull(&b);
        b.pull(&a);
        let stable = VersionVector::meet([a.state.context(), b.state.context()]);
        assert_eq!(a.state.collect_tombstones(&stable), 0);
        assert_eq!(a.state.conflicts().len(), 1);
    }

    #[test]
    fn settings_whitelist_excludes_copy_on_select_and_update_check() {
        for path in [
            "theme",
            "dark_theme",
            "keymap.tui.connect",
            "keymap.gui.palette",
        ] {
            let key = SettingKey::parse(path).expect("synced");
            assert_eq!(key.path(), path);
        }
        for path in [
            "gui.copy_on_select",
            "updates.check",
            "keymap.tui.",
            "keymap.tui.a\u{202e}b",
            "keymap.other.x",
            "recordings_dir",
            "",
        ] {
            assert_eq!(SettingKey::parse(path), None, "{path}");
        }
        // A newer device's unknown setting is opaque, not an error.
        let json = r#"{"context":{},"records":[{"key":{"kind":"setting","key":"updates.check"},"entries":[]}]}"#;
        let state: SyncState = serde_json::from_str(json).expect("parse");
        assert_eq!(state.records().count(), 0);
        assert_eq!(state.opaque_len(), 1);
    }

    #[test]
    fn writes_are_validated() {
        let mut a = Replica::new(1);
        let bad = [
            (host_key(""), host("h")),
            (host_key("a b"), host("h")),
            (host_key("prod"), host("")),
            (host_key("prod"), host("h\u{1b}[31m")),
            (host_key("prod"), tags(&["x"])),
            (
                RecordKey::Snippet {
                    id: "Bad_Id".into(),
                },
                RecordValue::Tombstone,
            ),
            (
                RecordKey::Secret {
                    account: "u@h:22".into(),
                },
                RecordValue::Tombstone,
            ),
            (
                RecordKey::Setting {
                    key: SettingKey::KeymapGui("bad\u{7}action".into()),
                },
                RecordValue::Text("mod+k".into()),
            ),
            (tags_key("prod"), tags(&["has space"])),
            (
                RecordKey::HostMeta {
                    alias: "prod".into(),
                    field: MetaField::Favorite,
                },
                RecordValue::Text("yes".into()),
            ),
        ];
        for (key, value) in bad {
            let error = a
                .state
                .write(&mut a.clock, key.clone(), value, 0)
                .expect_err("invalid");
            assert!(matches!(error, SyncError::InvalidRecord { .. }), "{key:?}");
        }
        assert_eq!(a.clock.counter(), 0, "no dot spent on a refused write");
        let mut port0 = HostValue {
            hostname: "h".into(),
            user: None,
            port: Some(0),
            identity_file: None,
            proxy_jump: None,
            renamed_from: None,
        };
        assert!(validate_record(&host_key("p"), &RecordValue::Host(port0.clone())).is_err());
        port0.port = Some(22);
        validate_record(&host_key("p"), &RecordValue::Host(port0)).expect("valid");
        let notes = RecordKey::HostMeta {
            alias: "p".into(),
            field: MetaField::Notes,
        };
        validate_record(&notes, &RecordValue::Text("line 1\nline 2".into())).expect("valid");
    }

    #[test]
    fn synced_identity_file_rejects_parent_and_unc() {
        for good in ["~/.ssh/id_ed25519", "~/keys/work/id_rsa", "~/a b/c"] {
            HomeRelPath::parse(good).expect(good);
        }
        for bad in [
            "/home/a/.ssh/id",
            "~",
            "~/",
            "~/../etc/passwd",
            "~/.ssh/../../x",
            "~/./x",
            "~//x",
            "~/x/",
            "~/C:/x",
            "~/..\\x",
            "\\\\server\\share\\id",
            "C:\\Users\\a\\.ssh\\id",
            "~user/x",
            "~/x\u{0}y",
            "~/x\u{202e}y",
        ] {
            assert!(HomeRelPath::parse(bad).is_err(), "{bad:?}");
        }
        assert!(HomeRelPath::parse(&format!("~/{}", "a".repeat(2000))).is_err());
        // An invalid path from the folder never parses as a known record.
        let json = r#"{"context":{"01010101010101010101010101010101":1},"records":[{"key":{"kind":"host","alias":"p"},"entries":[{"dot":{"d":"01010101010101010101010101010101","c":1},"at":0,"value":{"type":"host","v":{"hostname":"h","identity_file":"~/../x"}}}]}]}"#;
        let state: SyncState = serde_json::from_str(json).expect("parse");
        assert!(state.get(&host_key("p")).is_none());
        assert_eq!(state.opaque_len(), 1);
    }

    #[test]
    fn opaque_records_pass_through_with_a_cap() {
        let unknown = r#"{"key":{"kind":"future_kind","id":"x"},"entries":[]}"#;
        let json = format!(r#"{{"context":{{}},"records":[{unknown}]}}"#);
        let state: SyncState = serde_json::from_str(&json).expect("parse");
        assert_eq!(state.opaque_len(), 1);
        let mut a = Replica::new(1);
        a.write(host_key("p"), host("h"));
        a.state.merge(&state);
        let republished = serde_json::to_string(&a.state).expect("ser");
        assert!(republished.contains(unknown), "{republished}");
        let back: SyncState = serde_json::from_str(&republished).expect("parse");
        assert_eq!(back, a.state);
        // The cap: big opaque records beyond 256 KiB are dropped, counted.
        let mut big = SyncState::default();
        for i in 0..5 {
            big.opaque
                .insert(format!(r#"{{"k":{i},"pad":"{}"}}"#, "x".repeat(60 * 1024)));
        }
        let mut target = SyncState::default();
        let report = target.merge(&big);
        assert_eq!(target.opaque_len(), 4);
        assert_eq!(report.opaque_dropped, 1);
        assert!(big.check_invariants().is_err(), "too large to read");
        target.purge_opaque();
        assert_eq!(target.opaque_len(), 0);
    }

    #[test]
    fn invariants_reject_structural_corruption() {
        let dev = "01010101010101010101010101010101";
        let parse = |json: String| serde_json::from_str::<SyncState>(&json);
        // A dot outside the context.
        let state = parse(format!(
            r#"{{"context":{{}},"records":[{{"key":{{"kind":"host","alias":"p"}},"entries":[{{"dot":{{"d":"{dev}","c":1}},"at":0,"value":{{"type":"tombstone"}}}}]}}]}}"#
        ))
        .expect("parse");
        assert!(state.check_invariants().is_err());
        // An empty register.
        let state = parse(
            r#"{"context":{},"records":[{"key":{"kind":"host","alias":"p"},"entries":[]}]}"#
                .to_owned(),
        )
        .expect("parse");
        assert!(state.check_invariants().is_err());
        // Zero counter.
        let state = parse(format!(r#"{{"context":{{"{dev}":0}},"records":[]}}"#)).expect("parse");
        assert!(state.check_invariants().is_err());
        // Duplicate keys or dots are parse errors.
        let entry =
            format!(r#"{{"dot":{{"d":"{dev}","c":1}},"at":0,"value":{{"type":"tombstone"}}}}"#);
        let record = format!(r#"{{"key":{{"kind":"host","alias":"p"}},"entries":[{entry}]}}"#);
        assert!(
            parse(format!(
                r#"{{"context":{{"{dev}":1}},"records":[{record},{record}]}}"#
            ))
            .is_err()
        );
        assert!(
            parse(format!(
                r#"{{"context":{{"{dev}":1}},"records":[{{"key":{{"kind":"host","alias":"p"}},"entries":[{entry},{entry}]}}]}}"#
            ))
            .is_err()
        );
        let ok = parse(format!(
            r#"{{"context":{{"{dev}":1}},"records":[{record}]}}"#
        ))
        .expect("parse");
        ok.check_invariants().expect("valid");
        // Too many concurrent values.
        let many: Vec<String> = (1..=65)
            .map(|c| {
                format!(
                    r#"{{"dot":{{"d":"{dev}","c":{c}}},"at":0,"value":{{"type":"tombstone"}}}}"#
                )
            })
            .collect();
        assert!(
            parse(format!(
                r#"{{"context":{{"{dev}":65}},"records":[{{"key":{{"kind":"host","alias":"p"}},"entries":[{}]}}]}}"#,
                many.join(",")
            ))
            .is_err()
        );
    }

    #[test]
    fn state_round_trips_through_json() {
        let (mut a, mut b) = (Replica::new(1), Replica::new(2));
        a.write(host_key("prod"), host("10.0.0.1"));
        a.write(
            RecordKey::Snippet { id: "logs".into() },
            RecordValue::Snippet(SnippetValue {
                name: "Logs".into(),
                command: "journalctl -f".into(),
                description: None,
                scope: SnippetScopeValue::Tags(vec!["db".into()]),
                run: true,
            }),
        );
        a.write(
            RecordKey::Setting {
                key: SettingKey::Theme,
            },
            RecordValue::Text("basalt".into()),
        );
        b.write(host_key("prod"), host("10.0.0.2"));
        a.pull(&b);
        let json = serde_json::to_string(&a.state).expect("ser");
        let back: SyncState = serde_json::from_str(&json).expect("de");
        assert_eq!(back, a.state);
        back.check_invariants().expect("valid");
    }

    // ── Property tests: merge laws over random histories ────────────────

    #[derive(Debug, Clone)]
    enum Op {
        Write { device: usize, key: u8, value: u8 },
        Delete { device: usize, key: u8 },
        Pull { to: usize, from: usize },
        Collect,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..3usize, 0..3u8, 0..3u8).prop_map(|(device, key, value)| Op::Write { device, key, value }),
            2 => (0..3usize, 0..3u8).prop_map(|(device, key)| Op::Delete { device, key }),
            4 => (0..3usize, 0..3usize).prop_map(|(to, from)| Op::Pull { to, from }),
            1 => Just(Op::Collect),
        ]
    }

    fn key_of(n: u8) -> RecordKey {
        host_key(&format!("h{n}"))
    }

    fn value_of(n: u8) -> RecordValue {
        host(&format!("10.0.0.{n}"))
    }

    /// A write as it happened: the dot, the key and the context the writer
    /// had just before (what it overwrote).
    #[derive(Debug, Clone)]
    struct Written {
        dot: Dot,
        key: RecordKey,
    }

    struct History {
        replicas: Vec<Replica>,
        /// Every intermediate state, for the law checks.
        states: Vec<SyncState>,
        writes: Vec<Written>,
        /// (key, overwritten dot) pairs: a later write that covered it.
        overwritten: BTreeSet<(RecordKey, Dot)>,
        collected: bool,
    }

    fn run(ops: &[Op]) -> History {
        let mut history = History {
            replicas: (1..=3).map(Replica::new).collect(),
            states: Vec::new(),
            writes: Vec::new(),
            overwritten: BTreeSet::new(),
            collected: false,
        };
        for op in ops {
            match *op {
                Op::Write { device, key, value } => {
                    let replica = &mut history.replicas[device];
                    if let Some(register) = replica.state.get(&key_of(key)) {
                        for dot in register.entries().keys() {
                            history.overwritten.insert((key_of(key), *dot));
                        }
                    }
                    let dot = replica.write(key_of(key), value_of(value));
                    history.writes.push(Written {
                        dot,
                        key: key_of(key),
                    });
                }
                Op::Delete { device, key } => {
                    let replica = &mut history.replicas[device];
                    let before: Vec<Dot> = replica
                        .state
                        .get(&key_of(key))
                        .map(|r| r.entries().keys().copied().collect())
                        .unwrap_or_default();
                    if let Some(dot) = replica.delete(key_of(key)) {
                        for old in before {
                            history.overwritten.insert((key_of(key), old));
                        }
                        history.writes.push(Written {
                            dot,
                            key: key_of(key),
                        });
                    }
                }
                Op::Pull { to, from } => {
                    let source = history.replicas[from].clone();
                    history.replicas[to].pull(&source);
                }
                Op::Collect => {
                    let stable =
                        VersionVector::meet(history.replicas.iter().map(|r| r.state.context()));
                    for replica in &mut history.replicas {
                        if replica.state.collect_tombstones(&stable) > 0 {
                            history.collected = true;
                        }
                    }
                }
            }
            history
                .states
                .extend(history.replicas.iter().map(|r| r.state.clone()));
        }
        history
    }

    fn pick(states: &[SyncState], index: prop::sample::Index) -> SyncState {
        if states.is_empty() {
            SyncState::default()
        } else {
            states[index.index(states.len())].clone()
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        #[test]
        fn merge_is_commutative(ops in prop::collection::vec(op(), 0..40), i in any::<prop::sample::Index>(), j in any::<prop::sample::Index>()) {
            let history = run(&ops);
            let (a, b) = (pick(&history.states, i), pick(&history.states, j));
            prop_assert_eq!(merged(&a, &b), merged(&b, &a));
        }

        #[test]
        fn merge_is_associative(ops in prop::collection::vec(op(), 0..40), i in any::<prop::sample::Index>(), j in any::<prop::sample::Index>(), k in any::<prop::sample::Index>()) {
            let history = run(&ops);
            let (a, b, c) = (pick(&history.states, i), pick(&history.states, j), pick(&history.states, k));
            prop_assert_eq!(merged(&merged(&a, &b), &c), merged(&a, &merged(&b, &c)));
        }

        #[test]
        fn merge_is_idempotent(ops in prop::collection::vec(op(), 0..40), i in any::<prop::sample::Index>(), j in any::<prop::sample::Index>()) {
            let history = run(&ops);
            let (a, b) = (pick(&history.states, i), pick(&history.states, j));
            prop_assert_eq!(merged(&a, &a), a.clone());
            let ab = merged(&a, &b);
            prop_assert_eq!(merged(&ab, &b), ab.clone());
            prop_assert_eq!(merged(&ab, &a), ab);
        }

        #[test]
        fn every_delivery_order_converges(ops in prop::collection::vec(op(), 0..40), order in Just(vec![0usize, 1, 2]).prop_shuffle()) {
            let history = run(&ops);
            let mut forward = SyncState::default();
            for r in &history.replicas {
                forward.merge(&r.state);
            }
            let mut shuffled = SyncState::default();
            for &index in &order {
                shuffled.merge(&history.replicas[index].state);
                // Duplicated deliveries change nothing.
                shuffled.merge(&history.replicas[index].state);
            }
            prop_assert_eq!(&forward, &shuffled);
            forward.check_invariants().map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        }

        #[test]
        fn no_write_is_lost_unless_overwritten(ops in prop::collection::vec(op(), 0..40)) {
            let history = run(&ops);
            // Collection drops (stable) tombstones by design: check only
            // histories without it.
            prop_assume!(!history.collected);
            let mut all = SyncState::default();
            for r in &history.replicas {
                all.merge(&r.state);
            }
            for written in &history.writes {
                let present = all
                    .get(&written.key)
                    .is_some_and(|r| r.entries().contains_key(&written.dot));
                let overwritten = history.overwritten.contains(&(written.key.clone(), written.dot));
                prop_assert!(present || overwritten, "lost {:?}", written);
                prop_assert!(all.context().covers(&written.dot));
            }
        }

        #[test]
        fn conflict_iff_concurrent_different_values(v1 in 0..3u8, v2 in 0..3u8, sync_first in any::<bool>()) {
            let (mut a, mut b) = (Replica::new(1), Replica::new(2));
            a.write(key_of(0), value_of(v1));
            if sync_first {
                b.pull(&a);
            }
            b.write(key_of(0), value_of(v2));
            a.pull(&b);
            let conflict = a.state.get(&key_of(0)).is_some_and(MvRegister::is_conflict);
            prop_assert_eq!(conflict, !sync_first && v1 != v2);
        }
    }
}
