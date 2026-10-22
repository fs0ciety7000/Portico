//! Fuzzy host search (powered by `nucleo-matcher`, the Helix matcher).
//!
//! Query syntax:
//! - free text → fuzzy match on alias, hostname, user, tags, group;
//! - `#tag` → host must carry that tag (exact, case-insensitive);
//! - `@group` → host's group must start with `group` (case-insensitive, so
//!   `@prod` finds "Prod EU"; a group with spaces is reached by its first
//!   word); several `@` words must all match;
//! - `*` alone → favorites only.
//!
//! Filters combine (AND) with each other and with the free text.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::hosts::HostEntry;

/// One search result: index into the input slice + score (higher is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub score: u32,
}

/// Reusable searcher (keeps the matcher's scratch buffers allocated).
pub struct HostSearch {
    matcher: Matcher,
}

impl std::fmt::Debug for HostSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSearch").finish_non_exhaustive()
    }
}

impl Default for HostSearch {
    fn default() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
    }
}

impl HostSearch {
    /// Filters and ranks `hosts`. Empty query keeps the input order, with
    /// favorites first.
    pub fn search(&mut self, hosts: &[HostEntry], query: &str) -> Vec<Hit> {
        let mut tags: Vec<String> = Vec::new();
        let mut groups: Vec<String> = Vec::new();
        let mut favorites_only = false;
        let mut text: Vec<&str> = Vec::new();
        for word in query.split_whitespace() {
            if let Some(tag) = word.strip_prefix('#').filter(|t| !t.is_empty()) {
                tags.push(tag.to_lowercase());
            } else if let Some(group) = word.strip_prefix('@').filter(|g| !g.is_empty()) {
                groups.push(group.to_lowercase());
            } else if word == "*" {
                favorites_only = true;
            } else {
                text.push(word);
            }
        }
        let pattern = Pattern::parse(&text.join(" "), CaseMatching::Smart, Normalization::Smart);

        let mut buf = Vec::new();
        let mut hits: Vec<Hit> = hosts
            .iter()
            .enumerate()
            .filter(|(_, h)| !favorites_only || h.meta.favorite)
            .filter(|(_, h)| {
                tags.iter()
                    .all(|t| h.meta.tags.iter().any(|ht| ht.to_lowercase() == *t))
            })
            .filter(|(_, h)| {
                groups.is_empty()
                    || h.meta.group.as_ref().is_some_and(|g| {
                        let g = g.to_lowercase();
                        groups.iter().all(|wanted| g.starts_with(wanted.as_str()))
                    })
            })
            .filter_map(|(index, h)| {
                if text.is_empty() {
                    return Some(Hit {
                        index,
                        score: u32::from(h.meta.favorite),
                    });
                }
                let hay = h.haystack();
                pattern
                    .score(Utf32Str::new(&hay, &mut buf), &mut self.matcher)
                    .map(|score| Hit { index, score })
            })
            .collect();
        // Stable sort keeps declaration order on equal scores.
        hits.sort_by_key(|h| std::cmp::Reverse(h.score));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::HostMeta;
    use crate::ssh_config::{ResolveContext, SshConfig};

    fn hosts() -> Vec<HostEntry> {
        let config = SshConfig::parse_str(
            "Host api\nHost db\nHost web\n",
            std::path::Path::new("cfg"),
            std::path::Path::new("/x"),
        )
        .expect("parse");
        let ctx = ResolveContext {
            local_user: "alice".into(),
            home: "/home/alice".into(),
            user_override: None,
        };
        let mut meta = crate::hosts::HostMetaFile::default();
        let mut set = |alias: &str, group: Option<&str>, tags: &[&str], favorite: bool| {
            meta.hosts.insert(
                alias.into(),
                HostMeta {
                    group: group.map(str::to_owned),
                    tags: tags.iter().map(|t| (*t).to_owned()).collect(),
                    favorite,
                    ..HostMeta::default()
                },
            );
        };
        set("api", Some("Prod EU"), &["web"], true);
        set("db", Some("Data"), &["prod"], false);
        set("web", Some("production"), &[], false);
        crate::hosts::inventory(&config, &meta, &ctx)
    }

    fn found(query: &str) -> Vec<String> {
        let hosts = hosts();
        let mut hits: Vec<String> = HostSearch::default()
            .search(&hosts, query)
            .into_iter()
            .filter_map(|hit| hosts.get(hit.index).map(|h| h.alias.clone()))
            .collect();
        hits.sort();
        hits
    }

    #[test]
    fn group_tag_and_favorite_filters() {
        assert_eq!(found("@prod"), ["api", "web"]);
        assert_eq!(found("@PROD @prod"), ["api", "web"]);
        assert_eq!(found("@data"), ["db"]);
        assert_eq!(found("@nope"), Vec::<String>::new());
        assert_eq!(found("@prod #web"), ["api"]);
        assert_eq!(found("@prod *"), ["api"]);
        assert_eq!(found("@prod api"), ["api"]);
        // A lone `@` is free text, not an empty filter.
        assert_eq!(found("#prod"), ["db"]);
        assert_eq!(found(""), ["api", "db", "web"]);
    }
}
