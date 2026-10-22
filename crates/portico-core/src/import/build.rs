//! Export file → [`ImportPlan`]: reading the file (bounded, `Zeroizing`),
//! detecting the format from the content, then turning records into
//! planned hosts (aliases, collisions, groups, tags) and listing the keys.
//! Nothing is written.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use zeroize::Zeroizing;

use super::private_keys::{self, KeySecret};
use super::{
    BoundedNotices, IgnoredSecrets, ImportError, ImportFormat, ImportNotice, ImportOptions,
    ImportPlan, ImportSource, KeyId, KeyMaterial, MAX_ALIAS_CHARS, MAX_IMPORT_FILE_BYTES,
    NoticeCode, Occupancy, PlannedHost, PlannedKey, clean_hostname, clean_user, display_text,
    include_line, looks_like_other_host, slug,
};
use crate::engine::EngineConfig;
use crate::error::{Error, Result};
use crate::hosts::{MAX_GROUP_CHARS, MAX_TAG_CHARS, MAX_TAGS};
use crate::store;

/// Most tags read from one cell (more are ignored without a notice: the
/// alias keeps at most [`MAX_TAGS`] anyway).
pub(crate) const MAX_TAGS_PER_CELL: usize = MAX_TAGS * 4;

/// One record as read, before any check.
#[derive(Debug, Default)]
pub(crate) struct RawHost {
    pub(crate) record: usize,
    pub(crate) label: String,
    /// An alias chosen by the exporting tool (`termius-local-export`).
    pub(crate) alias: Option<String>,
    pub(crate) hostname: String,
    pub(crate) port: String,
    pub(crate) user: String,
    pub(crate) protocol: String,
    pub(crate) group: String,
    pub(crate) tags: Vec<String>,
    /// Pinned in the export (Termix).
    pub(crate) favorite: bool,
    /// The private key of the record (Termix), parsed into
    /// [`Collected::keys`].
    pub(crate) key: Option<KeyId>,
    /// The record's own id in the export, when it has one (Termix `id`).
    pub(crate) import_id: Option<String>,
}

/// Everything collected while reading, besides the hosts.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub(crate) raw: Vec<RawHost>,
    pub(crate) records: usize,
    pub(crate) secrets: IgnoredSecrets,
    pub(crate) identity_files: Vec<String>,
    pub(crate) warnings: BoundedNotices,
    /// Records refused while reading (before any value is looked at).
    pub(crate) skipped: Vec<ImportNotice>,
    /// Private keys found, by id (Termix).
    pub(crate) keys: BTreeMap<KeyId, KeySecret>,
}

impl Collected {
    pub(crate) fn warn(&mut self, code: NoticeCode, record: Option<usize>, message: String) {
        self.notice(code, record, None, message);
    }

    pub(crate) fn notice(
        &mut self,
        code: NoticeCode,
        record: Option<usize>,
        label: Option<String>,
        message: String,
    ) {
        self.warnings.push(ImportNotice {
            code,
            record,
            label,
            message,
        });
    }
}

