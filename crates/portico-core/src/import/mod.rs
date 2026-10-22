//! Imports from other SSH managers: Termius ([`termius`]) and Termix
//! ([`termix`]). ADRs "Import Termius" and "Import Termix et imports vers
//! les hôtes Portico".
//!
//! ## Two steps: plan, then apply
//!
//! [`crate::Engine::plan_import`] (or the per-source
//! [`crate::Engine::plan_termius_import`] / [`crate::Engine::plan_termix_import`])
//! reads an export and returns an [`ImportPlan`] without writing anything:
//! the hosts that would be imported (alias, `HostName`, `User`, `Port`,
//! group, tags, favorite, key), the private keys found (type, fingerprint,
//! hosts using them, a local copy), the records skipped and why, the
//! warnings, and how many secrets were found. [`crate::Engine::apply_import`]
//! then writes it with the [`ImportOptions`] chosen by the user (which keys
//! to import, a passphrase) and returns an [`ImportReport`].
//!
//! ## Invariants
//!
//! - `~/.ssh/config` is **never** written. Imported hosts become **Portico
//!   hosts** in `<config_dir>/hosts.conf` ([`crate::managed_hosts`]): the
//!   same lock, strict reading, allowlists and events as hosts created in
//!   the UIs. An old `imports/termius.conf` stays read as it is.
//! - Only `Host`, `HostName`, `User`, `Port`, and for an imported key
//!   `IdentityFile` + `IdentitiesOnly yes`, are written. Every value is
//!   checked against a strict character set (no space, line break, control
//!   character, `#`, quote or `%`): an export can never inject a directive.
//! - A re-import updates only the hosts it created and nobody edited since
//!   (provenance and a hash of the written values in `hosts.toml`, see
//!   [`crate::hosts`]); an alias defined anywhere else, or edited by hand,
//!   is never touched: that record is skipped, unless the
//!   [`ImportOptions::prefix`] option prefixes every alias.
//! - No password is imported, kept or logged: password cells and fields
//!   are only tested for emptiness (and wiped from memory, `zeroize`).
//!   Counts are reported, never values. Errors and notices never quote a
//!   cell or a key.
//! - Private keys (Termix) are imported **only when the user selects
//!   them** ([`KeySelection`]). Their text lives in `Zeroizing` memory held
//!   by the plan ([`KeyMaterial`]: never serialized, `Debug` redacted) and
//!   is written to `~/.ssh/portico/` only (`0600`, directory `0700`,
//!   `create_new`, never overwritten), unless a local key with the same
//!   fingerprint already exists. See [`private_keys`].

mod apply;
mod build;
pub(crate) mod csv;
pub(crate) mod private_keys;
pub mod termius;
pub mod termix;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ssh_config::SshConfig;
use crate::store::{Paths, same_file};

pub(crate) use apply::apply;
pub(crate) use build::plan;
pub use private_keys::{IMPORTED_KEYS_DIR, KeyFormat};

/// Largest export file read, in bytes.
pub const MAX_IMPORT_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Most records (rows, hosts, `Host` blocks) read from one export.
pub const MAX_IMPORT_RECORDS: usize = csv::MAX_RECORDS;
/// Longest alias generated, in characters.
pub const MAX_ALIAS_CHARS: usize = 64;
/// Prefix given to every alias of a Termius import by
/// [`ImportOptions::prefix`] (see [`ImportSource::prefix`]).
pub const IMPORT_PREFIX: &str = "termius-";
/// Most warnings listed in a plan or report; the rest are counted in one
/// [`NoticeCode::NoticesTruncated`] notice.
pub const MAX_NOTICES: usize = 500;

// ── Sources and formats ─────────────────────────────────────────────────

/// The tool an export comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportSource {
    Termius,
    Termix,
}

impl ImportSource {
    /// Stable name, stored as the provenance of imported hosts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Termius => "termius",
            Self::Termix => "termix",
        }
    }

    /// Human name (`Termius`), also the start of generated notes.
    pub fn label(self) -> &'static str {
        match self {
            Self::Termius => "Termius",
            Self::Termix => "Termix",
        }
    }

    /// Prefix of [`ImportOptions::prefix`] (`termius-`, `termix-`).
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Termius => IMPORT_PREFIX,
            Self::Termix => "termix-",
        }
    }
}

