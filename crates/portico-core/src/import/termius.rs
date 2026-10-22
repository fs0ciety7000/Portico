//! Termius exports → records for [`super::ImportPlan`] (Phase 5).
//!
//! Termius has no official host export: Portico reads what users can
//! actually produce (see `docs/IMPORT.md`), detected from the content, not
//! the file name:
//!
//! | format | recognized by |
//! |---|---|
//! | [`ImportFormat::TemplateCsv`] | CSV header with `Hostname/IP` (Termius import template: `Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password,SSH_KEY`) |
//! | [`ImportFormat::LocalExportCsv`] | CSV header with `Alias` and `Host` (`termius-local-export` `credentials.csv`) |
//! | [`ImportFormat::LocalExportJson`] | JSON object with a `hosts` array (`termius-local-export` `hosts.json`) |
//! | [`ImportFormat::SshConfig`] | first directive is `Host` (archived `termius export-ssh-config`) |
//!
//! The template layout comes from unofficial sources: columns are found by
//! name (any order, any case, punctuation ignored), unknown columns are
//! reported and ignored. CSV may start with a UTF-8 BOM, use CRLF, quotes,
//! and `;` instead of `,` (spreadsheets in some locales). UTF-16 with a
//! BOM is decoded too.
//!
//! `termius export-ssh-config` writes labels as they are (`Host prod one`
//! means *two* patterns to OpenSSH): the whole rest of the line is taken
//! as the label. Its `identityfile ~/.termius/…` lines are listed, never
//! imported.
//!
//! Secrets: the `Password`, `SSH_KEY`, `password`, `identityFile`,
//! `PrivateKeyPath` (and similar) columns are only tested for emptiness;
//! the file text and every CSV cell are wiped from memory when the plan is
//! built (`Zeroizing`), JSON secret fields are never copied out of the
//! parser's input.

use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zeroize::Zeroizing;

use super::build::{Collected, MAX_TAGS_PER_CELL, RawHost};
use super::csv;
use super::{ImportError, ImportFormat, ImportNotice, NoticeCode, display_text, looks_like_secret};
use crate::error::Result;

/// Detects the format and reads every record.
pub(crate) fn read(text: &str, source: &Path) -> Result<(ImportFormat, Collected)> {
    let unknown = || ImportError::UnknownFormat {
        path: source.to_path_buf(),
    };
    if text.trim_start().starts_with('{') {
        return Ok((ImportFormat::LocalExportJson, read_json(text, source)?));
    }
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let delimiter = if first.matches(';').count() > first.matches(',').count() {
        ';'
    } else {
        ','
    };
    let header = csv::parse(first, delimiter).ok().and_then(|mut records| {
        let fields = records.pop()?.fields;
        Some(Columns::from_header(&fields))
    });
    if let Some(columns) = header.filter(Columns::is_host_table) {
        let format = if columns.template_header || columns.alias.is_none() {
            ImportFormat::TemplateCsv
        } else {
            ImportFormat::LocalExportCsv
        };
        return Ok((format, read_csv(text, delimiter, &columns, source)?));
    }
    let first_keyword = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| split_directive(l).0.to_ascii_lowercase());
    if first_keyword.as_deref() == Some("host") {
        return Ok((ImportFormat::SshConfig, read_ssh_config(text)));
    }
    Err(unknown().into())
}

// ── CSV ─────────────────────────────────────────────────────────────────

/// Column positions, found by name.
#[derive(Debug, Default)]
struct Columns {
    label: Option<usize>,
    alias: Option<usize>,
    hostname: Option<usize>,
    port: Option<usize>,
    user: Option<usize>,
    protocol: Option<usize>,
    group: Option<usize>,
    tags: Option<usize>,
    passwords: Vec<(usize, String)>,
    keys: Vec<(usize, String)>,
    unknown: Vec<String>,
    /// The header has `Hostname/IP` (Termius template).
    template_header: bool,
    names: Vec<String>,
}