/// Reads `source` and builds the plan (blocking; nothing is written).
/// `only`: refuse any other source (`import_unknown_format`).
pub(crate) fn plan(
    config: &EngineConfig,
    source: &Path,
    options: &ImportOptions,
    only: Option<ImportSource>,
) -> Result<ImportPlan> {
    let paths = &config.paths;
    let bytes = match store::read_capped_bytes(source, MAX_IMPORT_FILE_BYTES) {
        Ok(Some(bytes)) => Zeroizing::new(bytes),
        Ok(None) => {
            return Err(ImportError::NotFound {
                path: source.to_path_buf(),
            }
            .into());
        }
        Err(Error::FileTooLarge { max, .. }) => {
            return Err(ImportError::TooLarge {
                path: source.to_path_buf(),
                max,
            }
            .into());
        }
        Err(e) => return Err(e),
    };
    let text = decode(&bytes).ok_or_else(|| ImportError::Encoding {
        path: source.to_path_buf(),
    })?;
    drop(bytes);
    let (format, mut collected) = read(&text, source)?;
    drop(text);
    let kind = format.source();
    if only.is_some_and(|only| only != kind) {
        return Err(ImportError::UnknownFormat {
            path: source.to_path_buf(),
        }
        .into());
    }

    let target = crate::managed_hosts::ImportTarget::read_only(config)?;
    let occupancy = Occupancy::compute(&target, paths, kind)?;
    drop(target);
    let mut builder = Builder {
        source: kind,
        prefix: options.prefix,
        occupancy: &occupancy,
        taken: HashSet::new(),
        next_suffix: HashMap::new(),
        hosts: Vec::new(),
        skipped: collected.skipped,
        warnings: collected.warnings,
    };
    for raw in &collected.raw {
        builder.add(raw);
    }
    let hosts = builder.hosts;

    // The keys used by a planned host, in host order; the others are
    // ignored (their hosts were skipped).
    let local = if collected.keys.is_empty() {
        Vec::new()
    } else {
        private_keys::local_keys(paths)
    };
    let mut keys: Vec<PlannedKey> = Vec::new();
    let mut material = BTreeMap::new();
    for host in &hosts {
        let Some(id) = &host.key else { continue };
        if let Some(planned) = keys.iter_mut().find(|k| &k.id == id) {
            planned.aliases.push(host.alias.clone());
            continue;
        }
        let Some(secret) = collected.keys.remove(id) else {
            continue;
        };
        keys.push(PlannedKey {
            id: id.clone(),
            algorithm: secret.public.algorithm().to_string(),
            fingerprint: id.as_str().to_owned(),
            format: secret.format,
            encrypted: secret.encrypted,
            aliases: vec![host.alias.clone()],
            local: private_keys::local_copy(paths, &local, id),
        });
        material.insert(id.clone(), secret);
    }
    let mut secrets = collected.secrets;
    secrets.keys = secrets.keys.saturating_add(collected.keys.len());
    secrets.keys_in_export = keys.len().saturating_add(collected.keys.len());
    drop(collected.keys);

    let managed = paths.managed_hosts();
    let updates = hosts.iter().filter(|h| h.update).count();
    Ok(ImportPlan {
        source: source.to_path_buf(),
        format,
        options: options.for_plan(),
        records: collected.records,
        hosts,
        skipped: builder.skipped,
        warnings: builder.warnings.into_vec(),
        secrets,
        identity_files: collected.identity_files,
        include_line: include_line(paths, &managed),
        replaces: (updates > 0).then_some(updates),
        target: managed,
        keys,
        key_material: KeyMaterial::new(material),
    })
}

/// UTF-8 (BOM optional) or UTF-16 with a BOM; `None` otherwise.
pub(crate) fn decode(bytes: &[u8]) -> Option<Zeroizing<String>> {
    let utf16 = |rest: &[u8], little: bool| -> Option<Zeroizing<String>> {
        if !rest.len().is_multiple_of(2) {
            return None;
        }
        let units = rest.as_chunks::<2>().0.iter().map(|&pair| {
            if little {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        });
        // One UTF-16 unit is at most 3 UTF-8 bytes (a surrogate pair, two
        // units, is 4): the string never reallocates, so no copy of the
        // text is left unwiped behind.
        let mut out = Zeroizing::new(String::with_capacity(rest.len() / 2 * 3));
        for unit in char::decode_utf16(units) {
            out.push(unit.ok()?);
        }
        Some(out)
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => std::str::from_utf8(rest)
            .ok()
            .map(|s| Zeroizing::new(s.to_owned())),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        _ => std::str::from_utf8(bytes)
            .ok()
            .map(|s| Zeroizing::new(s.to_owned())),
    }
}

/// Detects the format from the content and reads every record.
fn read(text: &str, source: &Path) -> Result<(ImportFormat, Collected)> {
    if text.trim_start().starts_with('{') && super::termix::is_termix(text) {
        return Ok((ImportFormat::TermixJson, super::termix::read(text, source)?));
    }
    super::termius::read(text, source)
}

// ── Records → hosts ─────────────────────────────────────────────────────

struct Builder<'a> {
    source: ImportSource,
    prefix: bool,
    occupancy: &'a Occupancy,
    /// Lowercased aliases already given in this import.
    taken: HashSet<String>,
    /// The next `-n` suffix to try for a base alias (renaming stays linear
    /// with thousands of identical labels).
    next_suffix: HashMap<String, usize>,
    hosts: Vec<PlannedHost>,
    skipped: Vec<ImportNotice>,
    warnings: BoundedNotices,
}

