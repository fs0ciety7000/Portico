//! The synced state and its merge (S1a).
//!
//! A [`SyncState`] is a global causal context ([`VersionVector`]) plus one
//! **generic** multi-value register per key. The merge layer only sees
//! canonical JSON ([`WireKey`]: an object with a `kind`; [`WireValue`]: any
//! JSON), so every kind, known to this version or not, merges with the
//! same dots and contexts (S1a review finding 4). The typed records below
//! are decoded at projection time ([`SyncState::project`]):
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
//!   resurrecting it, even when it comes back from an old snapshot. Such a
//!   disappearance by coverage **never** yields a local deletion: only an
//!   explicit tombstone value does (S1b applies tombstones, never absences).
//! - **Transactional merge** (finding 2): [`SyncState::merge`] computes on
//!   a copy and refuses a result that breaks the caps (records, context
//!   devices, values per register, encoded size), naming the source device
//!   and leaving the state untouched.
//! - **Conflicts** (more than one distinct value) are never resolved
//!   automatically; [`SyncState::resolve`] is an explicit user decision
//!   written as a new dot. Concurrent tag edits are conflicts too, with
//!   [`Resolution::Union`] as an offered choice (owner decision #10).
//! - **Tombstone collection** only drops a register whose every value is a
//!   tombstone covered by the causally stable context ([`SyncState::collect_tombstones`]).
//! - **Forward compatibility** (finding 4, replaces the opaque passthrough
//!   of F15): a register whose key or value this version cannot decode
//!   exactly (unknown kind, unknown setting key, unknown field: typed
//!   decoding keeps `deny_unknown_fields`) is [`RegisterView::Held`]: kept,
//!   merged and relayed causally (an unknown record edited twice is relayed
//!   once, at its latest value), reported as "needs a newer Portico", never
//!   applied, never a local deletion or overwrite, and a local write over
//!   it is refused ([`SyncError::RecordHeld`]). Adding a field to an
//!   existing value kind is therefore safe. Only JSON without the generic
//!   record shape makes a state unreadable.
//! - **Settings whitelist** (F18): `theme`, `dark_theme`, `keymap.tui.*`,
//!   `keymap.gui.*`. `gui.copy_on_select` and `updates.check` (a network
//!   consent) are never synced.
//!
//! Values are checked structurally here ([`validate_record`]: lengths,
//! character classes, home-relative identity paths F10). S1b runs
//! [`validate_record`] on **every received value** before applying it,
//! then the semantic checks against the local files (shadowed aliases,
//! `ProxyJump`, snippet safety rules) with the existing validators.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

use super::SyncError;
use super::clock::{DeviceId, Dot, Entry, LocalClock, MvRegister, VersionVector};
use super::crypto::{self, MAX_BUCKET};

/// Most records in one state (hosts + metadata fields + snippets + settings).
pub(crate) const MAX_RECORDS: usize = 20_000;
/// Most surviving dots in one register (concurrent writers of one key).
pub(crate) const MAX_ENTRIES_PER_REGISTER: usize = 64;
/// Most devices in a context.
pub(crate) const MAX_CONTEXT_DEVICES: usize = 1024;
/// Room left in the largest snapshot bucket for everything but the state
/// (device name, versions, `seen` vector of up to 1024 devices).
const SNAPSHOT_HEADROOM_BYTES: usize = 256 * 1024;
/// Largest encoded state a merge may produce: it must still fit in a
/// snapshot this device can seal.
pub(crate) const MAX_STATE_BYTES: usize = MAX_BUCKET - SNAPSHOT_HEADROOM_BYTES;

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
/// Portable between `/home/a`, `/Users/a` and `C:\Users\a`, so it is at
/// least as strict as what Portico writes into `hosts.conf`
/// (`managed_hosts::check_path_chars`: no quote, `%`, `$`, `#`, `=`,
/// backtick, backslash, control or invisible character) and refuses every
/// component Windows reserves or rewrites (`store::windows_name_problem`:
/// `CON`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`…, a trailing dot or space,
/// `:` and `<>"|?*`) on every platform.
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
        crate::managed_hosts::check_path_chars(text).map_err(|_| {
            invalid("contains a quote, `%`, `$`, `#`, `=`, a backslash or a control character")
        })?;
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
                _ if component.chars().any(forbidden_char) => {
                    return Err(invalid("contains a control or invisible character"));
                }
                _ if crate::store::windows_name_problem(component) => {
                    return Err(invalid(
                        "has a name Windows reserves or rewrites (CON, NUL, COM1, `:`, a trailing dot or space)",
                    ));
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
    /// Keep the sibling at this index of [`Conflict::siblings`] (the
    /// order of [`SyncState::view`]).
    Pick(usize),
    /// Tags only: the union of every sibling's tags (tombstones count as
    /// no tag). Offered, preselected, never applied automatically.
    Union,
}

// ── Generic registers: canonical JSON keys and values ───────────────────

/// Canonical JSON (compact, object keys sorted: `serde_json` is built
/// without `preserve_order`) of any JSON value.
fn canonical_json(value: &serde_json::Value) -> String {
    value.to_string()
}

