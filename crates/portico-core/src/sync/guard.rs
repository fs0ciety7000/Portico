//! Rollback, fork, downgrade and restore detection (S1a, `docs/SYNC.md`
//! §2.7, security review F4, F5, F17).
//!
//! The folder can be rolled back, frozen or partly synced by its provider
//! or by an attacker with write access. What this device has already
//! verified is remembered in [`HighWater`] (kept inside the sealed local
//! replica) and every new observation is checked against it:
//!
//! | Observation | Verdict |
//! |---|---|
//! | snapshot of `d` with `seq` < high-water | [`SyncError::Rollback`]: ignored |
//! | same `seq`, different SHA-256 | [`SyncError::Fork`]: one id on two machines |
//! | `bundle.toml` with `(epoch, keyset_generation, revision)` lower | [`SyncError::Rollback`] (F4) |
//! | same triple, different SHA-256 | [`BundleObservation::Diverged`]: concurrent rewrites, S1b picks the winner among the copies |
//! | file format older than one seen | [`SyncError::FormatDowngrade`] (ratchet) |
//! | own file ahead of the local replica | [`SyncError::RestoredState`] (F5): re-join under a new id |
//! | `seen[d]` of some snapshot > the `seq` present for `d` | [`Withheld`]: waiting, warning after a while |
//!
//! Observations never update a mark when they are refused. Staleness uses
//! the **local** time at which a new sequence was last observed, never the
//! `published_at` a device writes about itself (F17).
//!
//! Limits (documented in `docs/SYNC.md`): a device that has never synced
//! cannot detect a consistent rollback of the whole folder, and a total
//! freeze looks like "the other devices are off".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::SyncError;
use super::clock::DeviceId;
use super::crypto;

/// Last verified snapshot of one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceMark {
    pub(crate) seq: u64,
    /// SHA-256 of the file, hex.
    pub(crate) sha256: String,
    /// Local unix time when this `seq` was first seen (F17).
    pub(crate) observed_at: i64,
}

/// Last verified `bundle.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleMark {
    pub(crate) epoch: u32,
    pub(crate) keyset_generation: u64,
    pub(crate) revision: u64,
    pub(crate) sha256: String,
}

/// High-water marks of everything this device has verified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HighWater {
    pub(crate) devices: BTreeMap<DeviceId, DeviceMark>,
    pub(crate) bundle: Option<BundleMark>,
    /// Highest format version seen per file kind (ratchet).
    pub(crate) formats: BTreeMap<String, u32>,
}

/// Result of an accepted snapshot observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation {
    /// A newer (or first) snapshot: merge it.
    New,
    /// The same file as last time: nothing to do.
    Unchanged,
}

/// Result of an accepted `bundle.toml` observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BundleObservation {
    New,
    Unchanged,
    /// Same `(epoch, keyset_generation, revision)`, other content: two
    /// devices rewrote the bundle concurrently. The mark is not updated.
    Diverged,
}

/// What this device's own file in the folder says about the local state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnFile {
    /// The folder has what this device last published.
    InSync,
    /// The folder has an older (or no) copy: republish from local state.
    Behind,
}

/// A device whose latest snapshot other devices have merged but which is
/// not (yet) in the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Withheld {
    pub(crate) device: DeviceId,
    /// Highest `seen[device]` claimed by some snapshot.
    pub(crate) claimed: u64,
    /// The `seq` present in the folder (0: no file).
    pub(crate) present: u64,
}

fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut text, b| {
        let _ = write!(text, "{b:02x}");
        text
    })
}

impl HighWater {
    /// Checks a verified snapshot of `device` and records it when newer.
    pub(crate) fn observe_snapshot(
        &mut self,
        device: DeviceId,
        seq: u64,
        fingerprint: &[u8; 32],
        now: i64,
    ) -> Result<Observation, SyncError> {
        let sha256 = hex(fingerprint);
        if let Some(mark) = self.devices.get(&device) {
            if seq < mark.seq {
                return Err(SyncError::Rollback {
                    what: format!("snapshot of device {}", device.short()),
                });
            }
            if seq == mark.seq {
                return if mark.sha256 == sha256 {
                    Ok(Observation::Unchanged)
                } else {
                    Err(SyncError::Fork {
                        device: device.short(),
                    })
                };
            }
        }
        self.devices.insert(
            device,
            DeviceMark {
                seq,
                sha256,
                observed_at: now,
            },
        );
        Ok(Observation::New)
    }

