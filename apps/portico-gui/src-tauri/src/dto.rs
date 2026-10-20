//! Serializable views sent to the webview (camelCase for TypeScript).

use portico_core::hosts::HostEntry;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostDto {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: u16,
    pub proxy_jump: Vec<String>,
    pub identity_files: Vec<String>,
    pub local_forwards: Vec<String>,
    pub remote_forwards: Vec<String>,
    pub tags: Vec<String>,
    pub group: Option<String>,
    pub favorite: bool,
    pub source: String,
    pub warnings: Vec<String>,
}

impl From<&HostEntry> for HostDto {
    fn from(h: &HostEntry) -> Self {
        let r = &h.resolved;
        Self {
            alias: h.alias.clone(),
            hostname: r.hostname.clone(),
            user: r.user.clone(),
            port: r.port,
            proxy_jump: r.proxy_jump.iter().map(|j| j.host.clone()).collect(),
            identity_files: r
                .identity_files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            local_forwards: r.local_forwards.clone(),
            remote_forwards: r.remote_forwards.clone(),
            tags: h.meta.tags.clone(),
            group: h.meta.group.clone(),
            favorite: h.meta.favorite,
            source: format!("{}:{}", h.source.file.display(), h.source.line),
            warnings: r.warnings.iter().map(|w| w.message.clone()).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThemeDto {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}