/// The key of a register as the merge sees it: the canonical JSON of an
/// object with a non-empty string `kind` (the generic record shape). Every
/// kind, known to this version or not, merges the same way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WireKey(String);

impl WireKey {
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let kind = value.as_object()?.get("kind")?.as_str()?;
        (!kind.is_empty()).then(|| Self(canonical_json(value)))
    }

    /// The wire key of a typed key.
    pub(crate) fn encode(key: &RecordKey) -> Result<Self, SyncError> {
        let value = serde_json::to_value(key).map_err(|_| SyncError::Crypto("encoding failed"))?;
        Self::from_json(&value).ok_or(SyncError::Crypto("encoding failed"))
    }

    /// The typed key, when this version knows the kind and every field,
    /// and the key is in its canonical form (one typed key ⇔ one wire key).
    pub(crate) fn decode(&self) -> Option<RecordKey> {
        let key: RecordKey = serde_json::from_str(&self.0).ok()?;
        (Self::encode(&key).ok().as_ref() == Some(self)).then_some(key)
    }

    /// The `kind` field (for "needs a newer Portico" reports).
    pub(crate) fn kind(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&self.0)
            .ok()
            .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// A register value as the merge sees it: canonical JSON, whatever it
/// holds. Typed decoding happens at projection time only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireValue(String);

/// Canonical JSON of [`RecordValue::Tombstone`]: the one value shape every
/// version understands (deletions and tombstone collection of any kind).
const TOMBSTONE_JSON: &str = r#"{"type":"tombstone"}"#;

impl WireValue {
    fn from_json(value: &serde_json::Value) -> Self {
        Self(canonical_json(value))
    }

    pub(crate) fn encode(value: &RecordValue) -> Result<Self, SyncError> {
        let value =
            serde_json::to_value(value).map_err(|_| SyncError::Crypto("encoding failed"))?;
        Ok(Self::from_json(&value))
    }

    /// The typed value; `None` when this version cannot read it exactly
    /// (unknown type, unknown field: `deny_unknown_fields`, invalid field).
    pub(crate) fn decode(&self) -> Option<RecordValue> {
        serde_json::from_str(&self.0).ok()
    }

    pub(crate) fn is_tombstone(&self) -> bool {
        self.0 == TOMBSTONE_JSON
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! raw_json_serde {
    ($name:ident, $parse:expr, $what:literal) => {
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::Error as _;
                // Emitted verbatim (canonical already).
                let raw: &RawValue = serde_json::from_str(&self.0).map_err(S::Error::custom)?;
                raw.serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = serde_json::Value::deserialize(deserializer)?;
                let parse: fn(&serde_json::Value) -> Option<Self> = $parse;
                parse(&value).ok_or_else(|| D::Error::custom($what))
            }
        }
    };
}

raw_json_serde!(WireKey, WireKey::from_json, "a record key needs a kind");
raw_json_serde!(
    WireValue,
    |value| Some(WireValue::from_json(value)),
    "invalid record value"
);

/// One distinct decoded value of a register and every dot that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedSibling {
    pub(crate) value: RecordValue,
    /// Ascending; never empty.
    pub(crate) dots: Vec<Dot>,
    /// Most recent display stamp among `dots`.
    pub(crate) at: i64,
}

/// How this version sees a register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RegisterView {
    /// Every surviving value decodes: one sibling, or a conflict (more
    /// than one). Equal values are grouped, in order of their smallest dot.
    Known(Vec<TypedSibling>),
    /// A surviving value needs a newer Portico (or is malformed): the
    /// register is kept and relayed causally, and it **never** produces a
    /// local deletion or a local overwrite. The local record stays as it
    /// is until a version that reads it applies it.
    Held,
}

/// A register this version cannot apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldRecord {
    /// The `kind` of its key.
    pub(crate) kind: String,
    /// The typed key when the kind is known but a value is not: S1b must
    /// leave that local record untouched.
    pub(crate) key: Option<RecordKey>,
}

/// The typed view of a whole state (what S1b applies).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    pub(crate) known: BTreeMap<RecordKey, Vec<TypedSibling>>,
    pub(crate) held: Vec<HeldRecord>,
}

/// One unresolved conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict {
    pub(crate) key: RecordKey,
    pub(crate) siblings: Vec<TypedSibling>,
}

/// Anomalies of a merge (alerts, S1b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MergeReport {
    /// Dots received with a value different from the local one for the
    /// same dot: a misbehaving device holding the key.
    pub(crate) divergent_dots: usize,
    /// Live local values removed by coverage while the other state has no
    /// entry for their key (S1a review finding 3). Expected only after a
    /// tombstone collection; otherwise an inflated context.
    pub(crate) dropped_live_without_successor: usize,
}

/// The synced state: global context and one generic register per key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SyncState {
    context: VersionVector,
    records: BTreeMap<WireKey, MvRegister<WireValue>>,
}

fn is_live(value: &WireValue) -> bool {
    !value.is_tombstone()
}

