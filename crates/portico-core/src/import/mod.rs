//! Imports from other SSH managers (Phase 5): Termius, see [`termius`].
//!
//! ## Two steps: plan, then apply
//!
//! [`crate::Engine::plan_termius_import`] reads an export and returns an
//! [`ImportPlan`] without writing anything: the hosts that would be
//! imported (alias, `HostName`, `User`, `Port`, group, tags), the records
//! skipped and why, the warnings, and how many secrets were found and
//! ignored. [`crate::Engine::apply_import`] then writes it and returns an
//! [`ImportReport`].
//!
//! ## Invariants
//!
//! - `~/.ssh/config` is **never** written. The hosts go to
//!   `<config_dir>/imports/termius.conf` (atomic, `0600`, directory
//!   `0700`), replaced in full on each import; the user adds the
//!   [`ImportPlan::include_line`] at the top of `~/.ssh/config` themselves.
//! - Only `Host`, `HostName`, `User` and `Port` are emitted. Every value is
//!   checked against a strict character set before it reaches the file
//!   (no space, line break, control character, `#`, quote or `%`): an
//!   export can never inject a directive such as `ProxyCommand`.
//! - No secret is imported, kept or logged: password and key cells are
//!   only tested for emptiness (and wiped from memory, `zeroize`); a cell
//!   that holds a private key in any other column is dropped. Counts are
//!   reported, never values. Errors and notices never quote a cell.
//! - Applying the same plan twice changes nothing: the file is rewritten
//!   only when a host changed (the import date aside), and metadata is
//!   merged (tags united, group and notes only set when missing).
//! - An alias already defined in the user's own config (outside the import
//!   file) is never shadowed: that record is skipped, unless the
//!   [`ImportOptions::prefix`] option gives every alias a `termius-` prefix.

pub(crate) mod csv;
pub mod termius;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::Result;
use crate::hosts::{HostMeta, ImportedMeta, merge_imported_meta};
use crate::ssh_config::SshConfig;
use crate::store::{self, Paths};

pub use termius::TermiusFormat;

/// Largest export file read, in bytes.
pub const MAX_IMPORT_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Longest alias generated, in characters.
pub const MAX_ALIAS_CHARS: usize = 64;
/// Prefix given to every alias by [`ImportOptions::prefix`].
pub const IMPORT_PREFIX: &str = "termius-";
/// Most warnings listed in a plan or report; the rest are counted in one
/// [`NoticeCode::NoticesTruncated`] notice.
pub const MAX_NOTICES: usize = 500;
/// Largest generated config file, in bytes.
const MAX_IMPORT_CONF_BYTES: u64 = 4 * 1024 * 1024;
/// The header line holding the import date: the only line allowed to
/// differ for a re-import to count as unchanged.
const DATE_LINE_PREFIX: &str = "# Imported on ";

/// Import choices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Prefix every alias with [`IMPORT_PREFIX`] (no collision with the
    /// user's own aliases).
    pub prefix: bool,
}

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
    /// Termius group path (`Production/EU`), for `hosts.toml`.
    pub group: Option<String>,
    /// Normalized tags, for `hosts.toml`.
    pub tags: Vec<String>,
    /// `Termius: <label>` when the alias differs from the label.
    pub notes: Option<String>,
    /// Line (CSV, ssh config) or position (JSON) of the record, 1-based.
    pub record: usize,
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
    /// The alias is already defined in the user's ssh config: skipped.
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
    /// elsewhere): it was given the `termius-` prefix.
    AliasLooksLikeHost,
    /// Too many notices: the rest are counted, not listed.
    NoticesTruncated,
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
        }
    }
}

/// A skipped record or a warning. Never holds a secret: `label` is the
/// record's label (cleaned), `message` names fields, not cell values
/// (except tags, groups and port numbers, which are not secret).
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

/// Secrets found in the export and **not** imported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredSecrets {
    pub passwords: usize,
    pub keys: usize,
}

impl IgnoredSecrets {
    pub fn total(self) -> usize {
        self.passwords.saturating_add(self.keys)
    }

