//! [`ImportPlan`] → Portico hosts (`hosts.conf`), metadata and provenance
//! (`hosts.toml`), and the selected private keys (`~/.ssh/portico/`).
//!
//! Order, all under the `hosts.conf` lock (then the `hosts.toml` one, the
//! order every editing command takes them in):
//!
//! 1. Who owns which alias is computed again ([`Occupancy`]) and every
//!    planned value is checked again (a plan is data; an alias defined
//!    since, or a host edited since, is skipped).
//! 2. The selected keys used by an accepted host are reused (same
//!    fingerprint on disk) or written.
//! 3. The new `hosts.toml` is built and its size checked, then `hosts.conf`
//!    is written (same checks as the editing commands), then `hosts.toml`.
//!    Should anything fail before `hosts.conf` is written, the key files
//!    written in step 2 are removed and nothing else changed. Should
//!    `hosts.toml` fail after it, the hosts exist without provenance: a
//!    later import treats them as created by hand (never overwritten).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::private_keys;
use super::{
    BoundedNotices, ImportError, ImportNotice, ImportOptions, ImportPlan, ImportReport, KeyId,
    KeyOutcome, MAX_ALIAS_CHARS, NoticeCode, Occupancy, PlannedHost, clean_hostname, clean_user,
    include_active, include_line, is_valid_alias, looks_like_other_host, slug,
};
use crate::engine::EngineConfig;
use crate::error::Result;
use crate::hosts::{HostMeta, ImportProvenance, ImportedMeta, prepare_imported_meta};
use crate::managed_hosts::{ImportTarget, ManagedHost, check_identity_file, values_hash};

/// What [`apply`] did, for the engine's events.
#[derive(Debug)]
pub(crate) struct Applied {
    pub(crate) report: ImportReport,
    /// `hosts.conf` was rewritten.
    pub(crate) hosts_changed: bool,
    /// Aliases whose metadata changed, with what the next load reads.
    pub(crate) meta: Vec<(String, HostMeta)>,
}

/// Writes `plan` with `options` (blocking). See the module doc.
pub(crate) fn apply(
    config: &EngineConfig,
    plan: &ImportPlan,
    options: &ImportOptions,
) -> Result<Applied> {
    let paths = &config.paths;
    let source = plan.format.source();
    let target = ImportTarget::open(config)?;
    let occupancy = Occupancy::compute(&target, paths, source)?;
    let mut skipped = plan.skipped.clone();
    let mut warnings = BoundedNotices::from_vec(plan.warnings.clone());
    let accepted = checked_hosts(
        plan,
        options.prefix || plan.options.prefix,
        &occupancy,
        &mut skipped,
        &mut warnings,
    );

    let (keys, key_paths, written) = import_keys(paths, plan, options, &accepted, &mut warnings)?;

    let Merged {
        hosts,
        imported,
        updated,
        entries,
    } = merged_hosts(&paths.home, source, target.hosts(), &accepted, &key_paths);
    let hosts_changed = hosts.as_slice() != target.hosts();
    let meta_path = paths.hosts_meta();
    let prepared = match prepare_imported_meta(&meta_path, &entries) {
        Ok(prepared) => prepared,
        Err(e) => {
            private_keys::remove_written(&written);
            return Err(e);
        }
    };
    if hosts_changed && let Err(e) = target.write(&hosts) {
        private_keys::remove_written(&written);
        return Err(e);
    }
    let merged = prepared.commit().map_err(|e| ImportError::MetaNotWritten {
        target: target.file().to_path_buf(),
        meta: meta_path.clone(),
        reason: e.summary().message,
    })?;
    drop(target);
    for alias in &merged.tags_truncated {
        warnings.push(ImportNotice {
            code: NoticeCode::TagsTruncated,
            record: None,
            label: Some(alias.clone()),
            message: "this host already has 16 tags: some imported tags were left out".to_owned(),
        });
    }

    let managed = paths.managed_hosts();
    let mut secrets = plan.secrets;
    secrets.keys = secrets.keys.saturating_add(keys.skipped);
    let report = ImportReport {
        format: plan.format,
        unchanged: !hosts_changed,
        imported,
        updated,
        skipped,
        warnings: warnings.into_vec(),
        secrets,
        identity_files: plan.identity_files.clone(),
        meta_updated: merged.changed.len(),
        include_line: include_line(paths, &managed),
        include_active: include_active(paths, &managed),
        target: managed,
        keys,
    };
    Ok(Applied {
        report,
        hosts_changed,
        meta: merged.changed,
    })
}