impl SyncState {
    pub(crate) fn context(&self) -> &VersionVector {
        &self.context
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// The register of a typed key.
    pub(crate) fn register(&self, key: &RecordKey) -> Option<&MvRegister<WireValue>> {
        self.records.get(&WireKey::encode(key).ok()?)
    }

    /// The typed view of the register of `key` (`None`: no register).
    pub(crate) fn view(&self, key: &RecordKey) -> Option<RegisterView> {
        self.register(key).map(view_of)
    }

    /// The value of `key` when its register is known and holds exactly one
    /// distinct value.
    pub(crate) fn value(&self, key: &RecordKey) -> Option<RecordValue> {
        match self.view(key)? {
            RegisterView::Known(mut siblings) if siblings.len() == 1 => {
                siblings.pop().map(|sibling| sibling.value)
            }
            _ => None,
        }
    }

    /// The typed view of every register.
    pub(crate) fn project(&self) -> Projection {
        let mut projection = Projection::default();
        for (wire_key, register) in &self.records {
            let key = wire_key.decode();
            match (key, view_of(register)) {
                (Some(key), RegisterView::Known(siblings)) => {
                    projection.known.insert(key, siblings);
                }
                (key, _) => projection.held.push(HeldRecord {
                    kind: wire_key.kind(),
                    key,
                }),
            }
        }
        projection
    }

    /// Joins `other` (the state of device `source`) into `self`.
    ///
    /// Transactional (S1a review finding 2): the merge is computed on a
    /// copy, whose invariants (counts, context size, values per register)
    /// and encoded size are checked; on failure `self` is left untouched
    /// and the error names `source`. Commutative, associative and
    /// idempotent on registers and contexts.
    pub(crate) fn merge(
        &mut self,
        other: &Self,
        source: &DeviceId,
    ) -> Result<MergeReport, SyncError> {
        let mut merged = self.clone();
        let report = merged.merge_unchecked(other);
        let refused = |reason| SyncError::MergeRefused {
            device: source.short(),
            reason,
        };
        if let Some(reason) = merged.invariant_problem() {
            return Err(refused(reason));
        }
        if crypto::json_len(&merged)? > MAX_STATE_BYTES {
            return Err(refused("the merged data would be too large to publish"));
        }
        *self = merged;
        Ok(report)
    }

    fn merge_unchecked(&mut self, other: &Self) -> MergeReport {
        let empty = MvRegister::default();
        let mut report = MergeReport::default();
        let keys: BTreeSet<WireKey> = self
            .records
            .keys()
            .chain(other.records.keys())
            .cloned()
            .collect();
        for key in keys {
            let theirs = other.records.get(&key).unwrap_or(&empty);
            let ours = self.records.entry(key).or_default();
            let merged = ours.merge(&self.context, theirs, &other.context, is_live);
            report.divergent_dots = report.divergent_dots.saturating_add(merged.divergent);
            report.dropped_live_without_successor = report
                .dropped_live_without_successor
                .saturating_add(merged.dropped_live_without_successor);
        }
        self.records.retain(|_, register| !register.is_empty());
        self.context.join(&other.context);
        report
    }

    /// A local write of `value` under a new dot (validated first).
    pub(crate) fn write(
        &mut self,
        clock: &mut LocalClock,
        key: &RecordKey,
        value: &RecordValue,
        at: i64,
    ) -> Result<Dot, SyncError> {
        validate_record(key, value)?;
        let wire_key = WireKey::encode(key)?;
        let wire_value = WireValue::encode(value)?;
        if let Some(RegisterView::Held) = self.view(key) {
            // A newer device's value: overwriting it from here would lose
            // what this version cannot read.
            return Err(SyncError::RecordHeld { kind: key.kind() });
        }
        if !self.records.contains_key(&wire_key) && self.records.len() >= MAX_RECORDS {
            return Err(SyncError::TooLarge {
                what: "sync state",
                max: MAX_RECORDS,
            });
        }
        let dot = clock.next()?;
        self.context.insert(dot);
        self.records
            .entry(wire_key)
            .or_default()
            .write(dot, at, wire_value);
        Ok(dot)
    }

    /// A local deletion: a tombstone under a new dot. `None` when there is
    /// nothing to delete (absent, or already a lone tombstone).
    pub(crate) fn delete(
        &mut self,
        clock: &mut LocalClock,
        key: &RecordKey,
        at: i64,
    ) -> Result<Option<Dot>, SyncError> {
        match self.register(key) {
            None => Ok(None),
            Some(register) if register.single().is_some_and(WireValue::is_tombstone) => Ok(None),
            Some(_) => self
                .write(clock, key, &RecordValue::Tombstone, at)
                .map(Some),
        }
    }

    /// Every known register with more than one distinct value.
    pub(crate) fn conflicts(&self) -> Vec<Conflict> {
        self.project()
            .known
            .into_iter()
            .filter(|(_, siblings)| siblings.len() > 1)
            .map(|(key, siblings)| Conflict { key, siblings })
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
        let siblings = match self.view(key) {
            None => {
                return Err(SyncError::Resolution {
                    reason: "no such record",
                });
            }
            Some(RegisterView::Held) => {
                return Err(SyncError::RecordHeld { kind: key.kind() });
            }
            Some(RegisterView::Known(siblings)) => siblings,
        };
        if siblings.len() < 2 {
            return Err(SyncError::Resolution {
                reason: "the record is not in conflict",
            });
        }
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
                    match &sibling.value {
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
        self.write(clock, key, &value, at)
    }

    /// Drops registers whose every surviving dot is a tombstone covered by
    /// `stable` (the meet of the contexts of every non-revoked device,
    /// computed by the engine), whatever their kind. The global context
    /// keeps them deleted everywhere (F6). Returns how many registers were
    /// collected.
    #[cfg(test)]
    pub(crate) fn collect_tombstones(&mut self, stable: &VersionVector) -> usize {
        self.collect_tombstones_except(stable, |_| false)
    }

    /// [`Self::collect_tombstones`], keeping the registers `keep` names
    /// (S1b: a deletion not applied to the local files yet).
    pub(crate) fn collect_tombstones_except(
        &mut self,
        stable: &VersionVector,
        keep: impl Fn(&WireKey) -> bool,
    ) -> usize {
        let before = self.records.len();
        self.records.retain(|key, register| {
            keep(key) || !register.collect_if(stable, WireValue::is_tombstone)
        });
        before.saturating_sub(self.records.len())
    }

    /// Structural invariants of a state read from a snapshot: bounded
    /// counts, counters ≥ 1, every dot covered by the context.
    pub(crate) fn check_invariants(&self) -> Result<(), SyncError> {
        if self.records.len() > MAX_RECORDS {
            return Err(SyncError::TooLarge {
                what: "sync state",
                max: MAX_RECORDS,
            });
        }
        match self.invariant_problem() {
            None => Ok(()),
            Some(reason) => Err(SyncError::Corrupt {
                what: "snapshot",
                reason,
            }),
        }
    }

    fn invariant_problem(&self) -> Option<&'static str> {
        if self.records.len() > MAX_RECORDS {
            return Some("too many records");
        }
        if self.context.len() > MAX_CONTEXT_DEVICES {
            return Some("too many devices in the context");
        }
        if self.context.iter().any(|(_, &counter)| counter == 0) {
            return Some("zero counter in the context");
        }
        for register in self.records.values() {
            if register.is_empty() {
                return Some("empty register");
            }
            if register.entries().len() > MAX_ENTRIES_PER_REGISTER {
                return Some("too many concurrent values");
            }
            for dot in register.entries().keys() {
                if dot.counter == 0 || !self.context.covers(dot) {
                    return Some("a value is outside the causal context");
                }
            }
        }
        None
    }
}

/// Typed view of one register (see [`RegisterView`]).
fn view_of(register: &MvRegister<WireValue>) -> RegisterView {
    let mut siblings: Vec<TypedSibling> = Vec::new();
    for (dot, entry) in register.entries() {
        let Some(value) = entry.value.decode() else {
            return RegisterView::Held;
        };
        if let Some(sibling) = siblings.iter_mut().find(|s| s.value == value) {
            sibling.dots.push(*dot);
            sibling.at = sibling.at.max(entry.at);
        } else {
            siblings.push(TypedSibling {
                value,
                dots: vec![*dot],
                at: entry.at,
            });
        }
    }
    RegisterView::Known(siblings)
}

// ── Wire format ─────────────────────────────────────────────────────────
//
// The generic record shape: `{"key": {"kind": "…", …}, "entries": [{"dot":
// {"d", "c"}, "at", "value": <any JSON>}]}`. Every kind, known or not, has
// it, so every kind merges causally. Something without that shape (an
// unknown field at the record or entry level, a key without `kind`) makes
// the whole state unreadable (`Corrupt`): changing the generic shape needs
// a new body format.

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryWire {
    dot: Dot,
    at: i64,
    value: WireValue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWire {
    key: WireKey,
    entries: Vec<EntryWire>,
}

#[derive(Serialize)]
struct StateWireOut<'a> {
    context: &'a VersionVector,
    records: Vec<RecordWireOut<'a>>,
}

#[derive(Serialize)]
struct RecordWireOut<'a> {
    key: &'a WireKey,
    entries: Vec<EntryWireOut<'a>>,
}

