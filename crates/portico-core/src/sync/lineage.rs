//! Judging a key lineage (S2a, `docs/SYNC.md` §13.3.2, §13.3.7). Pure:
//! no I/O, no key derivation beyond HMAC.
//!
//! A device at epoch `E` (its floor) holds `C_E` (and, unlocked, `K_rot_E`)
//! and remembers `(e, C_e, K_rot_e)` of the epochs it held. It judges a
//! `bundle.toml` of another lineage or of a newer epoch **from its clear
//! header**, before it can open it:
//!
//! | Clear header | Verdict |
//! |---|---|
//! | no link of epoch `E` (shape already checked by the parser: ≤ 16 contiguous links, the last `E' − 1`) | [`Verdict::Tampered`], no rewrite (R10: no "unverifiable" case; a device more than 16 rotations behind re-joins) |
//! | link `E` with this device's `C_E`, proof valid under `K_rot_E` | [`Verdict::RotatedElsewhere`] (a locked device only compares the digest; the proof is checked after the new passphrase opens the file) |
//! | link `E` with this device's `C_E`, proof false | [`Verdict::ForgedFromOurs`]: rewritten from `bundle.verified` |
//! | link `E` with another digest, an earlier link verifying under a retained `(C_e, K_rot_e)` | [`Verdict::OtherLineage`]: `RotationConflict`, never rewritten (RR2) |
//! | otherwise | [`Verdict::Tampered`] |
//!
//! After the file opened (adoption), [`verify_adoption`] requires the link
//! of epoch `E` to carry the retained `C_E`; the parser already verified
//! every link against the sealed `K_rot` (a file that copies authentic
//! links and adds its own cannot prove the links it holds no key for).

use super::SyncError;
use super::bundle::{ClearHeader, OpenedBundle};
use super::crypto::{self, MAC_LEN, Subkey};
use super::state::RetainedEpoch;

/// What a clear header of another lineage or a newer epoch means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// A rotation that continues this device's lineage: adopt it by
    /// typing the new passphrase.
    RotatedElsewhere,
    /// A link from this device's `C_E` with a false proof: forged from
    /// this device's own lineage (rewrite from `bundle.verified`).
    ForgedFromOurs,
    /// An authentic fork of an earlier common epoch: concurrent rotations.
    OtherLineage,
    /// Anything else: not written by a key holder of this lineage.
    Tampered,
}

fn verifies(
    rotate: &Subkey,
    bundle_id: &[u8; crypto::ID_LEN],
    epoch: u32,
    digest: &[u8; MAC_LEN],
    next: &[u8; MAC_LEN],
    proof: &[u8],
) -> bool {
    crypto::verify_lineage_proof(rotate, bundle_id, epoch, digest, next, proof).unwrap_or(false)
}

/// Judges a clear header of an epoch above `epoch` (§13.3.2).
pub(crate) fn judge_newer(
    clear: &ClearHeader,
    epoch: u32,
    commit: &[u8; MAC_LEN],
    rotate: Option<&Subkey>,
    memory: &[RetainedEpoch],
) -> Verdict {
    if clear.epoch <= epoch {
        return Verdict::Tampered;
    }
    let Some((link, next)) = clear.link_at(epoch) else {
        return Verdict::Tampered;
    };
    if link.digest == *commit {
        return match rotate {
            None => Verdict::RotatedElsewhere,
            Some(rotate) => {
                if verifies(rotate, &clear.bundle_id, epoch, commit, &next, &link.proof) {
                    Verdict::RotatedElsewhere
                } else {
                    Verdict::ForgedFromOurs
                }
            }
        };
    }
    if forks_from_memory(clear, epoch, memory) {
        Verdict::OtherLineage
    } else {
        Verdict::Tampered
    }
}

/// Judges a file of another lineage at this device's own epoch (§13.3.7).
pub(crate) fn judge_other_lineage(
    clear: &ClearHeader,
    epoch: u32,
    memory: &[RetainedEpoch],
) -> SyncError {
    if forks_from_memory(clear, epoch, memory) {
        SyncError::RotationConflict
    } else {
        SyncError::BundleTampered
    }
}

