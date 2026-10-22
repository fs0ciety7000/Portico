//! Rows of the host list (pure).
//!
//! Without a search, the list is organised in sections: `Recent` (the
//! [`RECENT_SHOWN`] most recent hosts that still exist in the config),
//! then one section per group (alphabetical, ignoring case), then `Other`
//! (hosts without a group). With neither recent hosts nor groups, the list
//! stays flat. With a search, it is flat and ranked by the matcher. A host
//! may appear twice (in Recent and in its group): both rows select it.

use portico_core::hosts::HostEntry;
use portico_core::recent::RecentHost;
use portico_core::search::Hit;

/// Recent hosts shown on top of the list.
pub(crate) const RECENT_SHOWN: usize = 5;
/// Recent entries fetched (some may be gone from the config).
pub(crate) const RECENT_FETCHED: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Section title (not selectable).
    Header(String),
    /// `index` into the inventory; `recent` on a row of the Recent section.
    Host { index: usize, recent: bool },
}

impl Entry {
    pub(crate) fn host(&self) -> Option<usize> {
        match self {
            Self::Host { index, .. } => Some(*index),
            Self::Header(_) => None,
        }
    }
}

pub(crate) fn build(
    hosts: &[HostEntry],
    hits: &[Hit],
    query: &str,
    recent: &[RecentHost],
) -> Vec<Entry> {
    let flat = || {
        hits.iter()
            .map(|h| Entry::Host {
                index: h.index,
                recent: false,
            })
            .collect()
    };
    if !query.trim().is_empty() {
        return flat();
    }
    let recent_rows: Vec<usize> = recent
        .iter()
        .filter_map(|r| hosts.iter().position(|h| h.alias == r.alias))
        .take(RECENT_SHOWN)
        .collect();
    // Groups in alphabetical order (ignoring case; the first spelling met
    // names the section), hosts in config order within each.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut others = Vec::new();
    for hit in hits {
        let group = hosts
            .get(hit.index)
            .and_then(|h| h.meta.group.as_deref())
            .map(str::trim)
            .filter(|g| !g.is_empty());
        match group {
            Some(name) => match groups
                .iter_mut()
                .find(|(g, _)| g.eq_ignore_ascii_case(name))
            {
                Some((_, rows)) => rows.push(hit.index),
                None => groups.push((name.to_owned(), vec![hit.index])),
            },
            None => others.push(hit.index),
        }
    }
    if recent_rows.is_empty() && groups.is_empty() {
        return flat();
    }
    groups.sort_by_key(|(name, _)| name.to_lowercase());
    let mut entries = Vec::new();
    let mut section = |title: &str, rows: &[usize], recent: bool| {
        if rows.is_empty() {
            return;
        }
        entries.push(Entry::Header(title.to_owned()));
        entries.extend(rows.iter().map(|&index| Entry::Host { index, recent }));
    };
    section("Recent", &recent_rows, true);
    for (name, rows) in &groups {
        section(name, rows, false);
    }
    section("Other", &others, false);
    entries
}

/// `Prod eu` → `P R O D   E U` in at most `max` columns; when the spaced
/// version does not fit, plain capitals (`PROD EU`), cut if still too long.
pub(crate) fn section_title(title: &str, max: usize) -> String {
    let spaced = spaced_caps(title, usize::MAX);
    if spaced.chars().count() <= max {
        return spaced;
    }
    let plain: Vec<String> = title.split_whitespace().map(str::to_uppercase).collect();
    crate::ui::fmt::truncate(&plain.join(" "), max)
}