/// Which kind of export a file is (detected from its content).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportFormat {
    /// CSV in the layout of Termius' import template.
    TemplateCsv,
    /// `credentials.csv` of `termius-local-export`.
    LocalExportCsv,
    /// `hosts.json` of `termius-local-export`.
    LocalExportJson,
    /// `termius export-ssh-config` (archived CLI).
    SshConfig,
    /// Termix JSON export (`{"hosts":[{"name","ip",…}]}`).
    TermixJson,
}

/// The formats of the first importer, kept as a name for compatibility.
pub type TermiusFormat = ImportFormat;

impl ImportFormat {
    /// Human name, for reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::TemplateCsv => "Termius CSV",
            Self::LocalExportCsv => "termius-local-export credentials.csv",
            Self::LocalExportJson => "termius-local-export hosts.json",
            Self::SshConfig => "termius export-ssh-config",
            Self::TermixJson => "Termix JSON",
        }
    }

    /// The tool that writes this format.
    pub fn source(self) -> ImportSource {
        match self {
            Self::TermixJson => ImportSource::Termix,
            Self::TemplateCsv | Self::LocalExportCsv | Self::LocalExportJson | Self::SshConfig => {
                ImportSource::Termius
            }
        }
    }
}

// ── Keys ────────────────────────────────────────────────────────────────

/// Opaque identifier of a private key of an export: its `SHA256:…`
/// fingerprint (public, stable across re-plans of the same file).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyId(String);

impl KeyId {
    /// A key id as shown by [`PlannedKey::id`] (UIs send it back).
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which private keys of the plan to import ([`ImportOptions::keys`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "ids")]
pub enum KeySelection {
    /// No key (the default: nothing is ticked).
    #[default]
    None,
    /// These keys ([`PlannedKey::id`]); unknown ids are ignored.
    Some(BTreeSet<KeyId>),
}

impl KeySelection {
    /// `true` when `id` is selected.
    pub fn contains(&self, id: &KeyId) -> bool {
        match self {
            Self::None => false,
            Self::Some(ids) => ids.contains(id),
        }
    }
}

/// A private key found in an export. Never holds key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedKey {
    pub id: KeyId,
    /// `ssh-ed25519`, `ssh-rsa`, `ecdsa-sha2-nistp256`…
    pub algorithm: String,
    /// `SHA256:…`, as printed by `ssh-keygen -l`.
    pub fingerprint: String,
    pub format: KeyFormat,
    /// Encrypted in the export (written as it is, the passphrase is not
    /// changed).
    pub encrypted: bool,
    /// Aliases of the planned hosts using it.
    pub aliases: Vec<String>,
    /// A local private key with the same fingerprint (`~/.ssh`,
    /// `~/.ssh/portico`): reused instead of writing a copy.
    pub local: Option<PathBuf>,
}

/// The private keys of a plan, held in memory (`Zeroizing`) for
/// [`crate::Engine::apply_import`]. Opaque: never serialized, `Debug`
/// shows a count only; dropping the last clone of the plan wipes them.
/// [`Default`] is empty (a plan built by hand imports no key).
#[derive(Clone, Default)]
pub struct KeyMaterial(Arc<BTreeMap<KeyId, private_keys::KeySecret>>);

impl KeyMaterial {
    pub(crate) fn new(keys: BTreeMap<KeyId, private_keys::KeySecret>) -> Self {
        Self(Arc::new(keys))
    }

    pub(crate) fn get(&self, id: &KeyId) -> Option<&private_keys::KeySecret> {
        self.0.get(id)
    }

    /// Number of keys held.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyMaterial({} keys, redacted)", self.0.len())
    }
}

impl PartialEq for KeyMaterial {
    fn eq(&self, other: &Self) -> bool {
        self.0.keys().eq(other.0.keys())
    }
}

impl Eq for KeyMaterial {}

// ── Options, plan, report ───────────────────────────────────────────────

/// Import choices. When planning, only [`Self::prefix`] is read; when
/// applying, [`Self::keys`] and [`Self::key_passphrase`] (and a
/// [`Self::prefix`] the plan did not have yet).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Prefix every alias with the source's prefix (`termius-`,
    /// `termix-`): no collision with the user's own aliases.
    pub prefix: bool,
    /// Private keys to import (none by default).
    #[serde(default)]
    pub keys: KeySelection,
    /// Encrypts the imported keys that are not encrypted in the export
    /// (empty: none). Never serialized.
    #[serde(skip)]
    pub key_passphrase: Option<SecretString>,
}