impl Columns {
    fn from_header(fields: &[Zeroizing<String>]) -> Self {
        let mut columns = Self::default();
        for (index, field) in fields.iter().enumerate() {
            let shown = display_text(field, 32);
            let key: String = field
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect();
            let slot = match key.as_str() {
                "label" | "name" | "title" => &mut columns.label,
                "alias" => &mut columns.alias,
                "hostnameip" | "hostname" | "host" | "address" | "ip" | "ipaddress" => {
                    columns.template_header |= key == "hostnameip";
                    &mut columns.hostname
                }
                "port" => &mut columns.port,
                "username" | "user" | "login" => &mut columns.user,
                "protocol" => &mut columns.protocol,
                "groups" | "group" | "folder" => &mut columns.group,
                "tags" | "tag" => &mut columns.tags,
                "password" | "passphrase" | "pass" => {
                    columns.passwords.push((index, shown.clone()));
                    columns.names.push(shown);
                    continue;
                }
                "sshkey" | "key" | "privatekey" | "privatekeypath" | "identityfile" | "keyfile" => {
                    columns.keys.push((index, shown.clone()));
                    columns.names.push(shown);
                    continue;
                }
                "" => {
                    columns.names.push(shown);
                    continue;
                }
                _ => {
                    columns.unknown.push(shown.clone());
                    columns.names.push(shown);
                    continue;
                }
            };
            // The first column of a kind wins; a repeated one is unused.
            if slot.is_none() {
                *slot = Some(index);
            } else {
                columns.unknown.push(shown.clone());
            }
            columns.names.push(shown);
        }
        columns
    }

    fn is_host_table(&self) -> bool {
        self.hostname.is_some() && (self.label.is_some() || self.alias.is_some())
    }

    fn name(&self, index: usize) -> &str {
        self.names.get(index).map_or("", String::as_str)
    }
}

/// The value of a non-secret cell: the anti-formula apostrophe of
/// spreadsheet-safe exports is removed (only before `= + - @`), and a cell
/// holding a private key is counted and dropped.
fn text_cell(
    fields: &[Zeroizing<String>],
    column: Option<usize>,
    columns: &Columns,
    line: usize,
    collected: &mut Collected,
) -> String {
    let Some(index) = column else {
        return String::new();
    };
    let raw = fields.get(index).map_or("", |f| f.as_str());
    let value = match raw.strip_prefix('\'') {
        Some(rest) if rest.starts_with(['=', '+', '-', '@']) => rest,
        _ => raw,
    };
    if looks_like_secret(value) {
        collected.secrets.keys = collected.secrets.keys.saturating_add(1);
        collected.warn(
            NoticeCode::SecretInField,
            Some(line),
            format!(
                "a private key was found in the `{}` column: that cell was ignored",
                columns.name(index)
            ),
        );
        return String::new();
    }
    value.to_owned()
}

