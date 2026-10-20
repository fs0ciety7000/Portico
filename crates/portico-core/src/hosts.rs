//! Host inventory: `~/.ssh/config` aliases enriched with Portico metadata.
//!
//! The ssh config stays the single source of truth for *how* to connect.
//! Portico only adds presentation metadata (tags, group, favorite, notes,
//! last used) in `hosts.toml`, keyed by alias.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ssh_config::{HostAlias, ResolveContext, ResolvedHost, Source, SshConfig};

/// Portico-side metadata for one alias.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostMeta {
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    pub notes: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used: Option<OffsetDateTime>,
}

/// Content of `hosts.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostMetaFile {
    pub hosts: BTreeMap<String, HostMeta>,
}

/// A host as displayed by the UIs.
#[derive(Debug, Clone)]
pub struct HostEntry {
    pub alias: String,
    pub source: Source,
    pub resolved: ResolvedHost,
    pub meta: HostMeta,
}

impl HostEntry {
    /// Text the fuzzy matcher searches: alias, hostname, user, tags, group.
    pub fn haystack(&self) -> String {
        let mut s = format!("{} {}", self.alias, self.resolved.hostname);
        if let Some(u) = &self.resolved.user {
            s.push(' ');
            s.push_str(u);
        }
        for t in &self.meta.tags {
            s.push_str(" #");
            s.push_str(t);
        }
        if let Some(g) = &self.meta.group {
            s.push(' ');
            s.push_str(g);
        }
        s
    }
}

/// Builds the host list: config aliases (declaration order) + metadata.
pub fn inventory(config: &SshConfig, meta: &HostMetaFile, ctx: &ResolveContext) -> Vec<HostEntry> {
    config
        .host_aliases()
        .into_iter()
        .map(|HostAlias { name, source }| HostEntry {
            resolved: config.resolve(&name, ctx),
            meta: meta.hosts.get(&name).cloned().unwrap_or_default(),
            alias: name,
            source,
        })
        .collect()
}