/// The hosts of `hosts.conf` once `accepted` is applied, and their
/// metadata entries.
struct Merged {
    hosts: Vec<ManagedHost>,
    imported: Vec<String>,
    updated: Vec<String>,
    entries: Vec<ImportedMeta>,
}

/// Creates or updates (same alias, ignoring case) each accepted host.
fn merged_hosts(
    home: &std::path::Path,
    source: super::ImportSource,
    current: &[ManagedHost],
    accepted: &[PlannedHost],
    key_paths: &HashMap<KeyId, PathBuf>,
) -> Merged {
    let mut hosts: Vec<ManagedHost> = current.to_vec();
    let mut imported = Vec::new();
    let mut updated = Vec::new();
    let mut entries: Vec<ImportedMeta> = Vec::new();
    for planned in accepted {
        let index = hosts
            .iter()
            .position(|h| h.alias.eq_ignore_ascii_case(&planned.alias));
        let selected = planned.key.as_ref().and_then(|id| key_paths.get(id));
        // A re-import without the key keeps the key the host already has.
        let identity_file = match (selected, index.and_then(|i| hosts.get(i))) {
            (Some(path), _) => Some(path.clone()),
            (None, Some(existing)) => existing
                .identity_file
                .clone()
                .filter(|p| check_identity_file(home, p).is_ok()),
            (None, None) => None,
        };
        let host = ManagedHost {
            alias: planned.alias.clone(),
            hostname: planned.hostname.clone(),
            user: planned.user.clone(),
            port: planned.port,
            identity_file,
            proxy_jump: None,
        };
        entries.push(ImportedMeta {
            alias: host.alias.clone(),
            tags: planned.tags.clone(),
            group: planned.group.clone(),
            notes: planned.notes.clone(),
            favorite: planned.favorite && index.is_none(),
            provenance: Some(ImportProvenance {
                source: source.as_str().to_owned(),
                id: planned.source_id.clone(),
                values: values_hash(home, &host),
            }),
        });
        imported.push(host.alias.clone());
        match index.and_then(|i| hosts.get_mut(i)) {
            Some(slot) => {
                updated.push(host.alias.clone());
                *slot = host;
            }
            None => hosts.push(host),
        }
    }
    Merged {
        hosts,
        imported,
        updated,
        entries,
    }
}

