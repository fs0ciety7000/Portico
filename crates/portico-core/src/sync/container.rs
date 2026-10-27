//! Sealed containers of the folder sync (S1a): device snapshots
//! (`devices/<device-id>.snap`) and this device's local replica
//! (`data_dir/sync/replica.snap`, security review F5).
//!
//! Both are a small TOML envelope around one XChaCha20-Poly1305 message:
//!
//! ```toml
//! format = "portico-sync-snapshot"   # or "portico-sync-replica"
//! version = 1
//! device = "hex(16)"
//! epoch = 1
//! keyset_generation = 1
//! seq = 42                           # publish sequence / local generation
//! nonce = "base64(24)"
//! ciphertext = "base64(…)"
//! ```
//!
//! - **Keys**: the snapshot subkey for snapshots, the local subkey for the
//!   replica (a replica never opens as a snapshot, nor the reverse).
//! - **AAD** (canonical encoding): label, bundle id, epoch, keyset
//!   generation, device id, sequence, envelope version, body format and
//!   padded length. The same binding is repeated inside the plaintext and
//!   compared after decryption. Opening also checks the device the caller
//!   expects (the file name), so a snapshot replayed under another
//!   device's name is refused, and refuses a keyset generation above the
//!   bundle's current one ([`Expected`]).
//! - **Fingerprint** (high-water marks): SHA-256 of the authenticated
//!   content only (AAD, nonce, ciphertext), never of the file bytes, so
//!   the provider can reformat the TOML without faking a fork.
//! - **Plaintext**: JSON padded with spaces to a power-of-two bucket of at
//!   least 16 KiB, at most 4 MiB; the file is capped at 8 MiB and its size
//!   is checked before parsing.
//! - **Snapshot content**: device name (F16), app version, publish time
//!   (display only), dot clock, "seen" vector, and the full merged
//!   [`SyncState`] with its global causal context (F6).
//! - **Replica content**: clock, publish sequence and the high-water marks
//!   live inside the same sealed, atomically written file as the state, so
//!   they can never be restored separately (F5).

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::SyncError;
use super::clock::{DeviceId, DeviceName};
use super::crypto::{self, Canonical, ID_LEN, MIN_BUCKET, NONCE_LEN, Subkey, Subkeys, TAG_LEN};
use super::guard::HighWater;
use super::model::{MAX_CONTEXT_DEVICES, SyncState};
use super::state::LocalReplica;

/// Envelope version of both containers.
pub(crate) const CONTAINER_VERSION: u32 = 1;
/// Version of the decrypted body.
pub(crate) const BODY_FORMAT: u32 = 1;
/// Largest snapshot or replica file.
pub(crate) const MAX_CONTAINER_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Longest `app_version` string.
const MAX_APP_VERSION_CHARS: usize = 32;

const FILE_COMMENT: &str = "# Portico encrypted sync data. Do not edit.\n\n";

/// Which container: decides the format string, the AAD label and the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerKind {
    Snapshot,
    Replica,
}

impl ContainerKind {
    fn format(self) -> &'static str {
        match self {
            Self::Snapshot => "portico-sync-snapshot",
            Self::Replica => "portico-sync-replica",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Replica => "local replica",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Replica => "local",
        }
    }

    fn key(self, keys: &Subkeys) -> &Subkey {
        match self {
            Self::Snapshot => &keys.snapshot,
            Self::Replica => &keys.local,
        }
    }
}

/// What a container is bound to (AAD and body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) bundle_id: [u8; ID_LEN],
    pub(crate) epoch: u32,
    /// Generation of the key slots when sealed (≤ the bundle's current).
    pub(crate) keyset_generation: u64,
    pub(crate) device: DeviceId,
    /// Publish sequence (snapshot) or local generation (replica), ≥ 1.
    pub(crate) seq: u64,
}