impl ImportOptions {
    /// The options a plan keeps: the prefix only.
    pub(crate) fn for_plan(&self) -> Self {
        Self {
            prefix: self.prefix,
            keys: KeySelection::None,
            key_passphrase: None,
        }
    }
}

impl PartialEq for ImportOptions {
    fn eq(&self, other: &Self) -> bool {
        self.prefix == other.prefix
            && self.keys == other.keys
            && self.key_passphrase.is_some() == other.key_passphrase.is_some()
    }
}

impl Eq for ImportOptions {}

/// One host of a plan, as it will be written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedHost {
    /// `Host` line: `[a-z0-9_.-]`, unique in the import.
    pub alias: String,
    /// Label in the export (control characters removed).
    pub label: String,
    pub hostname: String,
    pub user: Option<String>,
    /// `None` for 22 (left out of the file).
    pub port: Option<u16>,
    /// Group path (`Production/EU`), for `hosts.toml`.
    pub group: Option<String>,
    /// Normalized tags, for `hosts.toml`.
    pub tags: Vec<String>,
    /// `Termius: <label>` / `Termix: <label>` when the alias differs from
    /// the label.
    pub notes: Option<String>,
    /// Line (CSV, ssh config) or position (JSON) of the record, 1-based.
    pub record: usize,
    /// Pinned in the export: marked as a favorite when created.
    pub favorite: bool,
    /// The private key this host uses in the export ([`ImportPlan::keys`]),
    /// written as its `IdentityFile` only when selected.
    pub key: Option<KeyId>,
    /// `true` when it updates a host an earlier import of the same source
    /// created (and nobody edited since); `false` when it is created.
    pub update: bool,
    /// Stable id of the record in the export (Termix `id`, otherwise
    /// `label:<slug of the label>`), kept as the host's provenance.
    pub source_id: String,
}

/// Why a notice was raised (stable codes for UIs and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NoticeCode {
    /// Not an SSH host (telnet, mosh…): skipped.
    UnsupportedProtocol,
    /// No host name or address: skipped.
    MissingHostname,
    /// The host name has a character not allowed (space, line break…): skipped.
    InvalidHostname,
    /// The user name has a character not allowed: skipped.
    InvalidUser,
    /// The port is not 1–65535: left out (22).
    InvalidPort,
    /// The alias is already defined elsewhere (ssh config, a Portico host):
    /// skipped.
    AliasCollision,
    /// Two records gave the same alias: a `-2`, `-3`… suffix was added.
    AliasRenamed,
    /// A tag was transliterated or slugged.
    TagNormalized,
    /// A tag could not be kept (nothing left after slugging, too many).
    TagDropped,
    /// The group was cut to 64 characters or cleaned.
    GroupTruncated,
    /// A private key was found in a non-secret column: the cell was dropped.
    SecretInField,
    /// An `IdentityFile` of the export was not imported.
    IdentityFileIgnored,
    /// A directive other than `HostName`, `User`, `Port` was not imported.
    IgnoredDirective,
    /// A column Portico does not read.
    UnknownColumn,
    /// The alias already had 16 tags: some imported tags were left out.
    TagsTruncated,
    /// A CSV row has more cells than the header, or fewer while the header
    /// has a secret column (a shifted row could move a password into
    /// another field): skipped.
    ColumnCountMismatch,
    /// The alias looks like another host's name (`github.com` pointing
    /// elsewhere): it was given the source's prefix.
    AliasLooksLikeHost,
    /// Too many notices: the rest are counted, not listed.
    NoticesTruncated,
    /// A setting Portico does not import (Termix tunnels, SOCKS5 chain).
    IgnoredSetting,
    /// A private key Portico cannot read (garbage, `PuTTY`, encrypted PEM
    /// without its passphrase): skipped, never quoted.
    KeyUnreadable,
    /// A key is stored for a host that does not use key authentication:
    /// not offered.
    KeyNotUsed,
    /// The host was imported earlier, then edited by hand: left as it is.
    EditedSinceImport,
    /// A selected key could not be imported (see the message).
    KeySkipped,
}

