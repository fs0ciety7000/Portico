//! Serializable views sent to the webview (camelCase for TypeScript).

use std::path::{Path, PathBuf};

use portico_core::engine::{PromptResponse, PtySize};
use portico_core::hosts::HostEntry;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

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
            identity_files: r.identity_files.iter().map(|p| display_path(p)).collect(),
            local_forwards: r.local_forwards.clone(),
            remote_forwards: r.remote_forwards.clone(),
            tags: h.meta.tags.clone(),
            group: h.meta.group.clone(),
            favorite: h.meta.favorite,
            source: format!("{}:{}", display_path(&h.source.file), h.source.line),
            warnings: r.warnings.iter().map(|w| w.message.clone()).collect(),
        }
    }
}

/// `~/…` for paths under the home directory (shorter, and does not expose
/// the account's home path in screenshots).
fn display_path(path: &Path) -> String {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| {
            path.strip_prefix(home)
                .ok()
                .map(|rest| format!("~/{}", rest.display()))
        })
        .unwrap_or_else(|| path.display().to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThemeDto {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

/// Terminal size from the view: cells, and pixels when known (0 otherwise).
#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct PtySizeDto {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

impl From<PtySizeDto> for PtySize {
    fn from(s: PtySizeDto) -> Self {
        Self {
            cols: s.cols.max(1),
            rows: s.rows.max(1),
            pixel_width: s.width,
            pixel_height: s.height,
        }
    }
}

/// Answer to a prompt, as sent by the webview.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PromptAnswerDto {
    AcceptHostKey,
    // Deserialized straight into `SecretString`: no intermediate `String`.
    Secret { value: SecretString, remember: bool },
    Answers { values: Vec<SecretString> },
    Cancel,
}

// Manual impl: the derived one would print secrets.
impl std::fmt::Debug for PromptAnswerDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::AcceptHostKey => "accept_host_key",
            Self::Secret { .. } => "secret",
            Self::Answers { .. } => "answers",
            Self::Cancel => "cancel",
        };
        f.debug_struct("PromptAnswerDto")
            .field("kind", &kind)
            .finish_non_exhaustive()
    }
}

impl From<PromptAnswerDto> for PromptResponse {
    fn from(dto: PromptAnswerDto) -> Self {
        match dto {
            PromptAnswerDto::AcceptHostKey => Self::AcceptHostKey,
            PromptAnswerDto::Secret { value, remember } => Self::Secret { value, remember },
            PromptAnswerDto::Answers { values } => Self::Answers(values),
            PromptAnswerDto::Cancel => Self::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_paths_are_shortened() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            display_path(&Path::new(&home).join(".ssh/config")),
            "~/.ssh/config"
        );
        assert_eq!(
            display_path(Path::new("/etc/ssh/ssh_config")),
            "/etc/ssh/ssh_config"
        );
    }

    #[test]
    fn prompt_answer_dto_debug_is_redacted() {
        let dto: PromptAnswerDto =
            serde_json::from_str(r#"{"kind":"secret","value":"hunter2","remember":true}"#).unwrap();
        assert!(!format!("{dto:?}").contains("hunter2"));
        let PromptResponse::Secret { value, remember } = dto.into() else {
            panic!("expected a secret");
        };
        assert!(remember);
        assert_eq!(secrecy::ExposeSecret::expose_secret(&value), "hunter2");
    }
}