    /// Checks a verified `bundle.toml` (F4): refuses an older epoch, keyset
    /// generation or revision.
    pub(crate) fn observe_bundle(
        &mut self,
        epoch: u32,
        keyset_generation: u64,
        revision: u64,
        fingerprint: &[u8; 32],
    ) -> Result<BundleObservation, SyncError> {
        let sha256 = hex(fingerprint);
        if let Some(mark) = &self.bundle {
            let seen = (mark.epoch, mark.keyset_generation, mark.revision);
            let found = (epoch, keyset_generation, revision);
            if found < seen {
                return Err(SyncError::Rollback {
                    what: "bundle.toml".to_owned(),
                });
            }
            if found == seen {
                return Ok(if mark.sha256 == sha256 {
                    BundleObservation::Unchanged
                } else {
                    BundleObservation::Diverged
                });
            }
        }
        self.bundle = Some(BundleMark {
            epoch,
            keyset_generation,
            revision,
            sha256,
        });
        Ok(BundleObservation::New)
    }

    /// Format-version ratchet: a file kind never goes back to an older
    /// format once a newer one was seen from this bundle.
    pub(crate) fn ratchet_format(
        &mut self,
        what: &'static str,
        version: u32,
    ) -> Result<(), SyncError> {
        let seen = self.formats.entry(what.to_owned()).or_insert(version);
        if version < *seen {
            return Err(SyncError::FormatDowngrade {
                what,
                found: version,
                seen: *seen,
            });
        }
        *seen = version;
        Ok(())
    }

    /// Devices whose last **new** sequence was observed locally more than
    /// `horizon_secs` ago (F17); they no longer block tombstone collection.
    pub(crate) fn stale_devices(&self, now: i64, horizon_secs: i64) -> Vec<DeviceId> {
        self.devices
            .iter()
            .filter(|(_, mark)| now.saturating_sub(mark.observed_at) > horizon_secs)
            .map(|(device, _)| *device)
            .collect()
    }
}

/// Tombstone collection horizon (owner decision #9): 180 days.
pub(crate) const STALE_HORIZON_SECS: i64 = 180 * 24 * 3600;

/// Compares this device's own file in the folder with its local state
/// (F5), at start and on every pass, **before** any write.
///
/// `published` is `(seq, clock, fingerprint)` of the own file found in the
/// folder, `None` when there is none. A file ahead of the local state
/// means the data dir was restored or cloned: [`SyncError::RestoredState`].
/// The same sequence with another content is a fork.
pub(crate) fn check_own_file(
    device: &DeviceId,
    local_seq: u64,
    local_clock: u64,
    local_fingerprint: Option<&[u8; 32]>,
    published: Option<(u64, u64, &[u8; 32])>,
) -> Result<OwnFile, SyncError> {
    let Some((seq, clock, fingerprint)) = published else {
        return Ok(OwnFile::Behind);
    };
    if seq > local_seq || clock > local_clock {
        return Err(SyncError::RestoredState {
            device: device.short(),
        });
    }
    if seq < local_seq {
        return Ok(OwnFile::Behind);
    }
    match local_fingerprint {
        Some(local) if local != fingerprint => Err(SyncError::Fork {
            device: device.short(),
        }),
        _ => Ok(OwnFile::InSync),
    }
}

/// Devices some snapshot claims to have merged beyond what the folder
/// holds for them ("device X published seq 12, we only have 9").
pub(crate) fn withheld<'a>(
    present: &BTreeMap<DeviceId, u64>,
    seen_vectors: impl IntoIterator<Item = &'a BTreeMap<DeviceId, u64>>,
) -> Vec<Withheld> {
    let mut claimed: BTreeMap<DeviceId, u64> = BTreeMap::new();
    for seen in seen_vectors {
        for (device, &seq) in seen {
            let entry = claimed.entry(*device).or_insert(0);
            *entry = (*entry).max(seq);
        }
    }
    claimed
        .into_iter()
        .filter_map(|(device, claimed)| {
            let present = present.get(&device).copied().unwrap_or(0);
            (claimed > present).then_some(Withheld {
                device,
                claimed,
                present,
            })
        })
        .collect()
}