/// Reuses or writes the selected keys used by an accepted host. Returns
/// what happened, the `IdentityFile` of each key, and the files written
/// (removed again by the caller should the import fail later). On an error,
/// the files written so far are removed.
fn import_keys(
    paths: &crate::store::Paths,
    plan: &ImportPlan,
    options: &ImportOptions,
    accepted: &[PlannedHost],
    warnings: &mut BoundedNotices,
) -> Result<(KeyOutcome, HashMap<KeyId, PathBuf>, Vec<PathBuf>)> {
    let any_selected = plan.keys.iter().any(|k| options.keys.contains(&k.id));
    let local = if any_selected {
        private_keys::local_keys(paths)
    } else {
        Vec::new()
    };
    let mut keys = KeyOutcome::default();
    let mut key_paths: HashMap<KeyId, PathBuf> = HashMap::new();
    let mut written: Vec<PathBuf> = Vec::new();
    for planned in &plan.keys {
        let id = &planned.id;
        if !options.keys.contains(id) {
            keys.skipped = keys.skipped.saturating_add(1);
            continue;
        }
        let key_notice = |message: &str| ImportNotice {
            code: NoticeCode::KeySkipped,
            record: None,
            label: Some(id.as_str().to_owned()),
            message: message.to_owned(),
        };
        let Some(first) = accepted.iter().find(|h| h.key.as_ref() == Some(id)) else {
            keys.skipped = keys.skipped.saturating_add(1);
            warnings.push(key_notice(
                "this key was selected, but none of its hosts was imported",
            ));
            continue;
        };
        let Some(secret) = plan.key_material.get(id) else {
            keys.skipped = keys.skipped.saturating_add(1);
            warnings.push(key_notice(
                "this plan no longer holds the key: preview the export again",
            ));
            continue;
        };
        if let Some(path) = private_keys::local_copy(paths, &local, id) {
            key_paths.insert(id.clone(), path.clone());
            keys.reused.push(path);
            continue;
        }
        match private_keys::write(
            paths,
            &first.alias,
            id,
            secret,
            options.key_passphrase.as_ref(),
        ) {
            Ok(path) => {
                written.push(path.clone());
                key_paths.insert(id.clone(), path.clone());
                keys.imported.push(path);
            }
            Err(e) => {
                private_keys::remove_written(&written);
                return Err(e);
            }
        }
    }
    Ok((keys, key_paths, written))
}

/// The hosts of `plan` that can be written, checked again: a value
/// Portico does not write or a duplicate is skipped, an alias now taken
/// (or a host edited since) is skipped; an alias that looks like another
/// host, or every alias with `prefix_all`, gets the source's prefix.
fn checked_hosts(
    plan: &ImportPlan,
    prefix_all: bool,
    occupancy: &Occupancy,
    skipped: &mut Vec<ImportNotice>,
    warnings: &mut BoundedNotices,
) -> Vec<PlannedHost> {
    let source = plan.format.source();
    let prefix = source.prefix();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hosts: Vec<PlannedHost> = Vec::new();
    for planned in &plan.hosts {
        let notice = |code, message: String| ImportNotice {
            code,
            record: Some(planned.record),
            label: Some(planned.label.clone()),
            message,
        };
        let mut host = planned.clone();
        let wants_prefix = if prefix_all {
            !host.alias.starts_with(prefix)
        } else if looks_like_other_host(&host.alias, &host.hostname, prefix) {
            warnings.push(notice(
                NoticeCode::AliasLooksLikeHost,
                format!(
                    "this alias looks like another host's name: imported with the {prefix} prefix"
                ),
            ));
            true
        } else {
            false
        };
        if wants_prefix {
            host.alias = slug(&format!("{prefix}{}", host.alias), MAX_ALIAS_CHARS);
            if host.notes.is_none() {
                host.notes = Some(format!("{}: {}", source.label(), host.label));
            }
        }
        let lower = host.alias.to_lowercase();
        let valid = is_valid_alias(&host.alias)
            && seen.insert(lower.clone())
            && clean_hostname(&host.hostname).as_deref() == Some(host.hostname.as_str())
            && host
                .user
                .as_ref()
                .is_none_or(|u| clean_user(u).as_deref() == Some(u.as_str()))
            && host.port != Some(0);
        if !valid {
            skipped.push(notice(
                NoticeCode::InvalidHostname,
                "this host has a value Portico does not write".to_owned(),
            ));
        } else if occupancy.edited.contains(&lower) {
            skipped.push(notice(
                NoticeCode::EditedSinceImport,
                format!("this host was imported earlier, then edited: left as it is (import with the {prefix} prefix to keep both)"),
            ));
        } else if occupancy.blocked.contains(&lower) {
            skipped.push(notice(
                NoticeCode::AliasCollision,
                format!("this alias is now defined in your ssh config or your Portico hosts (import with the {prefix} prefix to keep both)"),
            ));
        } else {
            host.update = occupancy.updatable.contains(&lower);
            hosts.push(host);
        }
    }
    hosts
}
