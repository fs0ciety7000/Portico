//! Serializable views sent to the webview (camelCase for TypeScript).

use portico_core::engine::PromptResponse;
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
