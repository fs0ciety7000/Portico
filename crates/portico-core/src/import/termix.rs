//! Termix exports → records (ADR "Import Termix et imports vers les hôtes
//! Portico").
//!
//! A Termix export is a JSON object with a `hosts` array; it is recognized
//! from its content: at least one entry has an `ip` field (Termius'
//! `hosts.json` uses `address`). Each entry:
//!
//! ```json
//! {"name":"Hbd","ip":"85.17.179.101","port":2222,"username":"root",
//!  "authType":"key","password":null,"key":"-----BEGIN OPENSSH PRIVATE KEY-----\n…",
//!  "keyPassword":null,"keyType":null,"folder":null,"tags":[],"pin":false,
//!  "enableTerminal":true,"enableTunnel":true,"enableFileManager":true,
//!  "defaultPath":"/","tunnelConnections":[],"socks5ProxyChain":[]}
//! ```
//!
//! Read leniently: unknown fields are ignored, `null` and wrong types are
//! "absent", a missing `port` means 22. `name` → alias (same
//! transliteration as Termius), `ip` → `HostName`, `username` → `User`,
//! `folder` → group, `tags`, `pin` → favorite. `tunnelConnections` and
//! `socks5ProxyChain` are listed as notices when not empty, never imported.
//!
//! Secrets: `password`, `key` and `keyPassword` are read into `Zeroizing`
//! strings only (`serde_json`'s own unescaping buffer aside, which Portico
//! cannot reach), never copied elsewhere. Passwords and key passphrases are
//! counted, never kept; keys go through [`super::private_keys::parse`] and
//! stay in the plan's [`super::KeyMaterial`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zeroize::Zeroizing;

use super::build::{Collected, MAX_TAGS_PER_CELL, RawHost};
use super::private_keys::{self, KeySecret};
use super::termius::text;
use super::{ImportError, KeyId, MAX_IMPORT_RECORDS, NoticeCode, display_text};
use crate::error::Result;

/// `true` when `text` (a JSON object) looks like a Termix export: a
/// `hosts` array with at least one entry holding an `ip` field. Values
/// are skipped, never copied. A text that is not valid JSON is left to the
/// reader that reports the error (`false`).
pub(crate) fn is_termix(text: &str) -> bool {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default, deserialize_with = "probe_hosts")]
        hosts: bool,
    }
    serde_json::from_str::<Probe>(text).is_ok_and(|p| p.hosts)
}

/// `true` when the value is an array with an object holding `ip`.
fn probe_hosts<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<bool, D::Error> {
    struct HostsVisitor;
    impl<'de> Visitor<'de> for HostsVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of hosts")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<bool, A::Error> {
            #[derive(Deserialize)]
            struct Entry {
                #[serde(default, deserialize_with = "present")]
                ip: bool,
            }
            let mut found = false;
            while let Some(entry) = seq.next_element::<Option<IgnoredOr<Entry>>>()? {
                if let Some(IgnoredOr::Value(Entry { ip: true })) = entry {
                    found = true;
                }
            }
            Ok(found)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<bool, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(false)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<bool, E> {
            Ok(false)
        }
    }
    deserializer.deserialize_any(HostsVisitor)
}

/// An object (`Value`), or anything else skipped.
enum IgnoredOr<T> {
    Value(T),
    Ignored,
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for IgnoredOr<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        // Buffer-free: an object is read as `T`, anything else skipped.
        struct AnyVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for AnyVisitor<T> {
            type Value = IgnoredOr<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                T::deserialize(de::value::MapAccessDeserializer::new(map)).map(IgnoredOr::Value)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(IgnoredOr::Ignored)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(IgnoredOr::Ignored)
            }
        }
        deserializer.deserialize_any(AnyVisitor(std::marker::PhantomData))
    }
}

/// `true` when the field is present (its value is skipped, never copied).
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<bool, D::Error> {
    IgnoredAny::deserialize(deserializer).map(|_| true)
}