/// `PROD EU` → `P R O D   E U`: a section title in spaced capitals, cut
/// to `max` columns.
pub(crate) fn spaced_caps(title: &str, max: usize) -> String {
    let spaced: Vec<String> = title
        .split_whitespace()
        .map(|word| {
            word.to_uppercase()
                .chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    crate::ui::fmt::truncate(&spaced.join("   "), max)
}

/// A host with no config but its alias (tests only).
#[cfg(test)]
pub(crate) fn test_host(alias: &str, group: Option<&str>) -> HostEntry {
    use std::path::{Path, PathBuf};

    use portico_core::hosts::HostMeta;
    use portico_core::ssh_config::{ResolveContext, Source, SshConfig};

    let config = SshConfig::parse_str("", Path::new("cfg"), Path::new("/x")).unwrap();
    let ctx = ResolveContext {
        local_user: "alice".into(),
        home: PathBuf::from("/home/alice"),
        user_override: None,
    };
    HostEntry {
        alias: alias.into(),
        source: Source {
            file: PathBuf::from("cfg"),
            line: 1,
        },
        origin: portico_core::hosts::HostOrigin::SshConfig,
        imported_from: None,
        resolved: config.resolve(alias, &ctx),
        meta: HostMeta {
            group: group.map(str::to_owned),
            ..HostMeta::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn host(alias: &str, group: Option<&str>) -> HostEntry {
        test_host(alias, group)
    }

    fn hits(n: usize) -> Vec<Hit> {
        (0..n).map(|index| Hit { index, score: 0 }).collect()
    }

    fn recent(aliases: &[&str]) -> Vec<RecentHost> {
        aliases
            .iter()
            .map(|a| RecentHost {
                alias: (*a).into(),
                last: OffsetDateTime::UNIX_EPOCH,
                count: 1,
            })
            .collect()
    }

    fn shape(entries: &[Entry], hosts: &[HostEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Header(t) => format!("[{t}]"),
                Entry::Host { index, recent } => {
                    let alias = hosts
                        .get(*index)
                        .map(|h| h.alias.clone())
                        .unwrap_or_default();
                    if *recent { format!("{alias}*") } else { alias }
                }
            })
            .collect()
    }

    #[test]
    fn flat_without_groups_or_recent() {
        let hosts = vec![host("a", None), host("b", None)];
        let entries = build(&hosts, &hits(2), "", &[]);
        assert_eq!(shape(&entries, &hosts), ["a", "b"]);
        // Recent entries that no longer exist do not make a section.
        let entries = build(&hosts, &hits(2), "", &recent(&["gone"]));
        assert_eq!(shape(&entries, &hosts), ["a", "b"]);
    }

    #[test]
    fn sections_recent_groups_other() {
        let hosts = vec![
            host("web", Some("prod")),
            host("db", Some("Data")),
            host("lab", None),
            host("api", Some("Prod")),
        ];
        let entries = build(
            &hosts,
            &hits(4),
            "",
            &recent(&["gone", "lab", "api", "b1", "b2", "b3", "b4", "web", "db"]),
        );
        assert_eq!(
            shape(&entries, &hosts),
            [
                "[Recent]", "lab*", "api*", "web*", "db*", "[Data]", "db", "[prod]", "web", "api",
                "[Other]", "lab"
            ]
        );
        let six: Vec<HostEntry> = (0..7).map(|i| host(&format!("h{i}"), None)).collect();
        let names: Vec<String> = (0..7).map(|i| format!("h{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let entries = build(&six, &hits(7), "", &recent(&refs));
        let recent_rows = entries
            .iter()
            .filter(|e| matches!(e, Entry::Host { recent: true, .. }))
            .count();
        assert_eq!(recent_rows, RECENT_SHOWN);
        // Groups only: no Recent header.
        let entries = build(&hosts, &hits(4), "", &[]);
        assert_eq!(entries.first(), Some(&Entry::Header("Data".into())));
    }

    #[test]
    fn search_is_flat() {
        let hosts = vec![host("web", Some("prod")), host("db", None)];
        let ranked = vec![Hit { index: 1, score: 9 }, Hit { index: 0, score: 3 }];
        let entries = build(&hosts, &ranked, "d", &recent(&["web"]));
        assert_eq!(shape(&entries, &hosts), ["db", "web"]);
    }

    #[test]
    fn spaced_titles() {
        assert_eq!(spaced_caps("Prod eu", 40), "P R O D   E U");
        assert_eq!(spaced_caps("recent", 5), "R E …");
        // Spaced when it fits, plain capitals otherwise, cut last.
        assert_eq!(section_title("Prod eu", 13), "P R O D   E U");
        assert_eq!(section_title("Prod eu", 12), "PROD EU");
        assert_eq!(
            section_title("Production Europe West", 20),
            "PRODUCTION EUROPE W…"
        );
    }
}