impl NoticeCode {
    /// The `snake_case` code.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::MissingHostname => "missing_hostname",
            Self::InvalidHostname => "invalid_hostname",
            Self::InvalidUser => "invalid_user",
            Self::InvalidPort => "invalid_port",
            Self::AliasCollision => "alias_collision",
            Self::AliasRenamed => "alias_renamed",
            Self::TagNormalized => "tag_normalized",
            Self::TagDropped => "tag_dropped",
            Self::GroupTruncated => "group_truncated",
            Self::SecretInField => "secret_in_field",
            Self::IdentityFileIgnored => "identity_file_ignored",
            Self::IgnoredDirective => "ignored_directive",
            Self::UnknownColumn => "unknown_column",
            Self::TagsTruncated => "tags_truncated",
            Self::ColumnCountMismatch => "column_count_mismatch",
            Self::AliasLooksLikeHost => "alias_looks_like_host",
            Self::NoticesTruncated => "notices_truncated",
            Self::IgnoredSetting => "ignored_setting",
            Self::KeyUnreadable => "key_unreadable",
            Self::KeyNotUsed => "key_not_used",
            Self::EditedSinceImport => "edited_since_import",
            Self::KeySkipped => "key_skipped",
        }
    }
}

/// A skipped record or a warning. Never holds a secret: `label` is the
/// record's label (cleaned), `message` names fields, not cell values
/// (except tags, groups, aliases and port numbers, which are not secret).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportNotice {
    pub code: NoticeCode,
    /// Line or position of the record, when the notice is about one.
    pub record: Option<usize>,
    pub label: Option<String>,
    pub message: String,
}

/// A list of notices bounded to [`MAX_NOTICES`]: notices past the bound
/// are only counted, then summed up in one last
/// [`NoticeCode::NoticesTruncated`] notice by [`Self::into_vec`].
#[derive(Debug, Default)]
pub(crate) struct BoundedNotices {
    kept: Vec<ImportNotice>,
    hidden: usize,
}

impl BoundedNotices {
    /// Takes over `notices` (from a plan: possibly already summed up, or
    /// tampered with and longer than the bound).
    pub(crate) fn from_vec(notices: Vec<ImportNotice>) -> Self {
        let mut bounded = Self::default();
        for notice in notices {
            if notice.code == NoticeCode::NoticesTruncated {
                bounded.hidden = bounded.hidden.saturating_add(hidden_count(&notice));
            } else {
                bounded.push(notice);
            }
        }
        bounded
    }

    pub(crate) fn push(&mut self, notice: ImportNotice) {
        if self.kept.len() < MAX_NOTICES {
            self.kept.push(notice);
        } else {
            self.hidden = self.hidden.saturating_add(1);
        }
    }

    /// The kept notices, then the summary when some were dropped.
    pub(crate) fn into_vec(self) -> Vec<ImportNotice> {
        let mut out = self.kept;
        if self.hidden > 0 {
            out.push(ImportNotice {
                code: NoticeCode::NoticesTruncated,
                record: None,
                label: None,
                message: format!("{} more warnings not listed", self.hidden),
            });
        }
        out
    }
}

/// The count held by a summary notice (its leading number).
fn hidden_count(notice: &ImportNotice) -> usize {
    let digits: String = notice
        .message
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(1)
}

/// Secrets found in the export. Counts only.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredSecrets {
    /// Passwords (and key passphrases) never imported.
    pub passwords: usize,
    /// Private keys not imported (unreadable, unused or not selected).
    pub keys: usize,
    /// Distinct private keys the export holds in clear text or encrypted,
    /// whether imported or not (Termix).
    pub keys_in_export: usize,
}

impl IgnoredSecrets {
    /// Secrets ignored (passwords and keys).
    pub fn total(self) -> usize {
        self.passwords.saturating_add(self.keys)
    }