/// Some link `e < epoch` of `clear` carries a `C_e` this device retained
/// and verifies under the retained `K_rot_e`: an authentic fork.
fn forks_from_memory(clear: &ClearHeader, epoch: u32, memory: &[RetainedEpoch]) -> bool {
    clear
        .links
        .iter()
        .filter(|link| link.epoch < epoch)
        .any(|link| {
            let Some(retained) = memory.iter().find(|r| r.epoch == link.epoch) else {
                return false;
            };
            let Some((_, next)) = clear.link_at(link.epoch) else {
                return false;
            };
            retained.commit_bytes() == Some(link.digest)
                && verifies(
                    &retained.rotate.subkey(),
                    &clear.bundle_id,
                    link.epoch,
                    &link.digest,
                    &next,
                    &link.proof,
                )
        })
}

/// Adoption (§13.3.2 step 2): `opened` is a newer epoch whose lineage
/// passes through `epoch` with this device's retained `C_E`. The whole
/// chain from there was verified when the file opened.
pub(crate) fn verify_adoption(
    opened: &OpenedBundle,
    epoch: u32,
    commit: &[u8; MAC_LEN],
) -> Result<(), SyncError> {
    if opened.header.epoch <= epoch {
        return Err(SyncError::BundleTampered);
    }
    match opened.links.iter().find(|link| link.epoch == epoch) {
        Some(link) if link.digest == *commit => Ok(()),
        _ => Err(SyncError::BundleTampered),
    }
}

/// "Use the other device's keys" after a fork (§13.3.7): the link of the
/// last common epoch must carry a retained `C_e` and its sealed `K_rot_e`
/// must be the retained one.
pub(crate) fn verify_fork_adoption(
    opened: &OpenedBundle,
    memory: &[RetainedEpoch],
) -> Result<(), SyncError> {
    let common = opened.links.iter().rev().any(|link| {
        let Some(retained) = memory.iter().find(|r| r.epoch == link.epoch) else {
            return false;
        };
        retained.commit_bytes() == Some(link.digest)
            && opened
                .rotate_key(link.epoch)
                .is_some_and(|key| crypto::KeyBytes::of(&key) == retained.rotate)
    });
    if common {
        Ok(())
    } else {
        Err(SyncError::BundleTampered)
    }
}

#[cfg(test)]
mod tests {
    use super::super::bundle;
    use super::super::bundle::tests::{new_bundle, pass};
    use super::super::crypto::{KdfBounds, KeyBytes};
    use super::super::passphrase::tests::{STRONG, STRONG_2, STRONG_3, typed};
    use super::super::state::LocalReplica;
    use super::*;