#[derive(Serialize)]
struct EntryWireOut<'a> {
    dot: Dot,
    at: i64,
    value: &'a WireValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateWireIn {
    context: VersionVector,
    records: Vec<RecordWire>,
}

impl Serialize for SyncState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StateWireOut {
            context: &self.context,
            records: self
                .records
                .iter()
                .map(|(key, register)| RecordWireOut {
                    key,
                    entries: register
                        .entries()
                        .iter()
                        .map(|(dot, entry)| EntryWireOut {
                            dot: *dot,
                            at: entry.at,
                            value: &entry.value,
                        })
                        .collect(),
                })
                .collect(),
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
        let mut records: BTreeMap<WireKey, BTreeMap<Dot, Entry<WireValue>>> = BTreeMap::new();
        for record in wire.records {
            // Several records with one key (a newer writer, or a key
            // written in two equivalent forms): their entries merge, still
            // bounded by the caps below.
            let entries = records.entry(record.key).or_default();
            for entry in record.entries {
                let value = Entry {
                    at: entry.at,
                    value: entry.value,
                };
                match entries.get(&entry.dot) {
                    Some(existing) if existing.value != value.value => {
                        return Err(D::Error::custom("one dot with two values"));
                    }
                    Some(_) => {}
                    None => {
                        entries.insert(entry.dot, value);
                    }
                }
                if entries.len() > MAX_ENTRIES_PER_REGISTER {
                    return Err(D::Error::custom("too many concurrent values"));
                }
            }
        }
        Ok(Self {
            context: wire.context,
            records: records
                .into_iter()
                .map(|(key, entries)| (key, MvRegister::from_entries(entries)))
                .collect(),
        })
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