    /// Advice shown when the export held secrets (`None` otherwise): it
    /// must be deleted once imported.
    pub fn advice(self) -> Option<String> {
        if self.total() == 0 && self.keys_in_export == 0 {
            return None;
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let holds = if self.keys_in_export > 0 {
            format!(
                "The export holds {} in clear text or encrypted",
                plural(self.keys_in_export, "private key", "private keys")
            )
        } else {
            "The export holds them in clear text".to_owned()
        };
        Some(format!(
            "{} and {} ignored (never imported). {holds}: delete it once the import is done, and keep secrets in your keychain or ssh-agent.",
            plural(self.passwords, "password", "passwords"),
            plural(self.keys, "key", "keys"),
        ))
    }
}

/// What an import would do. Nothing is written until
/// [`crate::Engine::apply_import`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    /// The export file.
    pub source: PathBuf,
    pub format: ImportFormat,
    /// The options the plan was made with (the prefix only).
    pub options: ImportOptions,
    /// Records (rows, JSON hosts, `Host` blocks) found in the export.
    pub records: usize,
    pub hosts: Vec<PlannedHost>,
    /// Records not imported, one notice each.
    pub skipped: Vec<ImportNotice>,
    pub warnings: Vec<ImportNotice>,
    pub secrets: IgnoredSecrets,
    /// `IdentityFile` paths of the export, not imported (they point to
    /// the exporting tool's own key folder).
    pub identity_files: Vec<String>,
    /// The file the hosts are written to (`hosts.conf`).
    pub target: PathBuf,
    /// The line to add at the top of `~/.ssh/config` so OpenSSH sees them.
    pub include_line: String,
    /// Hosts of an earlier import of the same source that this plan
    /// updates; `None` when there are none.
    pub replaces: Option<usize>,
    /// Private keys found (Termix), offered for import; none is selected
    /// by default.
    pub keys: Vec<PlannedKey>,
    /// The keys' material, for [`crate::Engine::apply_import`] only.
    #[serde(skip)]
    pub key_material: KeyMaterial,
}

impl ImportPlan {
    /// The tool the export comes from.
    pub fn source_kind(&self) -> ImportSource {
        self.format.source()
    }
}

/// What happened to the keys of a plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyOutcome {
    /// Key files written (`~/.ssh/portico/<alias>`, with a `.pub`).
    pub imported: Vec<PathBuf>,
    /// Local keys with the same fingerprint, used instead.
    pub reused: Vec<PathBuf>,
    /// Keys of the plan neither imported nor reused (not selected, or
    /// selected for hosts that were skipped).
    pub skipped: usize,
}

/// What an applied import did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub format: ImportFormat,
    /// The file written (`hosts.conf`).
    pub target: PathBuf,
    /// `true` when the file already held exactly these hosts (not rewritten).
    pub unchanged: bool,
    /// Aliases created or updated, in plan order.
    pub imported: Vec<String>,
    /// The aliases of [`Self::imported`] that updated an earlier import.
    pub updated: Vec<String>,
    pub skipped: Vec<ImportNotice>,
    pub warnings: Vec<ImportNotice>,
    pub secrets: IgnoredSecrets,
    pub identity_files: Vec<String>,
    /// Aliases whose `hosts.toml` metadata changed.
    pub meta_updated: usize,
    /// The line to add at the top of `~/.ssh/config`.
    pub include_line: String,
    /// `true` when `~/.ssh/config` has an `Include` that resolves to the
    /// file (its hosts are visible to OpenSSH): nothing left to do.
    pub include_active: bool,
    pub keys: KeyOutcome,
}

/// An export Portico cannot read. Messages name the file and position,
/// never a cell value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    #[error("{} does not exist", path.display())]
    NotFound { path: PathBuf },
    #[error("{} is larger than {max} bytes", path.display())]
    TooLarge { path: PathBuf, max: u64 },
    #[error("{} is not UTF-8 or UTF-16 text", path.display())]
    Encoding { path: PathBuf },
    #[error("{} is not a Termius or Termix export Portico recognizes", path.display())]
    UnknownFormat { path: PathBuf },
    #[error("{}:{line}: {reason}", path.display())]
    Csv {
        path: PathBuf,
        line: usize,
        reason: &'static str,
    },
    #[error("{}:{line}:{column}: invalid JSON", path.display())]
    Json {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    #[error("{} has more than {max} records", path.display())]
    TooManyRecords { path: PathBuf, max: usize },
    /// Kept for compatibility: `hosts.toml` was updated, then the config
    /// file could not be written.
    #[error(
        "{} was not written ({reason}); {} already holds the tags and groups of {meta_updated} imported hosts",
        target.display(),
        meta.display()
    )]
    ConfNotWritten {
        target: PathBuf,
        meta: PathBuf,
        meta_updated: usize,
        reason: String,
    },
    /// The hosts were written, then `hosts.toml` (tags, groups, provenance)
    /// could not be.
    #[error(
        "the hosts were written to {} but {} was not ({reason})",
        target.display(),
        meta.display()
    )]
    MetaNotWritten {
        target: PathBuf,
        meta: PathBuf,
        reason: String,
    },
}