/// Content of a device snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SnapshotContent {
    pub(crate) device_name: DeviceName,
    pub(crate) app_version: String,
    /// Unix seconds, display only (never used for ordering or staleness).
    pub(crate) published_at: i64,
    /// Highest dot counter issued by this device.
    pub(crate) clock: u64,
    /// Publish sequences of the snapshots merged into this state.
    pub(crate) seen: BTreeMap<DeviceId, u64>,
    pub(crate) state: SyncState,
}

/// Content of the local replica: one atomic file for everything that must
/// never go back on its own (F5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplicaContent {
    pub(crate) device_name: DeviceName,
    pub(crate) clock: u64,
    /// Last publish sequence of this device.
    pub(crate) publish_seq: u64,
    pub(crate) state: SyncState,
    pub(crate) high_water: HighWater,
    /// What the engine keeps besides (S1b): merged sequences, last applied
    /// projection, confirmations (`state::LocalReplica`).
    #[serde(default)]
    pub(crate) local: LocalReplica,
}

/// What the reader expects of a container, from the verified
/// `bundle.toml` and the file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Expected {
    pub(crate) bundle_id: [u8; ID_LEN],
    pub(crate) epoch: u32,
    /// The bundle's current keyset generation: a container sealed under a
    /// higher one is refused (S1a review finding 6).
    pub(crate) max_keyset_generation: u64,
    /// The device the file name designates.
    pub(crate) device: DeviceId,
}

/// A verified container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Opened<C> {
    pub(crate) binding: Binding,
    pub(crate) content: C,
    /// Fingerprint of the authenticated content (per-device high-water
    /// mark): see [`fingerprint`].
    pub(crate) fingerprint: [u8; 32],
}

/// A sealed container: the file text and its fingerprint (what
/// `guard::check_own_file` compares with the copy found in the folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sealed {
    pub(crate) text: String,
    pub(crate) fingerprint: [u8; 32],
}