impl Builder<'_> {
    #[allow(clippy::too_many_lines)] // one linear pass over a record
    fn add(&mut self, raw: &RawHost) {
        let label = display_text(&raw.label, 200);
        let notice = |code, message: String| ImportNotice {
            code,
            record: Some(raw.record),
            label: Some(label.clone()).filter(|l| !l.is_empty()),
            message,
        };
        let Some((hostname, user, port)) = self.values(raw, &notice) else {
            return;
        };
        let label = if label.is_empty() {
            hostname.clone()
        } else {
            label
        };
        let mut base = raw
            .alias
            .as_deref()
            .map(|a| slug(a, MAX_ALIAS_CHARS))
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| slug(&label, MAX_ALIAS_CHARS));
        if base.is_empty() {
            base = slug(&hostname, MAX_ALIAS_CHARS);
        }
        if base.is_empty() {
            "host".clone_into(&mut base);
        }
        let notice = |code, message: String| ImportNotice {
            code,
            record: Some(raw.record),
            label: Some(label.clone()),
            message,
        };
        let prefix = self.source.prefix();
        if self.prefix {
            base = slug(&format!("{prefix}{base}"), MAX_ALIAS_CHARS);
        } else if looks_like_other_host(&base, &hostname, prefix) {
            let prefixed = slug(&format!("{prefix}{base}"), MAX_ALIAS_CHARS);
            self.warnings.push(notice(
                NoticeCode::AliasLooksLikeHost,
                format!("`{base}` looks like another host's name: imported as `{prefixed}`"),
            ));
            base = prefixed;
        }
        let blocked = &self.occupancy.blocked;
        let alias = if self.taken.contains(&base) {
            let mut n = self.next_suffix.get(&base).copied().unwrap_or(2);
            let renamed = loop {
                let candidate = with_suffix(&base, n);
                n = n.saturating_add(1);
                if !self.taken.contains(&candidate) && !blocked.contains(&candidate) {
                    break candidate;
                }
            };
            self.next_suffix.insert(base.clone(), n);
            self.warnings.push(notice(
                NoticeCode::AliasRenamed,
                format!("`{base}` is already used in this import: imported as `{renamed}`"),
            ));
            renamed
        } else if self.occupancy.edited.contains(&base) {
            self.skipped.push(notice(
                NoticeCode::EditedSinceImport,
                format!(
                    "`{base}` was imported earlier, then edited: left as it is (import with the {prefix} prefix to keep both)"
                ),
            ));
            return;
        } else if blocked.contains(&base) {
            self.skipped.push(notice(
                NoticeCode::AliasCollision,
                format!(
                    "`{base}` is already defined in your ssh config or your Portico hosts (import with the {prefix} prefix to keep both)"
                ),
            ));
            return;
        } else {
            base
        };
        self.taken.insert(alias.clone());

        let group = self.group(&raw.group, &notice);
        let tags = self.tags(&raw.tags, &notice);
        let notes = (alias != label).then(|| format!("{}: {label}", self.source.label()));
        let update = self.occupancy.updatable.contains(&alias);
        let source_id = match &raw.import_id {
            Some(id) => format!("id:{}", display_text(id, 128)),
            None => format!("label:{}", slug(&label, MAX_ALIAS_CHARS)),
        };
        self.hosts.push(PlannedHost {
            source_id,
            alias,
            label,
            hostname,
            user,
            port,
            group,
            tags,
            notes,
            record: raw.record,
            favorite: raw.favorite,
            key: raw.key.clone(),
            update,
        });
    }

    /// Protocol, `HostName`, `User` and `Port`; `None` when the record is
    /// skipped (the reason is recorded).
    fn values(
        &mut self,
        raw: &RawHost,
        notice: &dyn Fn(NoticeCode, String) -> ImportNotice,
    ) -> Option<(String, Option<String>, Option<u16>)> {
        let protocol = raw.protocol.trim().to_ascii_lowercase();
        if !protocol.is_empty() && protocol != "ssh" {
            self.skipped.push(notice(
                NoticeCode::UnsupportedProtocol,
                "the protocol is not SSH".to_owned(),
            ));
            return None;
        }
        if raw.hostname.trim().is_empty() {
            self.skipped.push(notice(
                NoticeCode::MissingHostname,
                "no host name or address".to_owned(),
            ));
            return None;
        }
        let Some(hostname) = clean_hostname(&raw.hostname) else {
            self.skipped.push(notice(
                NoticeCode::InvalidHostname,
                "the host name contains a space, line break, `#`, quote or another character not allowed in a host name".to_owned(),
            ));
            return None;
        };
        let user = if raw.user.trim().is_empty() {
            None
        } else if let Some(user) = clean_user(&raw.user) {
            Some(user)
        } else {
            self.skipped.push(notice(
                NoticeCode::InvalidUser,
                "the user name contains a space, line break, `#`, quote or another character not allowed".to_owned(),
            ));
            return None;
        };
        let port = match raw.port.trim() {
            "" => None,
            text => match text.parse::<u16>() {
                Ok(22) => None,
                Ok(port) if port > 0 => Some(port),
                _ => {
                    self.warnings.push(notice(
                        NoticeCode::InvalidPort,
                        "the port is not between 1 and 65535: left out (22 applies)".to_owned(),
                    ));
                    None
                }
            },
        };
        Some((hostname, user, port))
    }

    /// The group path (`Production/EU`): segments trimmed, control and
    /// invisible characters removed, at most 64 characters.
    fn group(
        &mut self,
        raw: &str,
        notice: &dyn Fn(NoticeCode, String) -> ImportNotice,
    ) -> Option<String> {
        let segments: Vec<String> = raw
            .split('/')
            .map(|s| display_text(s, MAX_GROUP_CHARS))
            .filter(|s| !s.is_empty())
            .collect();
        let joined = segments.join("/");
        if joined.is_empty() {
            return None;
        }
        let cleaned = raw
            .chars()
            .any(|c| c.is_control() || crate::snippets::is_hidden_format(c));
        let long = joined.chars().count() > MAX_GROUP_CHARS;
        let group: String = joined.chars().take(MAX_GROUP_CHARS).collect();
        let group = group.trim().trim_end_matches('/').trim().to_owned();
        if long || cleaned {
            self.warnings.push(notice(
                NoticeCode::GroupTruncated,
                format!("group imported as `{group}` (at most 64 visible characters)"),
            ));
        }
        crate::hosts::normalize_group(Some(&group)).ok().flatten()
    }

    /// Tags kept as typed when valid, otherwise slugged (with a warning);
    /// duplicates removed ignoring case, at most 16.
    fn tags(
        &mut self,
        raw: &[String],
        notice: &dyn Fn(NoticeCode, String) -> ImportNotice,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for item in raw {
            let trimmed = item.trim();
            let tag = trimmed.strip_prefix('#').unwrap_or(trimmed).trim();
            if tag.is_empty() {
                continue;
            }
            let shown = display_text(tag, 48);
            let valid = tag.len() <= MAX_TAG_CHARS
                && tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
            let tag = if valid {
                tag.to_owned()
            } else {
                let slugged = slug(tag, MAX_TAG_CHARS);
                if slugged.is_empty() {
                    self.warnings.push(notice(
                        NoticeCode::TagDropped,
                        format!("tag `{shown}` has no letter or digit Portico can keep: dropped"),
                    ));
                    continue;
                }
                self.warnings.push(notice(
                    NoticeCode::TagNormalized,
                    format!("tag `{shown}` imported as `{slugged}`"),
                ));
                slugged
            };
            if out.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                continue;
            }
            if out.len() >= MAX_TAGS {
                self.warnings.push(notice(
                    NoticeCode::TagDropped,
                    format!("more than {MAX_TAGS} tags: `{tag}` dropped"),
                ));
                continue;
            }
            out.push(tag);
        }
        crate::hosts::normalize_tags(&out).unwrap_or_default()
    }
}