impl ImportError {
    pub(crate) fn code_and_hint(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::NotFound { .. } => (
                "import_not_found",
                Some("Check the path of the export file."),
            ),
            Self::TooLarge { .. } => (
                "import_too_large",
                Some("An export of a few thousand hosts is far smaller: is this the right file?"),
            ),
            Self::Encoding { .. } => (
                "import_encoding",
                Some("Save the file again as CSV UTF-8 from your spreadsheet."),
            ),
            Self::UnknownFormat { .. } => (
                "import_unknown_format",
                Some(
                    "Use a CSV with the Termius import template header (Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,…), a termius-local-export hosts.json or credentials.csv, the output of `termius export-ssh-config`, or a Termix JSON export. See docs/IMPORT.md.",
                ),
            ),
            Self::Csv { .. } => (
                "import_csv",
                Some("Fix the reported line (an unclosed quote?) and retry."),
            ),
            Self::Json { .. } => (
                "import_json",
                Some("The JSON file is damaged: export it again."),
            ),
            Self::TooManyRecords { .. } => (
                "import_too_many",
                Some("Portico imports at most 5000 hosts from one export."),
            ),
            Self::ConfNotWritten { .. } => (
                "import_conf_not_written",
                Some(
                    "Fix the reported problem and import again: the metadata already written is merged, never duplicated.",
                ),
            ),
            Self::MetaNotWritten { .. } => (
                "import_meta_not_written",
                Some(
                    "The hosts are usable, but a re-import will treat them as created by hand: fix the reported problem, then edit their tags in Portico.",
                ),
            ),
        }
    }
}

// ── Values ──────────────────────────────────────────────────────────────

/// Text shown in notices and notes: control, bidi and invisible characters
/// removed, at most `max` characters.
pub(crate) fn display_text(raw: &str, max: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control() && !crate::snippets::is_hidden_format(*c))
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// ASCII for common Latin letters with diacritics (lowercase input).
fn transliterate(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ğ' | 'ģ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'ķ' => "k",
        'ł' | 'ľ' | 'ļ' | 'ĺ' => "l",
        'ñ' | 'ń' | 'ň' | 'ņ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ř' => "r",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'ß' => "ss",
        'ť' | 'ţ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Lowercase `[a-z0-9_.-]` slug of at most `max` characters: letters with
/// diacritics are transliterated, anything else becomes a `-` separator;
/// repeated separators collapse and `-`/`.` are trimmed at both ends.
pub(crate) fn slug(raw: &str, max: usize) -> String {
    let mut out = String::new();
    for c in raw.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            out.push(c);
        } else if let Some(ascii) = transliterate(c) {
            out.push_str(ascii);
        } else if !crate::snippets::is_hidden_format(c) && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut collapsed = String::with_capacity(out.len());
    for c in out.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let trim = |s: &str| s.trim_matches(|c| c == '-' || c == '.').to_owned();
    let cut: String = trim(&collapsed).chars().take(max).collect();
    trim(&cut)
}

/// `true` for an alias Portico generates (and accepts back in a plan).
pub(crate) fn is_valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.chars().count() <= MAX_ALIAS_CHARS
        && alias
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
        && !alias.starts_with(['-', '.'])
}

/// A `HostName` value: `[A-Za-z0-9._:-]`, at most 253 characters, not
/// starting with `-` or `.`. Surrounding brackets (`[2001:db8::1]`) are
/// removed. Anything else (space, line break, `#`, quote, `%` token…) is
/// refused: the value is written unquoted in an ssh config.
pub(crate) fn clean_hostname(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let value = trimmed
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(trimmed);
    let ok = !value.is_empty()
        && value.len() <= 253
        && !value.starts_with(['-', '.'])
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
    ok.then(|| value.to_owned())
}

