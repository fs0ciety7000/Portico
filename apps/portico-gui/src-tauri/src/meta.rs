//! Host metadata and recent hosts (phase 4).
//!
//! Favorites, tags, group and notes live in `hosts.toml`, written by
//! `portico-core` (validated, normalized, atomic, never `~/.ssh/config`).
//! Every edit emits `host_meta` on `engine://event`, so all windows update
//! the host in place. Recent hosts come from `state/recent.toml`; a change
//! emits `recent_changed`.

// Tauri injects `State` (and deserialized arguments) by value.
#![allow(clippy::needless_pass_by_value)]

use portico_core::hosts::{HostMeta, HostMetaError, normalize_tags};
use portico_core::recent::{MAX_RECENT, RecentHost};
use portico_core::{Engine, ErrorSummary};
use serde::Serialize;
use tauri::State;

use crate::dto::unix_ms;

fn summary(e: portico_core::Error) -> ErrorSummary {
    e.summary()
}

#[tauri::command]
pub(crate) async fn set_favorite(
    engine: State<'_, Engine>,
    alias: String,
    favorite: bool,
) -> Result<HostMeta, ErrorSummary> {
    engine.set_favorite(&alias, favorite).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn set_tags(
    engine: State<'_, Engine>,
    alias: String,
    tags: Vec<String>,
) -> Result<HostMeta, ErrorSummary> {
    engine.set_tags(&alias, tags).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn set_group(
    engine: State<'_, Engine>,
    alias: String,
    group: Option<String>,
) -> Result<HostMeta, ErrorSummary> {
    engine.set_group(&alias, group).await.map_err(summary)
}

#[tauri::command]
pub(crate) async fn set_notes(
    engine: State<'_, Engine>,
    alias: String,
    notes: Option<String>,
) -> Result<HostMeta, ErrorSummary> {
    engine.set_notes(&alias, notes).await.map_err(summary)
}

/// A refused tag list: the core's summary, plus the 1-based position of the
/// offending tag when the error names one (`HostMetaError::Tag`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TagsErrorDto {
    #[serde(flatten)]
    pub summary: ErrorSummary,
    pub position: Option<usize>,
}

/// Validates and normalizes tags as `set_tags` would, without writing:
/// the edit dialog checks each chip as it is created. Synchronous (pure
/// computation, no I/O).
#[tauri::command]
pub(crate) fn check_tags(tags: Vec<String>) -> Result<Vec<String>, TagsErrorDto> {
    normalize_tags(&tags).map_err(|e| {
        let position = match &e {
            HostMetaError::Tag { position, .. } => Some(*position),
            _ => None,
        };
        TagsErrorDto {
            summary: portico_core::Error::from(e).summary(),
            position,
        }
    })
}

/// One recent host; `last` in Unix milliseconds.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecentDto {
    pub alias: String,
    pub last: i64,
    pub count: u32,
}

impl From<&RecentHost> for RecentDto {
    fn from(r: &RecentHost) -> Self {
        Self {
            alias: r.alias.clone(),
            last: unix_ms(r.last),
            count: r.count,
        }
    }
}

/// The `limit` most recent hosts (at most `MAX_RECENT`), most recent first.
/// The view keeps those still in the config.
#[tauri::command]
pub(crate) async fn recent_hosts(
    engine: State<'_, Engine>,
    limit: usize,
) -> Result<Vec<RecentDto>, ErrorSummary> {
    let list = engine
        .recent(limit.min(MAX_RECENT))
        .await
        .map_err(summary)?;
    Ok(list.iter().map(RecentDto::from).collect())
}

#[tauri::command]
pub(crate) async fn clear_recent(engine: State<'_, Engine>) -> Result<(), ErrorSummary> {
    engine.clear_recent().await.map_err(summary)
}

/// `false` when `alias` was not in the list.
#[tauri::command]
pub(crate) async fn forget_recent(
    engine: State<'_, Engine>,
    alias: String,
) -> Result<bool, ErrorSummary> {
    engine.forget_recent(&alias).await.map_err(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_dates_are_unix_milliseconds() {
        let r = RecentHost {
            alias: "web".into(),
            last: time::OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(1_500),
            count: 3,
        };
        let dto = RecentDto::from(&r);
        assert_eq!(dto.last, 1_500);
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            serde_json::json!({ "alias": "web", "last": 1500, "count": 3 })
        );
    }

    #[test]
    fn check_tags_normalizes_like_set_tags() {
        let ok = check_tags(vec!["#prod".into(), "db".into(), "PROD".into(), " ".into()]).unwrap();
        assert_eq!(ok, vec!["prod".to_owned(), "db".to_owned()]);
    }

    #[test]
    fn check_tags_names_the_offending_tag() {
        let err = check_tags(vec!["prod".into(), "bad tag".into()]).unwrap_err();
        assert_eq!(err.summary.code, "host_tag");
        assert_eq!(err.position, Some(2));
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["code"], "host_tag");
        assert_eq!(json["position"], 2);
        assert!(json["hint"].is_string());

        let many: Vec<String> = (0..17).map(|i| format!("t{i}")).collect();
        let err = check_tags(many).unwrap_err();
        assert_eq!(err.summary.code, "host_tags");
        assert_eq!(err.position, None);
    }
}