/// `base-n`, `base` shortened so the result fits [`MAX_ALIAS_CHARS`].
pub(crate) fn with_suffix(base: &str, n: usize) -> String {
    let suffix = format!("-{n}");
    let keep = MAX_ALIAS_CHARS.saturating_sub(suffix.len());
    let stem: String = base.chars().take(keep).collect();
    format!("{}{suffix}", stem.trim_end_matches(['-', '.']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bom_and_utf16() {
        assert_eq!(decode(b"\xEF\xBB\xBFabc").expect("utf8").as_str(), "abc");
        assert_eq!(decode("é".as_bytes()).expect("utf8").as_str(), "é");
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("Hé".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode(&le).expect("utf16le").as_str(), "Hé");
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("Hé".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode(&be).expect("utf16be").as_str(), "Hé");
        assert!(decode(b"\xFF\xFEa").is_none(), "odd UTF-16 length");
        assert!(decode(b"\xC3\x28").is_none(), "invalid UTF-8");
    }

    #[test]
    fn suffixes_fit_the_alias_limit() {
        assert_eq!(with_suffix("web", 2), "web-2");
        let long = "a".repeat(MAX_ALIAS_CHARS);
        let renamed = with_suffix(&long, 12);
        assert_eq!(renamed.len(), MAX_ALIAS_CHARS);
        assert!(renamed.ends_with("-12"));
    }

    #[test]
    fn termius_json_is_not_taken_for_termix() {
        let (format, _) = read(
            r#"{"hosts":[{"label":"a","address":"192.0.2.1"}]}"#,
            Path::new("hosts.json"),
        )
        .expect("json");
        assert_eq!(format, ImportFormat::LocalExportJson);
        let (format, _) = read(
            r#"{"hosts":[{"name":"a","ip":"192.0.2.1"}]}"#,
            Path::new("termix.json"),
        )
        .expect("json");
        assert_eq!(format, ImportFormat::TermixJson);
    }
}