/// A `User` value: `[A-Za-z0-9._@+-]`, at most 64 characters, not
/// starting with `-`.
pub(crate) fn clean_user(raw: &str) -> Option<String> {
    let value = raw.trim();
    let ok = !value.is_empty()
        && value.chars().count() <= 64
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | '+'));
    ok.then(|| value.to_owned())
}

/// `true` when `alias` reads like a host name (`github.com`, `10.0.0.1`)
/// other than `hostname`: `ssh <alias>` would then reach `hostname` for
/// every tool reading the config. Aliases already under `prefix` are left
/// alone.
pub(crate) fn looks_like_other_host(alias: &str, hostname: &str, prefix: &str) -> bool {
    alias.contains(['.', ':'])
        && !alias.eq_ignore_ascii_case(hostname)
        && !alias.starts_with(prefix)
}

/// `true` when a cell holds private key material.
pub(crate) fn looks_like_secret(value: &str) -> bool {
    value.contains("-----BEGIN") || value.contains("PRIVATE KEY")
}

// ── Files and Include line ──────────────────────────────────────────────

/// `<config_dir>/imports/termius.conf`: where the first Termius importer
/// wrote its hosts. Still read (origin `Imported`), never written again.
pub fn termius_target(paths: &Paths) -> PathBuf {
    paths.config_dir.join("imports").join("termius.conf")
}

/// The `Include` line for `target`: `~/…` when it is under the home
/// directory, double-quoted when it contains a space (macOS
/// `Application Support`).
pub fn include_line(paths: &Paths, target: &Path) -> String {
    let shown = match target.strip_prefix(&paths.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => target.display().to_string(),
    };
    if shown.chars().any(char::is_whitespace) {
        format!("Include \"{shown}\"")
    } else {
        format!("Include {shown}")
    }
}

/// `true` when the user's ssh config has an `Include` (nested ones too)
/// that resolves to `target`, whatever the file holds.
pub(crate) fn include_active(paths: &Paths, target: &Path) -> bool {
    SshConfig::load(&paths.ssh_config(), &paths.ssh_dir).is_ok_and(|c| includes(&c, target))
}

/// `true` when `config` has an `Include` (nested ones too) that resolves
/// to `target`.
pub(crate) fn includes(config: &SshConfig, target: &Path) -> bool {
    config
        .included_files()
        .iter()
        .any(|included| same_file(included, target))
}

// ── Who owns which alias ────────────────────────────────────────────────

/// Which aliases an import of `source` may write, computed under the
/// `hosts.conf` lock (at planning time too: the same answer as applying).
#[derive(Debug, Default)]
pub(crate) struct Occupancy {
    /// Lowercased aliases an import must not write: every occurrence in
    /// the configuration the engine loads (and the old Termius import
    /// file), except the hosts below that it may update.
    pub(crate) blocked: HashSet<String>,
    /// The subset of `blocked` that an earlier import of `source` created,
    /// then someone edited ([`NoticeCode::EditedSinceImport`]).
    pub(crate) edited: HashSet<String>,
    /// Lowercased aliases of `hosts.conf` that an earlier import of
    /// `source` created and nobody edited since: updated in place.
    pub(crate) updatable: HashSet<String>,
}

impl Occupancy {
    pub(crate) fn compute(
        target: &crate::managed_hosts::ImportTarget<'_>,
        paths: &Paths,
        source: ImportSource,
    ) -> Result<Self> {
        let provenance = crate::hosts::load_provenance(&paths.hosts_meta())?;
        let mut occupancy = Self::default();
        // `ProxyJump none` means "no jump host": never an alias Portico writes.
        occupancy.blocked.insert("none".to_owned());
        for host in target.hosts() {
            let Some(found) = provenance.get(&host.alias) else {
                continue;
            };
            if found.source != source.as_str() {
                continue;
            }
            let lower = host.alias.to_lowercase();
            if found.values == crate::managed_hosts::values_hash(&paths.home, host) {
                occupancy.updatable.insert(lower);
            } else {
                occupancy.edited.insert(lower);
            }
        }
        for alias in target.every_alias()? {
            let lower = alias.name.to_lowercase();
            let own = same_file(&alias.source.file, target.file())
                && occupancy.updatable.contains(&lower);
            if !own {
                occupancy.blocked.insert(lower);
            }
        }
        // An updatable alias also defined elsewhere is not updatable.
        let blocked = &occupancy.blocked;
        occupancy.updatable.retain(|a| !blocked.contains(a));
        Ok(occupancy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("web eu 1", 64), "web-eu-1");
        assert_eq!(slug("AT 198.51.100.7", 64), "at-198.51.100.7");
        assert_eq!(slug("Web Serveur", 32), "web-serveur");
        assert_eq!(slug("Équipe Données", 32), "equipe-donnees");
        assert_eq!(slug("  --db (eu)/main--  ", 64), "db-eu-main");
        assert_eq!(slug("Straße Œuvre", 64), "strasse-oeuvre");
        assert_eq!(slug("日本", 64), "");
        assert_eq!(slug("a\u{202e}b", 64), "ab");
        assert_eq!(slug(&"x".repeat(80), 64).len(), 64);
        assert_eq!(slug(&format!("{}-y", "x".repeat(63)), 64), "x".repeat(63));
    }