#[derive(Deserialize)]
struct TermixExport {
    #[serde(deserialize_with = "hosts_list")]
    hosts: Vec<TermixHost>,
}

impl fmt::Debug for TermixExport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(&self.hosts).finish()
    }
}

/// The hosts array: entries that are not objects are counted as empty
/// hosts (skipped later for a missing address); more than
/// [`MAX_IMPORT_RECORDS`] fails (the rest is not read into memory).
fn hosts_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<TermixHost>, D::Error> {
    struct ListVisitor;
    impl<'de> Visitor<'de> for ListVisitor {
        type Value = Vec<TermixHost>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of hosts")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut hosts = Vec::new();
            while let Some(entry) = seq.next_element::<IgnoredOr<TermixHost>>()? {
                if hosts.len() >= MAX_IMPORT_RECORDS {
                    return Err(de::Error::custom(TOO_MANY));
                }
                hosts.push(match entry {
                    IgnoredOr::Value(host) => host,
                    IgnoredOr::Ignored => TermixHost::default(),
                });
            }
            Ok(hosts)
        }
    }
    deserializer.deserialize_seq(ListVisitor)
}

/// Marker of the "too many hosts" deserialization error.
const TOO_MANY: &str = "portico: too many hosts";

/// One host entry. Every field is optional and lenient; `Debug` never
/// shows the secret fields.
#[derive(Default, Deserialize)]
#[serde(default)]
struct TermixHost {
    #[serde(deserialize_with = "text")]
    id: Option<String>,
    #[serde(deserialize_with = "text")]
    name: Option<String>,
    #[serde(deserialize_with = "text")]
    ip: Option<String>,
    #[serde(deserialize_with = "text")]
    port: Option<String>,
    #[serde(deserialize_with = "text")]
    username: Option<String>,
    #[serde(rename = "authType", deserialize_with = "text")]
    auth_type: Option<String>,
    #[serde(deserialize_with = "secret")]
    password: Option<Zeroizing<String>>,
    #[serde(deserialize_with = "secret")]
    key: Option<Zeroizing<String>>,
    #[serde(rename = "keyPassword", deserialize_with = "secret")]
    key_password: Option<Zeroizing<String>>,
    #[serde(deserialize_with = "text")]
    folder: Option<String>,
    #[serde(deserialize_with = "tags")]
    tags: Vec<String>,
    #[serde(deserialize_with = "flag")]
    pin: bool,
    #[serde(rename = "tunnelConnections", deserialize_with = "count")]
    tunnels: usize,
    #[serde(rename = "socks5ProxyChain", deserialize_with = "count")]
    socks5: usize,
}

impl fmt::Debug for TermixHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TermixHost")
            .field("name", &self.name)
            .field("ip", &self.ip)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field(
                "key_password",
                &self.key_password.as_ref().map(|_| "<redacted>"),
            )
            .finish_non_exhaustive()
    }
}

/// A string as a wiped-on-drop secret; blank, `null` or another type is
/// `None` (an owned string handed over by the parser is wiped too).
fn secret<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Zeroizing<String>>, D::Error> {
    struct SecretVisitor;
    impl<'de> Visitor<'de> for SecretVisitor {
        type Value = Option<Zeroizing<String>>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            if v.trim().is_empty() {
                return Ok(None);
            }
            let mut out = Zeroizing::new(String::with_capacity(v.len()));
            out.push_str(v);
            Ok(Some(out))
        }
        fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
            let v = Zeroizing::new(v);
            if v.trim().is_empty() {
                return Ok(None);
            }
            Ok(Some(v))
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok(None)
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
    deserializer.deserialize_any(SecretVisitor)
}