/// SHA-256 of file bytes, for callers that hold only the bytes.
pub(crate) fn fingerprint(bytes: &[u8]) -> [u8; 32] {
    crypto::sha256(bytes)
}

#[cfg(test)]
mod tests {
    use super::super::bundle::tests::{new_bundle, pass};
    use super::super::bundle::{self, change_passphrase};
    use super::super::crypto::{ID_LEN, KdfBounds};
    use super::super::passphrase::tests::STRONG;
    use super::*;

    fn device(n: u8) -> DeviceId {
        DeviceId::from_bytes([n; ID_LEN])
    }

    #[test]
    fn snapshot_rollback_and_fork_are_refused() {
        let mut marks = HighWater::default();
        let d = device(1);
        assert_eq!(
            marks.observe_snapshot(d, 5, &[1; 32], 100),
            Ok(Observation::New)
        );
        assert_eq!(
            marks.observe_snapshot(d, 5, &[1; 32], 200),
            Ok(Observation::Unchanged)
        );
        assert_eq!(
            marks.devices[&d].observed_at, 100,
            "unchanged keeps the time"
        );
        assert!(matches!(
            marks.observe_snapshot(d, 4, &[9; 32], 300),
            Err(SyncError::Rollback { .. })
        ));
        assert!(matches!(
            marks.observe_snapshot(d, 5, &[2; 32], 300),
            Err(SyncError::Fork { .. })
        ));
        assert_eq!(marks.devices[&d].seq, 5, "refusals change nothing");
        assert_eq!(marks.devices[&d].sha256, hex(&[1; 32]));
        assert_eq!(
            marks.observe_snapshot(d, 6, &[3; 32], 400),
            Ok(Observation::New)
        );
        assert_eq!(marks.devices[&d].observed_at, 400);
        // Other devices are independent.
        assert_eq!(
            marks.observe_snapshot(device(2), 1, &[1; 32], 0),
            Ok(Observation::New)
        );
    }

    #[test]
    fn bundle_downgrade_is_refused() {
        let mut marks = HighWater::default();
        assert_eq!(
            marks.observe_bundle(1, 2, 3, &[1; 32]),
            Ok(BundleObservation::New)
        );
        assert_eq!(
            marks.observe_bundle(1, 2, 3, &[1; 32]),
            Ok(BundleObservation::Unchanged)
        );
        assert_eq!(
            marks.observe_bundle(1, 2, 3, &[2; 32]),
            Ok(BundleObservation::Diverged)
        );
        for (epoch, generation, revision) in [(1, 2, 2), (1, 1, 9), (0, 9, 9)] {
            assert!(
                matches!(
                    marks.observe_bundle(epoch, generation, revision, &[5; 32]),
                    Err(SyncError::Rollback { .. })
                ),
                "{epoch} {generation} {revision}"
            );
        }
        assert_eq!(marks.bundle.as_ref().map(|m| m.revision), Some(3));
        assert_eq!(
            marks.observe_bundle(1, 2, 4, &[6; 32]),
            Ok(BundleObservation::New)
        );
        assert_eq!(
            marks.observe_bundle(1, 3, 1, &[7; 32]),
            Ok(BundleObservation::New)
        );
        assert_eq!(
            marks.observe_bundle(2, 1, 1, &[8; 32]),
            Ok(BundleObservation::New)
        );
    }

