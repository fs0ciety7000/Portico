//! SFTP file browser model (Phase 2: backed by `russh-sftp` on the session's
//! transport). Transfers are chunked, report progress through
//! [`crate::events::SftpEvent`] and are cancellable.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One row of the remote file browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix permission bits.
    pub mode: Option<u32>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub modified: Option<OffsetDateTime>,
}

/// Sorting used by both UIs: directories first, then case-insensitive name.
pub fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|a, b| {
        let dir = |e: &RemoteEntry| e.kind != EntryKind::Dir;
        dir(a)
            .cmp(&dir(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Conflict policy when the destination exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    #[default]
    Ask,
    Overwrite,
    Skip,
    Rename,
}