/// `tags`: a list of strings (other items skipped), or one comma-separated
/// string; at most [`MAX_TAGS_PER_CELL`] are kept.
fn tags<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Vec<String>, D::Error> {
    struct TagsVisitor;
    impl<'de> Visitor<'de> for TagsVisitor {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of tags")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
            Ok(v.split(',')
                .take(MAX_TAGS_PER_CELL)
                .map(str::to_owned)
                .collect())
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            #[derive(Deserialize)]
            struct Tag(#[serde(deserialize_with = "text")] Option<String>);
            let mut out = Vec::new();
            while let Some(Tag(tag)) = seq.next_element::<Tag>()? {
                if let Some(tag) = tag
                    && out.len() < MAX_TAGS_PER_CELL
                {
                    out.push(tag);
                }
            }
            Ok(out)
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(Vec::new())
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    deserializer.deserialize_any(TagsVisitor)
}

/// `true` for `true`, `1` or `"true"`; anything else is `false`.
fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<bool, D::Error> {
    struct FlagVisitor;
    impl<'de> Visitor<'de> for FlagVisitor {
        type Value = bool;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a boolean")
        }
        fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<bool, E> {
            Ok(v)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<bool, E> {
            Ok(v == 1)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<bool, E> {
            Ok(v == 1)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<bool, E> {
            Ok(v.trim().eq_ignore_ascii_case("true"))
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<bool, E> {
            Ok(false)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<bool, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(false)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<bool, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(false)
        }
    }
    deserializer.deserialize_any(FlagVisitor)
}

/// The number of items of a list (an object counts as 1, anything else 0).
/// Items are skipped, never copied.
fn count<'de, D: Deserializer<'de>>(deserializer: D) -> std::result::Result<usize, D::Error> {
    struct CountVisitor;
    impl<'de> Visitor<'de> for CountVisitor {
        type Value = usize;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<usize, A::Error> {
            let mut n = 0usize;
            while seq.next_element::<IgnoredAny>()?.is_some() {
                n = n.saturating_add(1);
            }
            Ok(n)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<usize, A::Error> {
            let mut any = false;
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                any = true;
            }
            Ok(usize::from(any))
        }
        fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<usize, E> {
            Ok(0)
        }
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<usize, E> {
            Ok(0)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<usize, E> {
            Ok(0)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<usize, E> {
            Ok(0)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<usize, E> {
            Ok(0)
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<usize, E> {
            Ok(0)
        }
    }
    deserializer.deserialize_any(CountVisitor)
}

/// Reads a Termix export: hosts, counted secrets and parsed keys
/// (blocking: an encrypted PEM key runs a KDF).
#[allow(clippy::too_many_lines)] // one linear pass over the hosts
pub(crate) fn read(text: &str, source: &Path) -> Result<Collected> {
    let export: TermixExport = serde_json::from_str(text).map_err(|e| {
        // Never the serde message: it can quote a value.
        if e.to_string().starts_with(TOO_MANY) {
            return ImportError::TooManyRecords {
                path: source.to_path_buf(),
                max: MAX_IMPORT_RECORDS,
            };
        }
        match e.classify() {
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
        }
    })?;
    let mut collected = Collected::default();
    let mut keys: BTreeMap<KeyId, KeySecret> = BTreeMap::new();
    for (index, host) in export.hosts.into_iter().enumerate() {
        let record = index.saturating_add(1);
        collected.records = collected.records.saturating_add(1);
        let label = host.name.clone().unwrap_or_default();
        let shown = Some(display_text(&label, 200)).filter(|l| !l.is_empty());
        if host.password.is_some() {
            collected.secrets.passwords = collected.secrets.passwords.saturating_add(1);
        }
        if host.key_password.is_some() {
            collected.secrets.passwords = collected.secrets.passwords.saturating_add(1);
        }
        for (count, field) in [
            (host.tunnels, "tunnelConnections"),
            (host.socks5, "socks5ProxyChain"),
        ] {
            if count > 0 {
                collected.notice(
                    NoticeCode::IgnoredSetting,
                    Some(record),
                    shown.clone(),
                    format!("`{field}` ({count}) was not imported: Portico imports hosts only"),
                );
            }
        }
        let auth = host
            .auth_type
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let key = match host.key.as_deref() {
            None => None,
            Some(_) if matches!(auth.as_str(), "password" | "none") => {
                collected.secrets.keys = collected.secrets.keys.saturating_add(1);
                collected.notice(
                    NoticeCode::KeyNotUsed,
                    Some(record),
                    shown.clone(),
                    "a private key is stored, but this host does not use key authentication: not offered".to_owned(),
                );
                None
            }
            Some(raw) => {
                match private_keys::parse(raw, host.key_password.as_deref().map(String::as_str)) {
                    Ok(secret) => {
                        let id = secret.id();
                        keys.entry(id.clone()).or_insert(secret);
                        Some(id)
                    }
                    Err(reason) => {
                        collected.secrets.keys = collected.secrets.keys.saturating_add(1);
                        collected.notice(
                            NoticeCode::KeyUnreadable,
                            Some(record),
                            shown.clone(),
                            format!("private key not imported: {reason}"),
                        );
                        None
                    }
                }
            }
        };
        collected.raw.push(RawHost {
            record,
            label,
            alias: None,
            hostname: host.ip.unwrap_or_default(),
            port: host.port.unwrap_or_default(),
            user: host.username.unwrap_or_default(),
            protocol: String::new(),
            group: host.folder.unwrap_or_default(),
            tags: host.tags,
            favorite: host.pin,
            key,
            import_id: host.id.filter(|id| !id.trim().is_empty()),
        });
    }
    collected.keys = keys;
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_looks_at_fields_only() {
        assert!(is_termix(r#"{"hosts":[{"name":"a","ip":"192.0.2.1"}]}"#));
        assert!(is_termix(r#"{"hosts":[1,null,{"ip":null}],"version":2}"#));
        assert!(!is_termix(
            r#"{"hosts":[{"label":"a","address":"192.0.2.1"}]}"#
        ));
        assert!(!is_termix(r#"{"hosts":[]}"#));
        assert!(!is_termix(r#"{"hosts":"x"}"#));
        assert!(!is_termix(r#"{"hosts":[{"ip":"x"}"#), "broken JSON");
    }

    #[test]
    fn lenient_fields_and_redacted_debug() {
        let text = r#"{"hosts":[{"name":"a","ip":"192.0.2.1","port":null,"username":7,
            "password":"FAKEPASS123","keyPassword":"","tags":["x",1,null,"y"],"pin":"true",
            "folder":{"no":1},"tunnelConnections":[{"a":1},{"b":2}],"socks5ProxyChain":null,
            "extra":{"deep":[1,2]}}]}"#;
        let collected = read(text, Path::new("t.json")).expect("read");
        let raw = collected.raw.first().expect("one");
        assert_eq!(raw.port, "");
        assert_eq!(raw.user, "7");
        assert_eq!(raw.group, "");
        assert_eq!(raw.tags, ["x", "1", "y"]);
        assert!(raw.favorite);
        assert_eq!(collected.secrets.passwords, 1, "blank keyPassword ignored");
        assert!(!format!("{collected:?}").contains("FAKEPASS123"));
        let export: TermixExport = serde_json::from_str(text).expect("parse");
        assert!(!format!("{export:?}").contains("FAKEPASS123"));
    }

    #[test]
    fn json_errors_never_quote_values() {
        let err = read(
            r#"{"hosts": [ {"ip": "1.2.3.4", "password": "FAKEPASS123" "#,
            Path::new("t.json"),
        )
        .expect_err("truncated");
        assert_eq!(err.summary().code, "import_json");
        assert!(!format!("{err:?}{err}").contains("FAKEPASS123"));
        let err = read(r#"{"hosts": "FAKEPASS123"}"#, Path::new("t.json")).expect_err("type");
        assert_eq!(err.summary().code, "import_unknown_format");
        assert!(!format!("{err:?}{err}").contains("FAKEPASS123"));
    }
}