        // Owned arguments keep the many test call sites short.
        #[allow(clippy::needless_pass_by_value)]
        pub(crate) fn write(&mut self, key: RecordKey, value: RecordValue) -> Dot {
            self.state
                .write(&mut self.clock, &key, &value, 0)
                .expect("write")
        }

        // Owned argument, as `write`.
        #[allow(clippy::needless_pass_by_value)]
        pub(crate) fn delete(&mut self, key: RecordKey) -> Option<Dot> {
            self.state.delete(&mut self.clock, &key, 0).expect("delete")
        }

        pub(crate) fn pull(&mut self, other: &Self) {
            self.clock
                .check_claim(other.state.context(), &other.clock.device())
                .expect("no inflated claim");
            self.state
                .merge(&other.state, &other.clock.device())
                .expect("within caps");
        }
    }

    fn merged(a: &SyncState, b: &SyncState) -> SyncState {
        let mut out = a.clone();
        out.merge(b, &device(9)).expect("within caps");
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
        assert_eq!(a.state.value(&host_key("prod")), Some(host("10.0.0.1")));
        let register = a.state.register(&host_key("prod")).expect("record");
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
        let values: Vec<&RecordValue> = conflicts[0].siblings.iter().map(|s| &s.value).collect();
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
            a.state.value(&host_key("prod")),
            Some(RecordValue::Tombstone)
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
        let register = forged
            .records
            .get_mut(&WireKey::encode(&host_key("prod")).expect("key"))
            .expect("reg");
        let dot = *register.entries().keys().next().expect("dot");
        register.write(dot, 0, WireValue::encode(&host("6.6.6.6")).expect("value"));
        let mut b = Replica::new(2);
        b.pull(&a);
        let report = b.state.merge(&forged, &device(3)).expect("merge");
        assert_eq!(report.divergent_dots, 1);
        assert_eq!(
            b.state.value(&host_key("prod")),
            Some(host("10.0.0.1")),
            "the value already verified is kept"
        );
        let copy = a.state.clone();
        assert_eq!(
            a.state
                .merge(&copy, &device(1))
                .expect("merge")
                .divergent_dots,
            0
        );
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
        assert_eq!(a.state.len(), 2);
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
        assert_eq!(a.state.value(&tags_key("prod")), Some(tags(&["db", "eu"])));
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
        let Some(RegisterView::Known(siblings)) = a.state.view(&host_key("prod")) else {
            panic!("known register");
        };
        let picked = siblings[1].value.clone();
        a.state
            .resolve(&mut clock, &host_key("prod"), &Resolution::Pick(1), 0)
            .expect("pick");
        assert_eq!(a.state.value(&host_key("prod")), Some(picked));
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
        assert!(a.state.register(&host_key("old")).is_none());
        // A new device joins a folder that only holds A's old snapshot.
        let mut joiner = Replica::new(3);
        joiner.pull(&old_snapshot_of_a);
        assert!(joiner.state.register(&host_key("old")).is_some());
        joiner.write(host_key("new"), host("10.0.0.3"));
        // The old devices merge the joiner: "old" stays deleted.
        a.pull(&joiner);
        b.pull(&joiner);
        assert!(a.state.register(&host_key("old")).is_none());
        assert!(b.state.register(&host_key("old")).is_none());
        assert!(a.state.register(&host_key("new")).is_some());
        // And the joiner learns the deletion as soon as it sees them.
        joiner.pull(&a);
        assert!(joiner.state.register(&host_key("old")).is_none());
        assert!(joiner.state.register(&host_key("kept")).is_some());
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
        // A newer device's unknown setting is held, not an error.
        let dev = "01010101010101010101010101010101";
        let json = format!(
            r#"{{"context":{{"{dev}":1}},"records":[{{"key":{{"kind":"setting","key":"updates.check"}},"entries":[{{"dot":{{"d":"{dev}","c":1}},"at":0,"value":{{"type":"text","v":"never"}}}}]}}]}}"#
        );
        let state: SyncState = serde_json::from_str(&json).expect("parse");
        state.check_invariants().expect("valid");
        let projection = state.project();
        assert!(projection.known.is_empty());
        assert_eq!(
            projection.held,
            vec![HeldRecord {
                kind: "setting".into(),
                key: None
            }]
        );
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
                .write(&mut a.clock, &key, &value, 0)
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
        // An invalid path from the folder never decodes as a known value:
        // the register is held (never applied), not dropped.
        let json = r#"{"context":{"01010101010101010101010101010101":1},"records":[{"key":{"kind":"host","alias":"p"},"entries":[{"dot":{"d":"01010101010101010101010101010101","c":1},"at":0,"value":{"type":"host","v":{"hostname":"h","identity_file":"~/../x"}}}]}]}"#;
        let state: SyncState = serde_json::from_str(json).expect("parse");
        assert_eq!(state.view(&host_key("p")), Some(RegisterView::Held));
        assert_eq!(state.value(&host_key("p")), None);
    }

    /// A write of an arbitrary wire key and value, as a newer Portico (or
    /// a device holding the key) produces it.
    fn write_wire(
        replica: &mut Replica,
        key: &serde_json::Value,
        value: &serde_json::Value,
    ) -> Dot {
        let dot = replica.clock.next().expect("dot");
        replica.state.context.insert(dot);
        replica
            .state
            .records
            .entry(WireKey::from_json(key).expect("key"))
            .or_default()
            .write(dot, 0, WireValue::from_json(value));
        dot
    }

    fn numbered_device(side: u8, n: usize) -> DeviceId {
        let mut bytes = [side; ID_LEN];
        let n = u16::try_from(n).expect("small");
        bytes[..2].copy_from_slice(&n.to_be_bytes());
        DeviceId::from_bytes(bytes)
    }

    #[test]
    fn merge_that_would_break_invariants_is_refused_and_leaves_state_intact() {
        // 1024 disjoint context devices on each side: each state is valid,
        // their union is not.
        let wide = |side: u8| {
            let mut state = SyncState::default();
            for n in 0..MAX_CONTEXT_DEVICES {
                state.context.insert(Dot {
                    device: numbered_device(side, n),
                    counter: 1,
                });
            }
            state.check_invariants().expect("valid on its own");
            state
        };
        let mut ours = wide(0xa0);
        let before = ours.clone();
        let error = ours.merge(&wide(0xb0), &device(7)).expect_err("too wide");
        assert_eq!(
            error,
            SyncError::MergeRefused {
                device: device(7).short(),
                reason: "too many devices in the context",
            }
        );
        assert_eq!(ours, before, "nothing merged");

        // Two registers of the same key with 64 concurrent dots each.
        let crowded = |side: u8| {
            let mut state = SyncState::default();
            let mut entries = BTreeMap::new();
            for n in 0..MAX_ENTRIES_PER_REGISTER {
                let dot = Dot {
                    device: numbered_device(side, n),
                    counter: 1,
                };
                state.context.insert(dot);
                entries.insert(
                    dot,
                    Entry {
                        at: 0,
                        value: WireValue::encode(&host(&format!("10.0.{side}.{n}")))
                            .expect("value"),
                    },
                );
            }
            state.records.insert(
                WireKey::encode(&host_key("prod")).expect("key"),
                MvRegister::from_entries(entries),
            );
            state.check_invariants().expect("valid on its own");
            state
        };
        let mut ours = crowded(1);
        let before = ours.clone();
        assert!(matches!(
            ours.merge(&crowded(2), &device(8)),
            Err(SyncError::MergeRefused {
                reason: "too many concurrent values",
                ..
            })
        ));
        assert_eq!(ours, before);
        // Within the caps, the same state merges.
        ours.merge(&crowded(1), &device(8)).expect("idempotent");
        assert_eq!(ours, before);
    }

    #[test]
    fn merge_that_would_be_too_large_to_publish_is_refused() {
        let mut big = Replica::new(1);
        let notes = "n".repeat(2000);
        for i in 0..2100 {
            big.write(
                RecordKey::HostMeta {
                    alias: format!("h{i}"),
                    field: MetaField::Notes,
                },
                RecordValue::Text(notes.clone()),
            );
        }
        big.state.check_invariants().expect("counts are fine");
        let mut target = Replica::new(2);
        target.write(host_key("p"), host("h"));
        let before = target.state.clone();
        assert!(matches!(
            target.state.merge(&big.state, &device(1)),
            Err(SyncError::MergeRefused { .. })
        ));
        assert_eq!(target.state, before);
    }

    #[test]
    fn inflated_third_party_context_is_deferred_not_merged() {
        use super::super::guard::{ClaimCheck, ContextClaim, check_context_claims};
        let (mut victim, mut observer) = (Replica::new(1), Replica::new(2));
        victim.write(host_key("prod"), host("10.0.0.1"));
        victim.write(tags_key("prod"), tags(&["db"]));
        observer.pull(&victim);
        // A compromised device publishes context[victim] = u64::MAX with
        // none of the victim's dots: every victim value looks overwritten.
        let mut attacker = Replica::new(5);
        attacker.write(host_key("other"), host("10.0.0.5"));
        attacker.state.context.insert(Dot {
            device: device(1),
            counter: u64::MAX,
        });
        attacker
            .state
            .check_invariants()
            .expect("structurally valid");
        let roster = BTreeSet::from([device(1), device(2), device(5)]);
        // Verified clocks: from each device's own authenticated snapshot.
        let verified = BTreeMap::from([
            (device(1), victim.clock.counter()),
            (device(2), observer.clock.counter()),
        ]);
        let check = check_context_claims(&device(5), attacker.state.context(), &roster, &verified);
        assert_eq!(
            check,
            ClaimCheck::Defer(vec![ContextClaim {
                device: device(1),
                claimed: u64::MAX,
                verified: 2,
                in_roster: true,
            }])
        );
        // Deferred: not merged, the victim's values survive.
        assert_eq!(
            observer.state.value(&host_key("prod")),
            Some(host("10.0.0.1"))
        );
        // Had it been merged, the report flags the anomaly.
        let mut forced = observer.state.clone();
        let report = forced.merge(&attacker.state, &device(5)).expect("merge");
        assert_eq!(report.dropped_live_without_successor, 2);
        assert_eq!(forced.register(&host_key("prod")), None);
        // The victim's clock is not marked restored by a third party.
        assert!(matches!(
            victim
                .clock
                .check_claim(attacker.state.context(), &device(5)),
            Err(SyncError::ThirdPartyClaim { .. })
        ));
        victim
            .clock
            .observe_own_context(victim.state.context())
            .expect("own evidence is consistent");
        victim.write(host_key("prod"), host("10.0.0.2"));
        // An honest snapshot is accepted.
        assert_eq!(
            check_context_claims(&device(2), observer.state.context(), &roster, &verified),
            ClaimCheck::Accept
        );
    }

    #[test]
    fn unknown_kind_edited_twice_relays_only_the_latest_and_stays_readable() {
        let key = serde_json::json!({"kind": "future_kind", "id": "x"});
        let (mut newer, mut older) = (Replica::new(2), Replica::new(1));
        write_wire(
            &mut newer,
            &key,
            &serde_json::json!({"type": "future", "v": 1}),
        );
        older.pull(&newer);
        let latest = write_wire(
            &mut newer,
            &key,
            &serde_json::json!({"type": "future", "v": 2}),
        );
        older.pull(&newer);
        // The older version holds it, relays it, never applies it.
        let projection = older.state.project();
        assert!(projection.known.is_empty());
        assert_eq!(
            projection.held,
            vec![HeldRecord {
                kind: "future_kind".into(),
                key: None
            }]
        );
        // Relayed through the older device's snapshot (JSON round trip).
        let json = serde_json::to_string(&older.state).expect("ser");
        let relayed: SyncState = serde_json::from_str(&json).expect("readable");
        relayed.check_invariants().expect("valid");
        let mut another_newer = Replica::new(3);
        another_newer
            .state
            .merge(&relayed, &device(1))
            .expect("no duplicate-key error");
        let wire_key = WireKey::from_json(&key).expect("key");
        let register = another_newer.state.records.get(&wire_key).expect("present");
        assert_eq!(register.entries().len(), 1, "only the latest edit");
        let (dot, entry) = register.entries().iter().next().expect("entry");
        assert_eq!(*dot, latest);
        assert_eq!(entry.value.as_str(), r#"{"type":"future","v":2}"#);
        // The newer device itself reads the relay back unchanged.
        let mut back = newer.state.clone();
        assert_eq!(
            back.merge(&relayed, &device(1)).expect("merge"),
            MergeReport::default()
        );
        assert_eq!(back, newer.state);
    }

    #[test]
    fn newer_field_on_a_known_kind_is_held_never_applied_and_relayed_intact() {
        let (mut older, mut newer) = (Replica::new(1), Replica::new(2));
        older.write(host_key("prod"), host("10.0.0.1"));
        newer.pull(&older);
        // A newer Portico added a field to host values and edited prod.
        let newer_value = serde_json::json!({"type": "host", "v": {
            "hostname": "10.0.0.9", "user": null, "port": null, "identity_file": null,
            "proxy_jump": null, "renamed_from": null, "color": "red"}});
        let key = serde_json::to_value(host_key("prod")).expect("key");
        write_wire(&mut newer, &key, &newer_value);
        older.pull(&newer);
        // Held: not a deletion, not an overwrite of the local record.
        assert_eq!(
            older.state.view(&host_key("prod")),
            Some(RegisterView::Held)
        );
        assert_eq!(older.state.value(&host_key("prod")), None);
        let projection = older.state.project();
        assert!(!projection.known.contains_key(&host_key("prod")));
        assert_eq!(
            projection.held,
            vec![HeldRecord {
                kind: "host".into(),
                key: Some(host_key("prod"))
            }]
        );
        assert!(older.state.conflicts().is_empty());
        // The older version cannot overwrite or delete it.
        let counter = older.clock.counter();
        assert_eq!(
            older
                .state
                .write(&mut older.clock, &host_key("prod"), &host("10.0.0.3"), 0),
            Err(SyncError::RecordHeld { kind: "host" })
        );
        assert!(
            older
                .state
                .delete(&mut older.clock, &host_key("prod"), 0)
                .is_err()
        );
        assert_eq!(older.clock.counter(), counter, "no dot spent");
        // Relayed intact to another newer device.
        let mut third = Replica::new(3);
        third.pull(&older);
        let register = third.state.register(&host_key("prod")).expect("relayed");
        let relayed = register.single().expect("one value");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(relayed.as_str()).expect("json"),
            newer_value
        );
        assert_eq!(
            third.state.register(&host_key("prod")),
            newer.state.register(&host_key("prod"))
        );
    }

    #[test]
    fn synced_identity_file_rejects_expansion_chars_and_trailing_dots() {
        for bad in [
            "~/.ssh/id_$USER",
            "~/.ssh/%h",
            "~/.ssh/id#1",
            "~/.ssh/a=b",
            "~/.ssh/\"id\"",
            "~/.ssh/'id'",
            "~/.ssh/`id`",
            "~/.ssh./id",
            "~/keys /id",
            "~/.ssh/id.",
            "~/.ssh/id ",
            "~/NUL",
            "~/.ssh/nul.key",
            "~/.ssh/com1",
            "~/.ssh/COM0.pem",
            "~/.ssh/LPT9",
            "~/.ssh/con .txt",
            "~/aux/id",
            "~/.ssh/a|b",
            "~/.ssh/a?b",
            "~/.ssh/a*b",
            "~/.ssh/a<b>",
            "~/x\ty",
        ] {
            assert!(
                matches!(
                    HomeRelPath::parse(bad),
                    Err(SyncError::InvalidRecord {
                        field: "identity_file",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        for good in [
            "~/.ssh/id_ed25519",
            "~/.ssh/console",
            "~/.ssh/com10",
            "~/.ssh/nul-key",
        ] {
            HomeRelPath::parse(good).expect(good);
        }
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
        // Duplicate keys merge their entries (finding 4); a repeated dot
        // with the same value is one entry, with another value an error.
        let entry =
            format!(r#"{{"dot":{{"d":"{dev}","c":1}},"at":0,"value":{{"type":"tombstone"}}}}"#);
        let other_value =
            format!(r#"{{"dot":{{"d":"{dev}","c":1}},"at":0,"value":{{"type":"bool","v":true}}}}"#);
        let record = format!(r#"{{"key":{{"kind":"host","alias":"p"}},"entries":[{entry}]}}"#);
        let twice = parse(format!(
            r#"{{"context":{{"{dev}":1}},"records":[{record},{record}]}}"#
        ))
        .expect("duplicate keys merge");
        assert_eq!(twice.len(), 1);
        assert_eq!(
            twice.register(&host_key("p")).map(|r| r.entries().len()),
            Some(1)
        );
        // Key fields in another order are the same canonical key.
        let reordered = format!(r#"{{"key":{{"alias":"p","kind":"host"}},"entries":[{entry}]}}"#);
        let same = parse(format!(
            r#"{{"context":{{"{dev}":1}},"records":[{record},{reordered}]}}"#
        ))
        .expect("parse");
        assert_eq!(same, twice);
        assert!(
            parse(format!(
                r#"{{"context":{{"{dev}":1}},"records":[{{"key":{{"kind":"host","alias":"p"}},"entries":[{entry},{other_value}]}}]}}"#
            ))
            .is_err()
        );
        // Without the generic record shape: refused.
        for bad in [
            r#"{"context":{},"records":[{"key":{"alias":"p"},"entries":[]}]}"#,
            r#"{"context":{},"records":[{"key":"p","entries":[]}]}"#,
            r#"{"context":{},"records":[{"key":{"kind":""},"entries":[]}]}"#,
            r#"{"context":{},"records":[{"key":{"kind":"host","alias":"p"},"entries":[],"extra":1}]}"#,
            r#"{"context":{},"records":[],"extra":1}"#,
        ] {
            assert!(parse(bad.to_owned()).is_err(), "{bad}");
        }
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
                    if let Some(register) = replica.state.register(&key_of(key)) {
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
                        .register(&key_of(key))
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
                forward.merge(&r.state, &r.clock.device()).expect("within caps");
            }
            let mut shuffled = SyncState::default();
            for &index in &order {
                let source = history.replicas[index].clock.device();
                shuffled.merge(&history.replicas[index].state, &source).expect("within caps");
                // Duplicated deliveries change nothing.
                shuffled.merge(&history.replicas[index].state, &source).expect("within caps");
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
                all.merge(&r.state, &r.clock.device()).expect("within caps");
            }
            for written in &history.writes {
                let present = all
                    .register(&written.key)
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
            let conflict = a.state.register(&key_of(0)).is_some_and(MvRegister::is_conflict);
            prop_assert_eq!(conflict, !sync_first && v1 != v2);
        }
    }
}