    /// Advice shown when the export held secrets (`None` otherwise).
    pub fn advice(self) -> Option<String> {
        if self.total() == 0 {
            return None;
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        Some(format!(
            "{} and {} ignored (never imported). The export holds them in clear text: delete it once the import is done, and keep secrets in your keychain or ssh-agent.",
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
    pub format: TermiusFormat,
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
    /// The file the hosts are written to.
    pub target: PathBuf,
    /// The line to add at the top of `~/.ssh/config`.
    pub include_line: String,
}

/// What an applied import did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub format: TermiusFormat,
    /// The file written.
    pub target: PathBuf,
    /// `true` when the file already held exactly these hosts (not rewritten).
    pub unchanged: bool,
    /// Aliases written, in file order.
    pub imported: Vec<String>,
    pub skipped: Vec<ImportNotice>,
    pub warnings: Vec<ImportNotice>,
    pub secrets: IgnoredSecrets,
    pub identity_files: Vec<String>,
    /// Aliases whose `hosts.toml` metadata changed.
    pub meta_updated: usize,
    /// The line to add at the top of `~/.ssh/config`.
    pub include_line: String,
    /// `true` when `~/.ssh/config` already includes the file (its hosts
    /// are visible): nothing left to do.
    pub include_active: bool,
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
    #[error("{} is not a Termius export Portico recognizes", path.display())]
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
    /// `hosts.toml` was updated, then the config file could not be written.
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
                Some(
                    "A Termius export of a few thousand hosts is far smaller: is this the right file?",
                ),
            ),
            Self::Encoding { .. } => (
                "import_encoding",
                Some("Save the file again as CSV UTF-8 from your spreadsheet."),
            ),
            Self::UnknownFormat { .. } => (
                "import_unknown_format",
                Some(
                    "Use a CSV with the Termius import template header (Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,…), a termius-local-export hosts.json or credentials.csv, or the output of `termius export-ssh-config`. See docs/IMPORT.md.",
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
fn is_valid_alias(alias: &str) -> bool {
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
/// every tool reading the config. Aliases already under
/// [`IMPORT_PREFIX`] are left alone.
pub(crate) fn looks_like_other_host(alias: &str, hostname: &str) -> bool {
    alias.contains(['.', ':'])
        && !alias.eq_ignore_ascii_case(hostname)
        && !alias.starts_with(IMPORT_PREFIX)
}

/// `true` when a cell holds private key material.
pub(crate) fn looks_like_secret(value: &str) -> bool {
    value.contains("-----BEGIN") || value.contains("PRIVATE KEY")
}

// ── Target file and Include line ────────────────────────────────────────

/// `<config_dir>/imports/termius.conf`.
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

/// Lowercased aliases of the user's ssh config, except the occurrences
/// declared in `exclude` (the import file itself: re-importing never
/// collides with the previous import). Every occurrence counts, not only
/// the first: a host the user copied into their own config *after* the
/// `Include` is still seen, so the next import leaves it out of the file.
pub(crate) fn existing_aliases(paths: &Paths, exclude: &Path) -> Result<HashSet<String>> {
    let config = SshConfig::load(&paths.ssh_config(), &paths.ssh_dir)?;
    Ok(config
        .all_concrete_aliases()
        .into_iter()
        .filter(|a| !same_file(&a.source.file, exclude))
        .map(|a| a.name.to_lowercase())
        .collect())
}

fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The config file text for `hosts` (only `Host`, `HostName`, `User`,
/// `Port`; values already checked). The `Include` line shown in the header
/// is computed again from `paths`, never taken from the plan.
fn render(paths: &Paths, plan: &ImportPlan, hosts: &[PlannedHost], now: OffsetDateTime) -> String {
    use std::fmt::Write as _;

    let date = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let source = plan
        .source
        .file_name()
        .map(|n| display_text(&n.to_string_lossy(), 120))
        .unwrap_or_default();
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# Hosts imported from Termius by Portico ({source}, {}).",
        plan.format.label()
    );
    let _ = writeln!(text, "{DATE_LINE_PREFIX}{date}.");
    let _ = writeln!(
        text,
        "# Replaced in full on each import. To edit a host, copy it to ~/.ssh/config,"
    );
    let _ = writeln!(
        text,
        "# then import again: a host defined there is left out of this file."
    );
    let _ = writeln!(
        text,
        "# Portico never edits ~/.ssh/config: add this line at its top yourself:"
    );
    let _ = writeln!(text, "#   {}", include_line(paths, &termius_target(paths)));
    for host in hosts {
        let _ = writeln!(text, "\nHost {}", host.alias);
        let _ = writeln!(text, "    HostName {}", host.hostname);
        if let Some(user) = &host.user {
            let _ = writeln!(text, "    User {user}");
        }
        if let Some(port) = host.port {
            let _ = writeln!(text, "    Port {port}");
        }
    }
    text
}

/// `text` without its import-date line.
fn without_date(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with(DATE_LINE_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The hosts of `plan` that can be written, checked again: a value
/// Portico does not write, a duplicate or an alias now defined in the
/// user's config is skipped; an alias that looks like another host gets
/// the prefix.
fn checked_hosts(
    plan: &ImportPlan,
    existing: &HashSet<String>,
    skipped: &mut Vec<ImportNotice>,
    warnings: &mut BoundedNotices,
) -> Vec<PlannedHost> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut hosts: Vec<PlannedHost> = Vec::new();
    for planned in &plan.hosts {
        let notice = |code, message: &str| ImportNotice {
            code,
            record: Some(planned.record),
            label: Some(planned.label.clone()),
            message: message.to_owned(),
        };
        let mut host = planned.clone();
        if looks_like_other_host(&host.alias, &host.hostname) {
            host.alias = slug(&format!("{IMPORT_PREFIX}{}", host.alias), MAX_ALIAS_CHARS);
            if host.notes.is_none() {
                host.notes = Some(format!("Termius: {}", host.label));
            }
            warnings.push(notice(
                NoticeCode::AliasLooksLikeHost,
                "this alias looks like another host's name: imported with the termius- prefix",
            ));
        }
        let valid = is_valid_alias(&host.alias)
            && seen.insert(host.alias.to_lowercase())
            && clean_hostname(&host.hostname).as_deref() == Some(host.hostname.as_str())
            && host
                .user
                .as_ref()
                .is_none_or(|u| clean_user(u).as_deref() == Some(u.as_str()))
            && host.port != Some(0);
        if !valid {
            skipped.push(notice(
                NoticeCode::InvalidHostname,
                "this host has a value Portico does not write",
            ));
        } else if existing.contains(&host.alias.to_lowercase()) {
            skipped.push(notice(
                NoticeCode::AliasCollision,
                "this alias is now defined in your ssh config (import with the termius- prefix to keep both)",
            ));
        } else {
            hosts.push(host);
        }
    }
    hosts
}

/// Writes `plan` (blocking). Values are checked again (a plan is plain
/// data), aliases defined in the user's config since the plan was made are
/// skipped, and an alias that looks like another host gets the prefix.
///
/// All or nothing as far as Portico can tell: the existing config file is
/// read and the new text built first, then the metadata is merged (size
/// checked before anything is written), and the config file is written
/// last. Should that last write fail, the error says `hosts.toml` was
/// already updated ([`ImportError::ConfNotWritten`]).
pub(crate) fn apply(
    paths: &Paths,
    plan: &ImportPlan,
    now: OffsetDateTime,
) -> Result<(ImportReport, Vec<(String, HostMeta)>)> {
    let target = termius_target(paths);
    let existing = existing_aliases(paths, &target)?;
    let mut skipped = plan.skipped.clone();
    let mut warnings = BoundedNotices::from_vec(plan.warnings.clone());
    let hosts = checked_hosts(plan, &existing, &mut skipped, &mut warnings);

    let text = render(paths, plan, &hosts, now);
    let previous = store::read_capped(&target, MAX_IMPORT_CONF_BYTES)?;
    let unchanged = previous
        .as_deref()
        .is_some_and(|p| without_date(p) == without_date(&text));
    if !unchanged && u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_IMPORT_CONF_BYTES {
        return Err(crate::Error::WriteTooLarge {
            path: target,
            max: MAX_IMPORT_CONF_BYTES,
        });
    }

    let entries: Vec<ImportedMeta> = hosts
        .iter()
        .map(|h| ImportedMeta {
            alias: h.alias.clone(),
            tags: h.tags.clone(),
            group: h.group.clone(),
            notes: h.notes.clone(),
        })
        .collect();
    let merged = merge_imported_meta(&paths.hosts_meta(), &entries)?;
    if !unchanged {
        store::write_atomic_capped(&target, text.as_bytes(), MAX_IMPORT_CONF_BYTES).map_err(
            |e| ImportError::ConfNotWritten {
                target: target.clone(),
                meta: paths.hosts_meta(),
                meta_updated: merged.changed.len(),
                reason: e.to_string(),
            },
        )?;
    }
    for alias in &merged.tags_truncated {
        warnings.push(ImportNotice {
            code: NoticeCode::TagsTruncated,
            record: None,
            label: Some(alias.clone()),
            message: "this host already has 16 tags: some imported tags were left out".to_owned(),
        });
    }

    let include_active = SshConfig::load(&paths.ssh_config(), &paths.ssh_dir).is_ok_and(|c| {
        c.host_aliases()
            .iter()
            .any(|a| same_file(&a.source.file, &target))
    });
    let report = ImportReport {
        format: plan.format,
        target: target.clone(),
        unchanged,
        imported: hosts.iter().map(|h| h.alias.clone()).collect(),
        skipped,
        warnings: warnings.into_vec(),
        secrets: plan.secrets,
        identity_files: plan.identity_files.clone(),
        meta_updated: merged.changed.len(),
        include_line: include_line(paths, &target),
        include_active,
    };
    Ok((report, merged.changed))
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
            include_line(&paths, &termius_target(&paths)),
            "Include ~/config/imports/termius.conf"
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
        }
        .advice()
        .expect("advice");
        assert!(
            advice.starts_with("2 passwords and 1 key ignored"),
            "{advice}"
        );
        assert!(advice.contains("delete it"), "{advice}");
    }
}