    #[test]
    fn older_bundle_toml_is_refused_after_passphrase_change() {
        let (old_text, _) = new_bundle();
        let new = "zoom zone zippy zipper zigzag zesty";
        let (new_text, _) = change_passphrase(
            &old_text,
            &pass(STRONG),
            &pass(new),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("change");
        let mut marks = HighWater::default();
        let current = bundle::open(&new_text, &pass(new), &KdfBounds::TEST).expect("open");
        let header = &current.header;
        marks
            .observe_bundle(
                header.epoch,
                header.keyset_generation,
                header.revision,
                &current.fingerprint,
            )
            .expect("new");
        // The provider puts the old file back: it opens (old passphrase,
        // same data key), but the high-water mark refuses it.
        let old = bundle::open_with_key(&old_text, &current.data_key).expect("valid old copy");
        assert!(matches!(
            marks.observe_bundle(
                old.header.epoch,
                old.header.keyset_generation,
                old.header.revision,
                &old.fingerprint
            ),
            Err(SyncError::Rollback { .. })
        ));
    }

    #[test]
    fn format_ratchet_refuses_downgrades() {
        let mut marks = HighWater::default();
        marks.ratchet_format("snapshot", 1).expect("first");
        marks.ratchet_format("snapshot", 2).expect("upgrade");
        assert_eq!(
            marks.ratchet_format("snapshot", 1),
            Err(SyncError::FormatDowngrade {
                what: "snapshot",
                found: 1,
                seen: 2
            })
        );
        marks
            .ratchet_format("bundle.toml", 1)
            .expect("independent kinds");
        marks.ratchet_format("snapshot", 2).expect("same");
    }

    #[test]
    fn restored_local_state_is_detected_from_the_own_file() {
        let me = device(1);
        // Normal cases.
        assert_eq!(check_own_file(&me, 0, 0, None, None), Ok(OwnFile::Behind));
        assert_eq!(
            check_own_file(&me, 7, 30, Some(&[1; 32]), Some((7, 30, &[1; 32]))),
            Ok(OwnFile::InSync)
        );
        assert_eq!(
            check_own_file(&me, 7, 30, None, Some((5, 20, &[1; 32]))),
            Ok(OwnFile::Behind)
        );
        // The data dir was restored: the folder is ahead → re-join.
        assert!(matches!(
            check_own_file(&me, 5, 20, None, Some((7, 30, &[1; 32]))),
            Err(SyncError::RestoredState { .. })
        ));
        assert!(matches!(
            check_own_file(&me, 7, 20, None, Some((7, 30, &[1; 32]))),
            Err(SyncError::RestoredState { .. })
        ));
        // Same seq, other content: a second machine writes under this id.
        assert!(matches!(
            check_own_file(&me, 7, 30, Some(&[1; 32]), Some((7, 30, &[2; 32]))),
            Err(SyncError::Fork { .. })
        ));
    }

    #[test]
    fn withheld_updates_are_reported() {
        let present = BTreeMap::from([(device(1), 9), (device(2), 4)]);
        let seen_a = BTreeMap::from([(device(1), 12), (device(2), 4)]);
        let seen_b = BTreeMap::from([(device(1), 10), (device(3), 2)]);
        let report = withheld(&present, [&seen_a, &seen_b]);
        assert_eq!(
            report,
            vec![
                Withheld {
                    device: device(1),
                    claimed: 12,
                    present: 9
                },
                Withheld {
                    device: device(3),
                    claimed: 2,
                    present: 0
                },
            ]
        );
        assert!(withheld(&present, []).is_empty());
    }

    #[test]
    fn staleness_uses_local_observation_time() {
        let mut marks = HighWater::default();
        marks
            .observe_snapshot(device(1), 1, &[1; 32], 0)
            .expect("new");
        marks
            .observe_snapshot(device(2), 1, &[1; 32], STALE_HORIZON_SECS)
            .expect("new");
        // Re-observing the same file does not refresh the time.
        marks
            .observe_snapshot(device(1), 1, &[1; 32], STALE_HORIZON_SECS)
            .expect("same");
        let now = STALE_HORIZON_SECS + 10;
        assert_eq!(
            marks.stale_devices(now, STALE_HORIZON_SECS),
            vec![device(1)]
        );
    }

    #[test]
    fn high_water_round_trips_through_json() {
        let mut marks = HighWater::default();
        marks
            .observe_snapshot(device(1), 3, &[1; 32], 5)
            .expect("new");
        marks
            .observe_bundle(1, 1, 1, &fingerprint(b"bundle"))
            .expect("new");
        marks.ratchet_format("snapshot", 1).expect("ratchet");
        let json = serde_json::to_string(&marks).expect("ser");
        assert_eq!(serde_json::from_str::<HighWater>(&json).expect("de"), marks);
    }
}