    #[test]
    fn hostnames_and_users() {
        assert_eq!(
            clean_hostname(" 203.0.113.10 ").as_deref(),
            Some("203.0.113.10")
        );
        assert_eq!(
            clean_hostname("2001:db8::10").as_deref(),
            Some("2001:db8::10")
        );
        assert_eq!(
            clean_hostname("[2001:db8::10]").as_deref(),
            Some("2001:db8::10")
        );
        assert_eq!(
            clean_hostname("staging.example.test").as_deref(),
            Some("staging.example.test")
        );
        for bad in [
            "",
            "a b",
            "192.0.2.61\nProxyCommand nc evil 22",
            "host#x",
            "-oProxyCommand=x",
            "h%h",
            "\"q\"",
            "fe80::1%eth0",
            "a\u{202e}b",
        ] {
            assert_eq!(clean_hostname(bad), None, "{bad:?}");
        }
        assert_eq!(clean_user("deploy").as_deref(), Some("deploy"));
        assert_eq!(
            clean_user("first.last@corp").as_deref(),
            Some("first.last@corp")
        );
        for bad in ["", "a b", "-l", "root\nProxyCommand x", "%u", "x#y", "'a'"] {
            assert_eq!(clean_user(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn include_lines() {
        let paths = Paths::rooted(Path::new("/home/ana"));
        assert_eq!(
            include_line(&paths, &paths.managed_hosts()),
            "Include ~/config/hosts.conf"
        );
        let mac = Path::new(
            "/Users/ana/Library/Application Support/dev.Portico.portico/imports/termius.conf",
        );
        let paths = Paths::rooted(Path::new("/Users/ana"));
        assert_eq!(
            include_line(&paths, mac),
            "Include \"~/Library/Application Support/dev.Portico.portico/imports/termius.conf\""
        );
        assert_eq!(
            include_line(&paths, Path::new("/opt/p/termius.conf")),
            "Include /opt/p/termius.conf"
        );
    }

    #[test]
    fn secrets_advice() {
        assert_eq!(IgnoredSecrets::default().advice(), None);
        let advice = IgnoredSecrets {
            passwords: 2,
            keys: 1,
            keys_in_export: 0,
        }
        .advice()
        .expect("advice");
        assert!(
            advice.starts_with("2 passwords and 1 key ignored"),
            "{advice}"
        );
        assert!(advice.contains("delete it"), "{advice}");
        // Every key imported, no password: still told to delete the export.
        let advice = IgnoredSecrets {
            passwords: 0,
            keys: 0,
            keys_in_export: 2,
        }
        .advice()
        .expect("advice");
        assert!(advice.contains("2 private keys"), "{advice}");
        assert!(advice.contains("delete it"), "{advice}");
    }

    #[test]
    fn options_never_serialize_the_passphrase() {
        let options = ImportOptions {
            prefix: true,
            keys: KeySelection::Some([KeyId::new("SHA256:x")].into()),
            key_passphrase: Some(SecretString::from("hunter2-passphrase")),
        };
        let json = serde_json::to_string(&options).expect("json");
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!format!("{options:?}").contains("hunter2"));
        assert_eq!(
            json,
            r#"{"prefix":true,"keys":{"kind":"some","ids":["SHA256:x"]}}"#
        );
        assert_eq!(options.for_plan().keys, KeySelection::None);
        assert!(options.for_plan().key_passphrase.is_none());
    }
}