/// Fingerprint of a container (S1a review finding 1): SHA-256 of what the
/// AEAD authenticates, decoded (AAD, which binds the kind, the binding and
/// the padded length; nonce; ciphertext and tag). The TOML around it (a
/// comment, whitespace, key order, CRLF) does not enter it.
fn fingerprint(aad: &[u8], nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> [u8; 32] {
    crypto::content_fingerprint("portico-sync/1 container", &[aad, nonce, ciphertext])
}

#[derive(Deserialize)]
struct VersionProbe {
    format: Option<String>,
    version: Option<u32>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    format: String,
    version: u32,
    device: DeviceId,
    epoch: u32,
    keyset_generation: u64,
    seq: u64,
    nonce: String,
    ciphertext: String,
}

/// The decrypted body: binding repeated, then the content. Unknown fields
/// are ignored (additive changes within format 1).
#[derive(Serialize, Deserialize)]
struct Body<C> {
    format: u32,
    bundle_id: String,
    epoch: u32,
    keyset_generation: u64,
    device: DeviceId,
    seq: u64,
    content: C,
}

fn aad(kind: ContainerKind, binding: &Binding, padded_len: usize) -> Vec<u8> {
    let mut aad = Canonical::new(kind.label());
    aad.bytes(&binding.bundle_id)
        .u32(binding.epoch)
        .u64(binding.keyset_generation)
        .bytes(binding.device.bytes())
        .u64(binding.seq)
        .u32(CONTAINER_VERSION)
        .u32(BODY_FORMAT)
        .u64(u64::try_from(padded_len).unwrap_or(u64::MAX));
    aad.finish().to_vec()
}

/// Seals `content` under `binding`; returns the file text and its
/// fingerprint.
pub(crate) fn seal<C: Serialize>(
    kind: ContainerKind,
    keys: &Subkeys,
    binding: &Binding,
    content: &C,
) -> Result<Sealed, SyncError> {
    if binding.seq == 0 || binding.epoch == 0 || binding.keyset_generation == 0 {
        return Err(SyncError::Corrupt {
            what: kind.what(),
            reason: "zero sequence, epoch or generation",
        });
    }
    let body = Body {
        format: BODY_FORMAT,
        bundle_id: B64.encode(binding.bundle_id),
        epoch: binding.epoch,
        keyset_generation: binding.keyset_generation,
        device: binding.device,
        seq: binding.seq,
        content,
    };
    // Serialized straight into its padded, zeroizing buffer: no
    // reallocation leaves plaintext behind (S1a review finding 6).
    let padded = crypto::padded_json(&body, MIN_BUCKET, kind.what())?;
    let aad = aad(kind, binding, padded.len());
    let (nonce, ciphertext) = crypto::seal(kind.key(keys).bytes(), &aad, padded)?;
    let fingerprint = fingerprint(&aad, &nonce, &ciphertext);
    let envelope = Envelope {
        format: kind.format().to_owned(),
        version: CONTAINER_VERSION,
        device: binding.device,
        epoch: binding.epoch,
        keyset_generation: binding.keyset_generation,
        seq: binding.seq,
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(&ciphertext),
    };
    let text =
        toml::to_string_pretty(&envelope).map_err(|_| SyncError::Crypto("encoding failed"))?;
    let text = format!("{FILE_COMMENT}{text}");
    if text.len() > MAX_CONTAINER_FILE_BYTES {
        return Err(SyncError::TooLarge {
            what: kind.what(),
            max: MAX_CONTAINER_FILE_BYTES,
        });
    }
    Ok(Sealed { text, fingerprint })
}

/// Opens a container read from the folder (or the data dir).
pub(crate) fn open<C: DeserializeOwned>(
    kind: ContainerKind,
    keys: &Subkeys,
    expected: &Expected,
    bytes: &[u8],
) -> Result<Opened<C>, SyncError> {
    let what = kind.what();
    let corrupt = |reason| SyncError::Corrupt { what, reason };
    let unauthenticated = || SyncError::Unauthenticated {
        what,
        device: expected.device.short(),
    };
    if bytes.len() > MAX_CONTAINER_FILE_BYTES {
        return Err(SyncError::TooLarge {
            what,
            max: MAX_CONTAINER_FILE_BYTES,
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| corrupt("not UTF-8"))?;
    let probe: VersionProbe = toml::from_str(text).map_err(|_| corrupt("not valid TOML"))?;
    if probe.format.as_deref() != Some(kind.format()) {
        return Err(corrupt("not a Portico sync file of this kind"));
    }
    let version = probe.version.ok_or_else(|| corrupt("no version"))?;
    if version != CONTAINER_VERSION {
        return Err(SyncError::UnsupportedVersion {
            what,
            found: version,
            supported: CONTAINER_VERSION,
        });
    }
    let envelope: Envelope = toml::from_str(text).map_err(|_| corrupt("unexpected content"))?;
    // Another device's file under this name, or another epoch: the AEAD
    // would fail anyway; refusing first gives a precise reason.
    if envelope.device != expected.device || envelope.epoch != expected.epoch {
        return Err(unauthenticated());
    }
    let nonce: [u8; NONCE_LEN] = crypto::exact(
        &B64.decode(&envelope.nonce)
            .map_err(|_| corrupt("bad nonce"))?,
        what,
        "bad nonce",
    )?;
    let ciphertext = B64
        .decode(&envelope.ciphertext)
        .map_err(|_| corrupt("bad ciphertext"))?;
    let padded_len = ciphertext.len().saturating_sub(TAG_LEN);
    if ciphertext.len() < TAG_LEN || !crypto::is_bucket(padded_len, MIN_BUCKET) {
        return Err(corrupt("bad ciphertext length"));
    }
    let binding = Binding {
        bundle_id: expected.bundle_id,
        epoch: expected.epoch,
        keyset_generation: envelope.keyset_generation,
        device: envelope.device,
        seq: envelope.seq,
    };
    let aad = aad(kind, &binding, padded_len);
    let plaintext = crypto::open(kind.key(keys).bytes(), &nonce, &aad, &ciphertext)
        .map_err(|_| unauthenticated())?;
    // Authenticated from here on.
    if binding.keyset_generation > expected.max_keyset_generation {
        return Err(corrupt("sealed under a newer key set than the bundle"));
    }
    let body: Body<C> = serde_json::from_slice(&plaintext).map_err(|_| corrupt("bad content"))?;
    if body.format != BODY_FORMAT {
        return Err(SyncError::UnsupportedVersion {
            what,
            found: body.format,
            supported: BODY_FORMAT,
        });
    }
    let same_binding = body.bundle_id == B64.encode(binding.bundle_id)
        && body.epoch == binding.epoch
        && body.keyset_generation == binding.keyset_generation
        && body.device == binding.device
        && body.seq == binding.seq;
    if !same_binding || binding.seq == 0 || binding.keyset_generation == 0 {
        return Err(corrupt("content does not match its envelope"));
    }
    Ok(Opened {
        binding,
        content: body.content,
        fingerprint: fingerprint(&aad, &nonce, &ciphertext),
    })
}

fn check_app_version(version: &str) -> Result<(), SyncError> {
    let ok = version.chars().count() <= MAX_APP_VERSION_CHARS
        && version.chars().all(|c| c.is_ascii_graphic());
    if ok {
        Ok(())
    } else {
        Err(SyncError::Corrupt {
            what: "snapshot",
            reason: "bad app version",
        })
    }
}

/// Seals this device's snapshot.
pub(crate) fn seal_snapshot(
    keys: &Subkeys,
    binding: &Binding,
    content: &SnapshotContent,
) -> Result<Sealed, SyncError> {
    check_app_version(&content.app_version)?;
    seal(ContainerKind::Snapshot, keys, binding, content)
}

/// Opens and checks a device snapshot: the state invariants, the clock
/// against the device's own entry of the context, the size of `seen`.
pub(crate) fn open_snapshot(
    keys: &Subkeys,
    expected: &Expected,
    bytes: &[u8],
) -> Result<Opened<SnapshotContent>, SyncError> {
    let opened: Opened<SnapshotContent> = open(ContainerKind::Snapshot, keys, expected, bytes)?;
    let content = &opened.content;
    check_app_version(&content.app_version)?;
    content.state.check_invariants()?;
    let corrupt = |reason| SyncError::Corrupt {
        what: "snapshot",
        reason,
    };
    if content.state.context().get(&opened.binding.device) > content.clock {
        return Err(corrupt("context ahead of the device clock"));
    }
    if content.seen.len() > MAX_CONTEXT_DEVICES {
        return Err(corrupt("too many devices in the seen vector"));
    }
    if content
        .seen
        .get(&opened.binding.device)
        .copied()
        .unwrap_or(0)
        >= opened.binding.seq
    {
        return Err(corrupt("seen vector ahead of its own sequence"));
    }
    Ok(opened)
}

/// Seals the local replica (`seq` = local generation).
pub(crate) fn seal_replica(
    keys: &Subkeys,
    binding: &Binding,
    content: &ReplicaContent,
) -> Result<Sealed, SyncError> {
    seal(ContainerKind::Replica, keys, binding, content)
}

/// Opens the local replica of `expected.device`.
pub(crate) fn open_replica(
    keys: &Subkeys,
    expected: &Expected,
    bytes: &[u8],
) -> Result<Opened<ReplicaContent>, SyncError> {
    let opened: Opened<ReplicaContent> = open(ContainerKind::Replica, keys, expected, bytes)?;
    opened.content.state.check_invariants()?;
    if opened.content.state.context().get(&expected.device) > opened.content.clock {
        return Err(SyncError::Corrupt {
            what: "local replica",
            reason: "context ahead of the device clock",
        });
    }
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use super::super::crypto::{DataKey, KEY_LEN, derive_subkeys};
    use super::super::model::tests::{Replica, host, host_key};
    use super::super::model::{RecordKey, RecordValue, SnippetScopeValue, SnippetValue};
    use super::*;

    const BUNDLE: [u8; ID_LEN] = [7; ID_LEN];

    fn keys() -> Subkeys {
        derive_subkeys(&DataKey::from_bytes(&[3; KEY_LEN]).expect("dk"), &BUNDLE, 1).expect("keys")
    }

    fn me() -> DeviceId {
        DeviceId::from_bytes([1; ID_LEN])
    }

    fn binding(seq: u64) -> Binding {
        Binding {
            bundle_id: BUNDLE,
            epoch: 1,
            keyset_generation: 1,
            device: me(),
            seq,
        }
    }

    fn content() -> SnapshotContent {
        let mut replica = Replica::new(1);
        replica.write(host_key("prod"), host("10.0.0.1"));
        replica.write(
            RecordKey::Snippet { id: "logs".into() },
            RecordValue::Snippet(SnippetValue {
                name: "Logs".into(),
                command: "journalctl -f".into(),
                description: None,
                scope: SnippetScopeValue::Global,
                run: true,
            }),
        );
        SnapshotContent {
            device_name: DeviceName::parse("Laptop").expect("name"),
            app_version: "0.5.0".into(),
            published_at: 1_700_000_000,
            clock: replica.clock.counter(),
            seen: BTreeMap::from([(DeviceId::from_bytes([2; ID_LEN]), 3)]),
            state: replica.state,
        }
    }

    fn expected(bundle_id: [u8; ID_LEN], epoch: u32, device: DeviceId) -> Expected {
        Expected {
            bundle_id,
            epoch,
            max_keyset_generation: 1,
            device,
        }
    }

    fn open_mine(bytes: &[u8]) -> Result<Opened<SnapshotContent>, SyncError> {
        open_snapshot(&keys(), &expected(BUNDLE, 1, me()), bytes)
    }

    fn edit(text: &str, edit: impl FnOnce(&mut Envelope)) -> String {
        let body = text.strip_prefix(FILE_COMMENT).expect("comment");
        let mut envelope: Envelope = toml::from_str(body).expect("parse");
        edit(&mut envelope);
        format!(
            "{FILE_COMMENT}{}",
            toml::to_string_pretty(&envelope).expect("render")
        )
    }

    #[test]
    fn snapshot_round_trips() {
        let sealed = seal_snapshot(&keys(), &binding(42), &content()).expect("seal");
        let text = sealed.text;
        assert!(!text.contains("prod") && !text.contains("Laptop") && !text.contains("journalctl"));
        let opened = open_mine(text.as_bytes()).expect("open");
        assert_eq!(opened.binding, binding(42));
        assert_eq!(opened.content, content());
        assert_eq!(opened.fingerprint, sealed.fingerprint);
        // A second seal of the same content differs (fresh nonce).
        let again = seal_snapshot(&keys(), &binding(42), &content())
            .expect("seal")
            .text;
        assert_ne!(again, text);
    }

    #[test]
    fn every_aad_field_is_bound() {
        let text = seal_snapshot(&keys(), &binding(42), &content())
            .expect("seal")
            .text;
        let unauthenticated = |result: Result<Opened<SnapshotContent>, SyncError>| {
            matches!(result, Err(SyncError::Unauthenticated { .. }))
        };
        // Envelope fields that enter the AAD.
        assert!(unauthenticated(open_mine(
            edit(&text, |e| e.seq = 43).as_bytes()
        )));
        assert!(unauthenticated(open_mine(
            edit(&text, |e| e.seq = 41).as_bytes()
        )));
        assert!(unauthenticated(open_mine(
            edit(&text, |e| e.keyset_generation = 2).as_bytes()
        )));
        // Device swap: another device's name on this file…
        let other = DeviceId::from_bytes([2; ID_LEN]);
        assert!(unauthenticated(open_snapshot(
            &keys(),
            &expected(BUNDLE, 1, other),
            text.as_bytes()
        )));
        // …or the envelope edited to match the file name.
        let swapped = edit(&text, |e| e.device = other);
        assert!(unauthenticated(open_snapshot(
            &keys(),
            &expected(BUNDLE, 1, other),
            swapped.as_bytes()
        )));
        // Epoch and bundle id the reader expects.
        assert!(unauthenticated(open_snapshot(
            &keys(),
            &expected(BUNDLE, 2, me()),
            text.as_bytes()
        )));
        let epoch_edit = edit(&text, |e| e.epoch = 2);
        assert!(unauthenticated(open_snapshot(
            &keys(),
            &expected(BUNDLE, 2, me()),
            epoch_edit.as_bytes()
        )));
        assert!(unauthenticated(open_snapshot(
            &keys(),
            &expected([8; ID_LEN], 1, me()),
            text.as_bytes()
        )));
        // Keys of another bundle or epoch.
        let other_keys =
            derive_subkeys(&DataKey::from_bytes(&[3; KEY_LEN]).expect("dk"), &BUNDLE, 2)
                .expect("keys");
        assert!(unauthenticated(open_snapshot(
            &other_keys,
            &expected(BUNDLE, 1, me()),
            text.as_bytes()
        )));
        // Padded length: a ciphertext cut to a smaller bucket.
        let truncated = edit(&text, |e| {
            let mut ct = B64.decode(&e.ciphertext).expect("b64");
            ct.truncate(MIN_BUCKET / 2 + TAG_LEN);
            e.ciphertext = B64.encode(ct);
        });
        assert!(open_mine(truncated.as_bytes()).is_err());
        // A replica never opens as a snapshot, nor the reverse.
        let replica_text = edit(&text, |e| e.format = ContainerKind::Replica.format().into());
        assert!(
            open::<SnapshotContent>(
                ContainerKind::Replica,
                &keys(),
                &expected(BUNDLE, 1, me()),
                replica_text.as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn a_tampered_byte_anywhere_is_rejected() {
        let text = seal_snapshot(&keys(), &binding(42), &content())
            .expect("seal")
            .text;
        let bytes = text.as_bytes();
        for index in (FILE_COMMENT.len()..bytes.len()).step_by(5) {
            let mut altered = bytes.to_vec();
            altered[index] ^= 0x01;
            assert!(open_mine(&altered).is_err(), "byte {index}");
        }
        // And every byte of the raw ciphertext.
        let envelope: Envelope =
            toml::from_str(text.strip_prefix(FILE_COMMENT).expect("c")).expect("p");
        let ciphertext = B64.decode(&envelope.ciphertext).expect("b64");
        let nonce: [u8; NONCE_LEN] = B64
            .decode(&envelope.nonce)
            .expect("b64")
            .try_into()
            .expect("24");
        let aad = aad(
            ContainerKind::Snapshot,
            &binding(42),
            ciphertext.len() - TAG_LEN,
        );
        for index in (0..ciphertext.len()).step_by(3) {
            let mut altered = ciphertext.clone();
            altered[index] ^= 0x80;
            assert!(crypto::open(keys().snapshot.bytes(), &nonce, &aad, &altered).is_err());
        }
    }

    #[test]
    fn padding_hides_the_size_within_a_bucket() {
        let small = seal_snapshot(&keys(), &binding(1), &content())
            .expect("seal")
            .text;
        let mut bigger = content();
        let mut replica = Replica::new(1);
        for i in 0..20 {
            replica.write(host_key(&format!("h{i}")), host("10.0.0.1"));
        }
        bigger.state = replica.state;
        bigger.clock = replica.clock.counter();
        let medium = seal_snapshot(&keys(), &binding(1), &bigger)
            .expect("seal")
            .text;
        assert_eq!(small.len(), medium.len(), "same 16 KiB bucket");
        let ct_len = |text: &str| {
            let envelope: Envelope =
                toml::from_str(text.strip_prefix(FILE_COMMENT).expect("c")).expect("p");
            B64.decode(envelope.ciphertext).expect("b64").len()
        };
        assert_eq!(ct_len(&small), MIN_BUCKET + TAG_LEN);
        let mut replica = Replica::new(1);
        for i in 0..400 {
            replica.write(host_key(&format!("host-{i}")), host("10.0.0.1"));
        }
        bigger.state = replica.state;
        bigger.clock = replica.clock.counter();
        let large = seal_snapshot(&keys(), &binding(1), &bigger)
            .expect("seal")
            .text;
        let len = ct_len(&large) - TAG_LEN;
        assert!(len.is_power_of_two() && len > MIN_BUCKET, "{len}");
        open_mine(large.as_bytes()).expect("open");
    }

    #[test]
    fn size_caps() {
        let huge = vec![b'#'; MAX_CONTAINER_FILE_BYTES + 1];
        assert!(matches!(open_mine(&huge), Err(SyncError::TooLarge { .. })));
        let mut too_much = content();
        too_much.device_name = DeviceName::parse("x").expect("name");
        let mut replica = Replica::new(1);
        let notes = "n".repeat(2000);
        // ~5 MiB of notes: above the 4 MiB bucket ceiling.
        for i in 0..2600 {
            replica.write(
                RecordKey::HostMeta {
                    alias: format!("h{i}"),
                    field: super::super::model::MetaField::Notes,
                },
                RecordValue::Text(notes.clone()),
            );
        }
        too_much.state = replica.state;
        too_much.clock = replica.clock.counter();
        assert!(matches!(
            seal_snapshot(&keys(), &binding(1), &too_much),
            Err(SyncError::TooLarge { .. })
        ));
    }

    #[test]
    fn versions_and_garbage() {
        let text = seal_snapshot(&keys(), &binding(42), &content())
            .expect("seal")
            .text;
        assert!(matches!(
            open_mine(edit(&text, |e| e.version = 2).as_bytes()),
            Err(SyncError::UnsupportedVersion { found: 2, .. })
        ));
        assert!(matches!(
            open_mine(edit(&text, |e| e.format = "portico-vault".into()).as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        for garbage in [
            &b""[..],
            b"\xff\xfe",
            b"garbage = [",
            b"format = \"portico-sync-snapshot\"\nversion = 1\n",
        ] {
            assert!(matches!(open_mine(garbage), Err(SyncError::Corrupt { .. })));
        }
        let unknown = format!("{text}\nextra = 1\n");
        assert!(matches!(
            open_mine(unknown.as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        assert!(matches!(
            seal_snapshot(&keys(), &binding(0), &content()),
            Err(SyncError::Corrupt { .. })
        ));
        let mut bad_version = content();
        bad_version.app_version = "1.0\u{202e}".into();
        assert!(seal_snapshot(&keys(), &binding(1), &bad_version).is_err());
    }

    /// Seals an arbitrary JSON body with a valid AAD: what a device holding
    /// the key (or a bug) could produce.
    fn seal_raw(binding: &Binding, json: &str) -> String {
        let padded = crypto::padded(json.as_bytes(), MIN_BUCKET, "snapshot").expect("pad");
        let aad = aad(ContainerKind::Snapshot, binding, padded.len());
        let (nonce, ciphertext) =
            crypto::seal(keys().snapshot.bytes(), &aad, padded).expect("seal");
        let envelope = Envelope {
            format: ContainerKind::Snapshot.format().into(),
            version: CONTAINER_VERSION,
            device: binding.device,
            epoch: binding.epoch,
            keyset_generation: binding.keyset_generation,
            seq: binding.seq,
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ciphertext),
        };
        format!(
            "{FILE_COMMENT}{}",
            toml::to_string_pretty(&envelope).expect("render")
        )
    }

    #[test]
    fn authenticated_content_is_still_checked() {
        let body = |bundle: &str, seq: u64, format: u32, content: &str| {
            format!(
                r#"{{"format":{format},"bundle_id":"{bundle}","epoch":1,"keyset_generation":1,"device":"{}","seq":{seq},"content":{content}}}"#,
                me()
            )
        };
        let good_content = serde_json::to_string(&content()).expect("json");
        let bundle = B64.encode(BUNDLE);
        open_mine(seal_raw(&binding(5), &body(&bundle, 5, 1, &good_content)).as_bytes())
            .expect("valid");
        // Binding repeated inside must match.
        assert!(matches!(
            open_mine(
                seal_raw(
                    &binding(5),
                    &body(&B64.encode([9u8; ID_LEN]), 5, 1, &good_content)
                )
                .as_bytes()
            ),
            Err(SyncError::Corrupt { .. })
        ));
        assert!(matches!(
            open_mine(seal_raw(&binding(5), &body(&bundle, 6, 1, &good_content)).as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        assert!(matches!(
            open_mine(seal_raw(&binding(5), &body(&bundle, 5, 2, &good_content)).as_bytes()),
            Err(SyncError::UnsupportedVersion { found: 2, .. })
        ));
        // Invalid device name, context ahead of the clock, seen ahead.
        let mut bad = serde_json::to_value(content()).expect("value");
        bad["device_name"] = serde_json::Value::String("evil\u{202e}".into());
        assert!(matches!(
            open_mine(seal_raw(&binding(5), &body(&bundle, 5, 1, &bad.to_string())).as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        let mut bad = serde_json::to_value(content()).expect("value");
        bad["clock"] = serde_json::Value::from(0);
        assert!(matches!(
            open_mine(seal_raw(&binding(5), &body(&bundle, 5, 1, &bad.to_string())).as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        let mut bad = serde_json::to_value(content()).expect("value");
        bad["seen"][me().to_string()] = serde_json::Value::from(5);
        assert!(matches!(
            open_mine(seal_raw(&binding(5), &body(&bundle, 5, 1, &bad.to_string())).as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
        // Not JSON at all.
        assert!(matches!(
            open_mine(seal_raw(&binding(5), "not json").as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
    }

    #[test]
    fn replica_round_trips_and_is_bound_to_its_own_key() {
        let content = content();
        let replica = ReplicaContent {
            device_name: content.device_name.clone(),
            clock: content.clock,
            publish_seq: 9,
            state: content.state.clone(),
            high_water: HighWater::default(),
            local: LocalReplica::default(),
        };
        let text = seal_replica(&keys(), &binding(77), &replica)
            .expect("seal")
            .text;
        let opened =
            open_replica(&keys(), &expected(BUNDLE, 1, me()), text.as_bytes()).expect("open");
        assert_eq!(opened.content, replica);
        assert_eq!(opened.binding.seq, 77);
        // The snapshot key or format cannot open it.
        assert!(open_mine(text.as_bytes()).is_err());
        let as_snapshot = edit(&text, |e| {
            e.format = ContainerKind::Snapshot.format().into();
        });
        assert!(matches!(
            open_mine(as_snapshot.as_bytes()),
            Err(SyncError::Unauthenticated { .. })
        ));
        // Local generation is bound.
        let older = edit(&text, |e| e.seq = 76);
        assert!(matches!(
            open_replica(&keys(), &expected(BUNDLE, 1, me()), older.as_bytes()),
            Err(SyncError::Unauthenticated { .. })
        ));
    }

    #[test]
    fn a_container_above_the_bundle_keyset_generation_is_refused() {
        let mut newer = binding(5);
        newer.keyset_generation = 2;
        let text = seal_snapshot(&keys(), &newer, &content())
            .expect("seal")
            .text;
        // The bundle this device verified is still at generation 1.
        assert_eq!(
            open_mine(text.as_bytes()).expect_err("above"),
            SyncError::Corrupt {
                what: "snapshot",
                reason: "sealed under a newer key set than the bundle",
            }
        );
        let mut current = expected(BUNDLE, 1, me());
        current.max_keyset_generation = 2;
        let opened = open_snapshot(&keys(), &current, text.as_bytes()).expect("open");
        assert_eq!(opened.binding.keyset_generation, 2);
        // An older generation stays readable (devices re-seal lazily).
        current.max_keyset_generation = 3;
        open_snapshot(&keys(), &current, text.as_bytes()).expect("older generation");
        // Same for the replica.
        let replica = ReplicaContent {
            device_name: content().device_name,
            clock: content().clock,
            publish_seq: 1,
            state: content().state,
            high_water: HighWater::default(),
            local: LocalReplica::default(),
        };
        let text = seal_replica(&keys(), &newer, &replica).expect("seal").text;
        assert!(matches!(
            open_replica(&keys(), &expected(BUNDLE, 1, me()), text.as_bytes()),
            Err(SyncError::Corrupt { .. })
        ));
    }
}