fn read_csv(text: &str, delimiter: char, columns: &Columns, source: &Path) -> Result<Collected> {
    let records = csv::parse(text, delimiter).map_err(|e| match e {
        csv::CsvError::UnterminatedQuote { line } => ImportError::Csv {
            path: source.to_path_buf(),
            line,
            reason: "a quoted cell is never closed",
        },
        csv::CsvError::TooManyFields { line } => ImportError::Csv {
            path: source.to_path_buf(),
            line,
            reason: "too many cells on this line",
        },
        csv::CsvError::TooManyRecords => ImportError::TooManyRecords {
            path: source.to_path_buf(),
            max: csv::MAX_RECORDS,
        },
    })?;
    let mut collected = Collected::default();
    if !columns.unknown.is_empty() {
        let names = columns
            .unknown
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        collected.warn(
            NoticeCode::UnknownColumn,
            Some(1),
            format!("columns not imported: {names}"),
        );
    }
    let header_cells = columns.names.len();
    let has_secret_column = !columns.passwords.is_empty() || !columns.keys.is_empty();
    for record in records.iter().skip(1) {
        let fields = record.fields.as_slice();
        if fields.iter().all(|f| f.trim().is_empty()) {
            continue;
        }
        let line = record.line;
        collected.records = collected.records.saturating_add(1);
        let present = |index: &usize| fields.get(*index).is_some_and(|f| !f.trim().is_empty());
        let passwords = columns.passwords.iter().filter(|(i, _)| present(i)).count();
        let keys = columns.keys.iter().filter(|(i, _)| present(i)).count();
        collected.secrets.passwords = collected.secrets.passwords.saturating_add(passwords);
        collected.secrets.keys = collected.secrets.keys.saturating_add(keys);
        // A row with a cell too many or too few is shifted: a password
        // could land in the user or host column. Only missing trailing
        // cells are tolerated, and only when no column holds a secret.
        let cells = fields.len();
        if cells > header_cells || (cells < header_cells && has_secret_column) {
            collected.skipped.push(ImportNotice {
                code: NoticeCode::ColumnCountMismatch,
                record: Some(line),
                label: None,
                message: format!(
                    "this row has {cells} cells and the header {header_cells}: its values could be in the wrong columns"
                ),
            });
            continue;
        }

        let mut cell = |column| text_cell(fields, column, columns, line, &mut collected);
        let label = cell(columns.label);
        let alias = cell(columns.alias);
        let hostname = cell(columns.hostname);
        let port = cell(columns.port);
        let user = cell(columns.user);
        let protocol = cell(columns.protocol);
        let group = cell(columns.group);
        let tags = cell(columns.tags);
        collected.raw.push(RawHost {
            record: line,
            label,
            alias: Some(alias).filter(|a| !a.trim().is_empty()),
            hostname,
            port,
            user,
            protocol,
            group,
            tags: tags
                .split(',')
                .take(MAX_TAGS_PER_CELL)
                .map(str::to_owned)
                .collect(),
            ..RawHost::default()
        });
    }
    Ok(collected)
}

// ── JSON (termius-local-export hosts.json) ──────────────────────────────

#[derive(Deserialize)]
struct JsonExport {
    hosts: Vec<JsonHost>,
}

/// Every field is optional and lenient (wrong types are ignored); secret
/// fields are only tested for presence.
#[derive(Default, Deserialize)]
#[serde(default)]
struct JsonHost {
    #[serde(deserialize_with = "text")]
    alias: Option<String>,
    #[serde(deserialize_with = "text")]
    label: Option<String>,
    #[serde(deserialize_with = "text", alias = "hostname", alias = "host")]
    address: Option<String>,
    #[serde(deserialize_with = "text")]
    port: Option<String>,
    #[serde(deserialize_with = "text", alias = "user")]
    username: Option<String>,
    #[serde(deserialize_with = "text")]
    group: Option<String>,
    #[serde(deserialize_with = "presence", alias = "passphrase")]
    password: bool,
    #[serde(
        rename = "identityFile",
        alias = "identity_file",
        alias = "privateKey",
        alias = "private_key",
        deserialize_with = "presence"
    )]
    identity_file: bool,
}

/// A string or number as text; anything else is `None`.
pub(crate) fn text<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    struct TextVisitor;
    impl<'de> Visitor<'de> for TextVisitor {
        type Value = Option<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or a number")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            Ok(Some(v.to_owned()))
        }
        fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }
    deserializer.deserialize_any(TextVisitor)
}

