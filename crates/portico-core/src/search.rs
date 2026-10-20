//! Fuzzy host search (powered by `nucleo-matcher`, the Helix matcher).
//!
//! Query syntax:
//! - free text → fuzzy match on alias, hostname, user, tags, group;
//! - `#tag` → host must carry that tag (exact, case-insensitive);
//! - `*` alone → favorites only.

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
        let mut favorites_only = false;
        let mut text: Vec<&str> = Vec::new();
        for word in query.split_whitespace() {
            if let Some(tag) = word.strip_prefix('#').filter(|t| !t.is_empty()) {
                tags.push(tag.to_lowercase());
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