    fn rotate(opened: &OpenedBundle, passphrase: &str) -> (String, OpenedBundle) {
        bundle::rotate(
            opened,
            &typed(passphrase),
            opened.roster.clone(),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("rotate")
    }

    fn memory_of(opened: &[&OpenedBundle]) -> Vec<RetainedEpoch> {
        let mut local = LocalReplica::default();
        for bundle in opened {
            local.remember_epoch(
                bundle.header.epoch,
                &bundle.commit,
                KeyBytes::of(&bundle.keys.rotate),
            );
        }
        local.lineage
    }

    #[test]
    fn a_rotation_continues_the_lineage_and_a_fork_is_recognised() {
        let (_, n) = new_bundle();
        let (a_text, a) = rotate(&n, STRONG_2);
        let (c_text, c) = rotate(&n, STRONG_3);
        let a_clear = bundle::peek(&a_text).expect("peek");
        // A device still at N: A's file continues its lineage.
        assert_eq!(
            judge_newer(&a_clear, 1, &n.commit, Some(&n.keys.rotate), &[]),
            Verdict::RotatedElsewhere
        );
        assert_eq!(
            judge_newer(&a_clear, 1, &n.commit, None, &[]),
            Verdict::RotatedElsewhere,
            "locked: digest only"
        );
        verify_adoption(&a, 1, &n.commit).expect("adoption");
        // A device on A's lineage sees C's file: an authentic fork (it
        // remembers N), never tampering.
        let c_clear = bundle::peek(&c_text).expect("peek");
        let memory = memory_of(&[&n, &a]);
        assert_eq!(
            judge_other_lineage(&c_clear, 2, &memory),
            SyncError::RotationConflict
        );
        assert_eq!(
            judge_other_lineage(&c_clear, 2, &memory_of(&[&a])),
            SyncError::BundleTampered,
            "without K_rot_N: tampering, still no rewrite"
        );
        verify_fork_adoption(&c, &memory).expect("common epoch N");
        assert_eq!(
            verify_fork_adoption(&c, &memory_of(&[&a])),
            Err(SyncError::BundleTampered)
        );
        // Keep mine: N+2 from A's lineage; C (remembering N) sees a fork.
        let (k_text, _) = rotate(&a, STRONG);
        let k_clear = bundle::peek(&k_text).expect("peek");
        assert_eq!(
            judge_newer(
                &k_clear,
                2,
                &c.commit,
                Some(&c.keys.rotate),
                &memory_of(&[&n, &c])
            ),
            Verdict::OtherLineage
        );
    }

    #[test]
    fn rotation_without_valid_lineage_is_tampering() {
        let (_, n) = new_bundle();
        let (a_text, _) = rotate(&n, STRONG_2);
        let clear = bundle::peek(&a_text).expect("peek");
        // A false proof from this device's own C_E: forged from our lineage.
        let mut forged = clear.clone();
        if let Some(link) = forged.links.last_mut() {
            link.proof[0] ^= 1;
        }
        assert_eq!(
            judge_newer(&forged, 1, &n.commit, Some(&n.keys.rotate), &[]),
            Verdict::ForgedFromOurs
        );
        // Another digest at our epoch, no retained fork: tampering.
        let mut other = clear.clone();
        if let Some(link) = other.links.last_mut() {
            link.digest[0] ^= 1;
        }
        assert_eq!(
            judge_newer(&other, 1, &n.commit, Some(&n.keys.rotate), &[]),
            Verdict::Tampered
        );
        // No link of our epoch at all.
        assert_eq!(
            judge_newer(&clear, 7, &n.commit, Some(&n.keys.rotate), &[]),
            Verdict::Tampered
        );
    }

    #[test]
    fn locked_device_verifies_chain_against_retained_commitment() {
        let (_, n) = new_bundle();
        let (_, a) = rotate(&n, STRONG_2);
        let (b_text, _) = rotate(&a, STRONG_3);
        // Opened with the new passphrase only (no DK_N): the chain from N
        // is checked against the retained C_N.
        let opened = bundle::open(&b_text, &pass(STRONG_3), &KdfBounds::TEST).expect("open");
        verify_adoption(&opened, 1, &n.commit).expect("two rotations behind");
        let mut wrong = n.commit;
        wrong[0] ^= 1;
        assert_eq!(
            verify_adoption(&opened, 1, &wrong),
            Err(SyncError::BundleTampered)
        );
        assert_eq!(
            verify_adoption(&opened, 3, &opened.commit),
            Err(SyncError::BundleTampered),
            "never sideways"
        );
    }

    #[test]
    fn device_more_than_16_rotations_behind_must_rejoin() {
        let (_, first) = new_bundle();
        let mut current = first.duplicate().expect("dup");
        let phrases = [STRONG, STRONG_2, STRONG_3];
        for i in 0..17 {
            let phrase = phrases.get(i % 3).copied().unwrap_or(STRONG);
            current = rotate(&current, phrase).1;
        }
        assert_eq!(current.header.epoch, 18);
        assert_eq!(current.links.len(), bundle::MAX_LINEAGE_LINKS);
        assert_eq!(current.links.first().map(|l| l.epoch), Some(2));
        assert_eq!(
            verify_adoption(&current, 1, &first.commit),
            Err(SyncError::BundleTampered),
            "epoch 1 left the lineage"
        );
    }
}