/// `true` when the value is present and not blank. The value itself is
/// never copied (an owned string is wiped).
fn presence<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<bool, D::Error> {
    struct PresenceVisitor;
    impl<'de> Visitor<'de> for PresenceVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("any value")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<bool, E> {
            Ok(!v.trim().is_empty())
        }
        fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<bool, E> {
            let v = Zeroizing::new(v);
            Ok(!v.trim().is_empty())
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<bool, E> {
            Ok(true)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<bool, E> {
            Ok(true)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<bool, E> {
            Ok(true)
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<bool, E> {
            Ok(v)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<bool, A::Error> {
            let mut any = false;
            while seq.next_element::<IgnoredAny>()?.is_some() {
                any = true;
            }
            Ok(any)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<bool, A::Error> {
            let mut any = false;
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                any = true;
            }
            Ok(any)
        }
    }
    deserializer.deserialize_any(PresenceVisitor)
}

fn read_json(text: &str, source: &Path) -> Result<Collected> {
    let export: JsonExport = serde_json::from_str(text).map_err(|e| match e.classify() {
        // Never the serde message: it can quote a value.
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            ImportError::Json {
                path: source.to_path_buf(),
                line: e.line(),
                column: e.column(),
            }
        }
        _ => ImportError::UnknownFormat {
            path: source.to_path_buf(),
        },
    })?;
    if export.hosts.len() > csv::MAX_RECORDS {
        return Err(ImportError::TooManyRecords {
            path: source.to_path_buf(),
            max: csv::MAX_RECORDS,
        }
        .into());
    }
    let mut collected = Collected::default();
    for (index, host) in export.hosts.into_iter().enumerate() {
        let record = index.saturating_add(1);
        collected.records = collected.records.saturating_add(1);
        if host.password {
            collected.secrets.passwords = collected.secrets.passwords.saturating_add(1);
        }
        if host.identity_file {
            collected.secrets.keys = collected.secrets.keys.saturating_add(1);
        }
        let mut field = |name: &str, value: Option<String>| {
            let value = value.unwrap_or_default();
            if looks_like_secret(&value) {
                collected.secrets.keys = collected.secrets.keys.saturating_add(1);
                collected.warn(
                    NoticeCode::SecretInField,
                    Some(record),
                    format!("a private key was found in the `{name}` field: it was ignored"),
                );
                return String::new();
            }
            value
        };
        let raw = RawHost {
            record,
            label: field("label", host.label),
            alias: Some(field("alias", host.alias)).filter(|a| !a.trim().is_empty()),
            hostname: field("address", host.address),
            port: field("port", host.port),
            user: field("username", host.username),
            protocol: String::new(),
            group: field("group", host.group),
            ..RawHost::default()
        };
        collected.raw.push(raw);
    }
    Ok(collected)
}

// ── ssh config fragment (termius export-ssh-config) ─────────────────────

/// `Keyword value`, `Keyword=value` or `Keyword = value`: the keyword and
/// the raw rest of the line (not split: labels keep their spaces).
fn split_directive(line: &str) -> (&str, &str) {
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let keyword = line.get(..end).unwrap_or(line);
    let rest = line.get(end..).unwrap_or("").trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    (keyword, rest)
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

fn read_ssh_config(text: &str) -> Collected {
    let mut collected = Collected::default();
    let mut current: Option<RawHost> = None;
    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index.saturating_add(1);
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = split_directive(line);
        let value = unquote(rest);
        let keyword_lower = keyword.to_ascii_lowercase();
        if keyword_lower == "host" {
            if collected.raw.len() >= csv::MAX_RECORDS {
                break;
            }
            collected.raw.extend(current.take());
            collected.records = collected.records.saturating_add(1);
            current = Some(RawHost {
                record: line_no,
                label: value.to_owned(),
                ..RawHost::default()
            });
            continue;
        }
        let Some(host) = current.as_mut() else {
            collected.warn(
                NoticeCode::IgnoredDirective,
                Some(line_no),
                format!(
                    "`{}` outside a Host block was not imported",
                    display_text(keyword, 32)
                ),
            );
            continue;
        };
        let secret = looks_like_secret(value);
        match keyword_lower.as_str() {
            _ if secret => {
                collected.secrets.keys = collected.secrets.keys.saturating_add(1);
                collected.warn(
                    NoticeCode::SecretInField,
                    Some(line_no),
                    "a private key was found on this line: it was ignored".to_owned(),
                );
            }
            "hostname" => value.clone_into(&mut host.hostname),
            "user" => value.clone_into(&mut host.user),
            "port" => value.clone_into(&mut host.port),
            "identityfile" => {
                collected.identity_files.push(display_text(value, 256));
                collected.warn(
                    NoticeCode::IdentityFileIgnored,
                    Some(line_no),
                    "IdentityFile not imported: it points to the exporting tool's key folder"
                        .to_owned(),
                );
            }
            _ => collected.warn(
                NoticeCode::IgnoredDirective,
                Some(line_no),
                format!(
                    "`{}` was not imported (only HostName, User and Port are)",
                    display_text(keyword, 32)
                ),
            ),
        }
    }
    collected.raw.extend(current);
    collected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_columns_are_found_by_name() {
        let fields: Vec<Zeroizing<String>> = [
            "  hostname/ip ",
            "LABEL",
            "Port",
            "SSH_KEY",
            "Extra",
            "password",
        ]
        .iter()
        .map(|s| Zeroizing::new((*s).to_owned()))
        .collect();
        let c = Columns::from_header(&fields);
        assert_eq!(c.hostname, Some(0));
        assert_eq!(c.label, Some(1));
        assert_eq!(c.port, Some(2));
        assert_eq!(c.keys.len(), 1);
        assert_eq!(c.passwords.len(), 1);
        assert_eq!(c.unknown, ["Extra"]);
        assert!(c.template_header && c.is_host_table());
    }

    #[test]
    fn directives_split_like_ssh() {
        assert_eq!(split_directive("Host prod one"), ("Host", "prod one"));
        assert_eq!(split_directive("hostname=1.2.3.4"), ("hostname", "1.2.3.4"));
        assert_eq!(split_directive("Port = 22"), ("Port", "22"));
        assert_eq!(split_directive("Host"), ("Host", ""));
    }

    #[test]
    fn semicolon_csv_is_read() {
        let text = "Label;Hostname/IP;Port\nweb;192.0.2.1;2222\n";
        let (format, collected) = read(text, Path::new("x.csv")).expect("csv");
        assert_eq!(format, ImportFormat::TemplateCsv);
        let raw = collected.raw.first().expect("one");
        assert_eq!(
            (raw.label.as_str(), raw.hostname.as_str(), raw.port.as_str()),
            ("web", "192.0.2.1", "2222")
        );
    }

    #[test]
    fn json_fields_are_lenient() {
        let text = r#"{"hosts":[{"label":"a","address":"192.0.2.1","port":2222,"password":"FAKEPASS123","identityFile":null,"group":{"x":1},"extra":[1]}]}"#;
        let (format, collected) = read(text, Path::new("hosts.json")).expect("json");
        assert_eq!(format, ImportFormat::LocalExportJson);
        assert_eq!(
            collected.secrets,
            super::super::IgnoredSecrets {
                passwords: 1,
                ..Default::default()
            }
        );
        let raw = collected.raw.first().expect("one");
        assert_eq!(raw.port, "2222");
        assert_eq!(raw.group, "");
        assert!(!format!("{collected:?}").contains("FAKEPASS123"));
    }

    #[test]
    fn json_errors_never_quote_values() {
        let err = read(
            r#"{"hosts": [ {"password": "FAKEPASS123" "#,
            Path::new("h.json"),
        )
        .expect_err("truncated");
        assert_eq!(err.summary().code, "import_json");
        assert!(!err.to_string().contains("FAKEPASS123"));
        let err = read(r#"{"hosts": "FAKEPASS123"}"#, Path::new("h.json")).expect_err("type");
        assert_eq!(err.summary().code, "import_unknown_format");
        assert!(!format!("{err:?}").contains("FAKEPASS123"));
    }
}
