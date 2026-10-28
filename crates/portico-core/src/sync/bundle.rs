//! `portico-sync/bundle.toml`: KDF parameters, key slots, sealed roster,
//! header MAC (S1a), and since S2a the epoch commitment, the chained key
//! lineage and the roster v2 (`docs/SYNC.md` §2–3, §13.2).
//!
//! ```toml
//! format = "portico-sync"
//! version = 2               # 1: S1 layout, still read and rewritten as is
//! bundle_id = "base64(16)"
//! epoch = 1                 # data key generation (+1 at every rotation)
//! keyset_generation = 1     # +1 at every key slot change (passphrase change/reset/rotation)
//! revision = 1              # +1 at every write of this file
//! commit = "base64(32)"     # v2: C_E, the commitment of this epoch
//! mac = "base64(32)"        # HMAC-SHA256 under the header subkey
//! [kdf]                     # argon2id v19: memory_kib, iterations, parallelism, salt
//! [[keyslot]]               # kind = "passphrase": id, nonce, wrapped
//! [roster]                  # nonce, ciphertext (sealed device list)
//! [[lineage]]               # v2: 0–16 links {epoch, digest, proof}, contiguous, last = epoch − 1
//! [lineage_keys]            # v2: nonce, ciphertext ([(e, K_rot_e)] sealed under this epoch)
//! ```
//!
//! - **Open** checks, in this order: size cap, format and version, strict
//!   parsing (`deny_unknown_fields`), field sizes, lineage shape, KDF
//!   bounds (**before** any derivation), slot unwrap, header MAC (constant
//!   time, over the **version read**, R13), roster, lineage keys and the
//!   whole chain (every link's digest is `C_e` under its `K_rot_e`, every
//!   proof verifies, `commit` is `C_E` recomputed from the data key).
//! - **Wrong passphrase vs tampering**: a slot that does not open gives
//!   [`SyncError::WrongPassphrase`], whether the passphrase is wrong or any
//!   slot-bound field (bundle id, epoch, keyset generation, slot kind and
//!   id, KDF parameters, salt, nonce, wrapped key) was altered: no oracle.
//!   A slot that opens with a MAC, roster or lineage that does not verify
//!   gives [`SyncError::BundleTampered`]. The functions are pure: nothing
//!   is written, whatever the outcome.
//! - **Key slot AAD** binds the keyset generation (security review F4), so
//!   an older slot cannot be spliced into a newer header.
//! - **Change of passphrase** re-wraps the same data key under a new salt
//!   and slot id (keyset generation + 1). It does **not** protect against
//!   an already-leaked passphrase: an older copy of this file (provider
//!   version history) still opens with the old passphrase and yields the
//!   same data key (F2, documented and tested). Only a [`rotate`] does.
//! - **Rotation** ([`rotate`]): epoch + 1, a fresh data key, a mandatory
//!   new passphrase, and a lineage link proved under `K_rot` of the old
//!   epoch, so only a holder of the old data key can start the new epoch
//!   (R1, RR1). The 15 newest links of the old file are carried over.
//! - **Version 2** starts at the first S2 operation ([`to_v2`], [`rotate`]);
//!   [`with_roster`] and [`rewrap`] keep the version they read (a 0.7
//!   device rewrites a v1 bundle as v1). A 0.6 device reads a v2 file as
//!   `sync_version` before any derivation: it fails safe.
//! - Device slots (`kind = "device"`, S3) are parsed, covered by the MAC
//!   and kept verbatim; S1/S2 never use them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::SyncError;
use super::clock::{DeviceId, DeviceName};
use super::crypto::{
    self, Canonical, DataKey, ID_LEN, KEY_LEN, KdfBounds, KdfParams, KeyBytes, MAC_LEN, NONCE_LEN,
    SALT_LEN, Subkey, Subkeys, TAG_LEN,
};
use super::passphrase::{self, NewPassphrase};

/// Value of the `format` field.
pub(crate) const BUNDLE_FORMAT: &str = "portico-sync";
/// S1 layout: no commitment nor lineage, roster v1.
pub(crate) const BUNDLE_V1: u32 = 1;
/// S2 layout: commitment, chained lineage, sealed lineage keys, roster v2.
pub(crate) const BUNDLE_V2: u32 = 2;
/// Largest v1 file (the S1 cap: a 0.6 device refuses more before parsing).
pub(crate) const MAX_BUNDLE_V1_BYTES: usize = 64 * 1024;
/// Largest file. A v2 roster may list up to [`MAX_REVOKED_DEVICES`]
/// forgotten devices, which does not fit the S1 cap (deviation recorded
/// in `docs/SYNC.md` §13, "Livraison S2a").
pub(crate) const MAX_BUNDLE_BYTES: usize = 512 * 1024;
/// Most key slots (one passphrase slot + S3 device slots).
const MAX_KEYSLOTS: usize = 32;
/// Most active devices in the roster.
pub(crate) const MAX_ROSTER_DEVICES: usize = 64;
/// Most forgotten devices kept in a v2 roster (never pruned, R4).
pub(crate) const MAX_REVOKED_DEVICES: usize = 1024;
/// Most links of the lineage (a device more rotations behind re-joins).
pub(crate) const MAX_LINEAGE_LINKS: usize = 16;
/// Smallest padding bucket of the sealed roster and lineage keys.
const ROSTER_MIN_BUCKET: usize = 1024;

const KDF_ALGORITHM: &str = "argon2id";
const ARGON2_VERSION: u32 = 0x13;
const SLOT_PASSPHRASE: &str = "passphrase";
const SLOT_DEVICE: &str = "device";
const WHAT: &str = "bundle.toml";

const FILE_COMMENT: &str = "# Portico encrypted sync bundle. Do not edit: any change makes it unreadable.\n\
# See docs/SYNC.md in the Portico repository.\n\n";

/// One device of the sealed roster. In a v2 roster every listed device
/// is active (`revoked` is always `false`, forgotten devices are in
/// [`Roster::revoked`]); a v1 roster may carry the S1 flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RosterDevice {
    pub(crate) id: DeviceId,
    pub(crate) name: DeviceName,
    /// Unix seconds, display only.
    pub(crate) added_at: i64,
    #[serde(default)]
    pub(crate) revoked: bool,
}

/// A forgotten device (roster v2, §13.4): never pruned, immutable once
/// observed. Its writes up to `revoked_clock` stay; any later one is
/// refused everywhere with the same exact bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokedDevice {
    pub(crate) id: DeviceId,
    /// Unix seconds, display only.
    pub(crate) revoked_at: i64,
    /// The device that forgot it (itself when it left the list).
    pub(crate) revoked_by: DeviceId,
    /// The exact bound of its accepted writes.
    pub(crate) revoked_clock: u64,
}

/// The bundle's password policy (roster v2, §13.5.3). Off at creation;
/// every change increments `generation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
    pub(crate) include_passwords: bool,
    pub(crate) generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) changed_by: Option<DeviceId>,
    /// Unix seconds, display only.
    #[serde(default)]
    pub(crate) changed_at: i64,
}

/// The device list, sealed inside `bundle.toml` (names never in clear).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Roster {
    pub(crate) devices: Vec<RosterDevice>,
    /// v2: forgotten devices (empty in a v1 bundle).
    pub(crate) revoked: Vec<RevokedDevice>,
    /// v2: password policy (the default in a v1 bundle).
    pub(crate) policy: Policy,
}

impl Roster {
    /// A roster of `devices` only (v1 content).
    pub(crate) fn of(devices: Vec<RosterDevice>) -> Self {
        Self {
            devices,
            ..Self::default()
        }
    }

    /// The forgotten entry of `device`.
    pub(crate) fn revoked_entry(&self, device: &DeviceId) -> Option<&RevokedDevice> {
        self.revoked.iter().find(|entry| entry.id == *device)
    }
}

/// Wire form of a v1 roster: exactly the S1 encoding.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterV1 {
    devices: Vec<RosterDevice>,
}

/// Wire form of an active device of a v2 roster.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceV2 {
    id: DeviceId,
    name: DeviceName,
    added_at: i64,
}

/// Wire form of a v2 roster.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterV2 {
    devices: Vec<DeviceV2>,
    revoked: Vec<RevokedDevice>,
    policy: Policy,
}

/// One link of the lineage: epoch `epoch` (commitment `digest`) started
/// the next epoch, proved under its `K_rot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineageLink {
    pub(crate) epoch: u32,
    pub(crate) digest: [u8; MAC_LEN],
    pub(crate) proof: [u8; MAC_LEN],
}

/// Wire form of one sealed lineage key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineageKeyWire {
    epoch: u32,
    key: KeyBytes,
}

/// Verified, non-secret header of an opened bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleHeader {
    /// The format version **read** (or written): MAC, proofs and the
    /// ratchet use it (R13).
    pub(crate) version: u32,
    pub(crate) bundle_id: [u8; ID_LEN],
    pub(crate) epoch: u32,
    pub(crate) keyset_generation: u64,
    pub(crate) revision: u64,
    pub(crate) kdf: KdfParams,
    pub(crate) salt: [u8; SALT_LEN],
}

impl BundleHeader {
    /// `(epoch, keyset generation, revision)`: the order of bundles.
    pub(crate) fn rank(&self) -> (u32, u64, u64) {
        (self.epoch, self.keyset_generation, self.revision)
    }
}

/// The clear, **unauthenticated** header of a file (judged before any
/// key is available, §13.3.2): nothing here is trusted beyond the shape
/// checks of the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClearHeader {
    pub(crate) version: u32,
    pub(crate) bundle_id: [u8; ID_LEN],
    pub(crate) epoch: u32,
    pub(crate) keyset_generation: u64,
    pub(crate) revision: u64,
    /// v2 only.
    pub(crate) commit: Option<[u8; MAC_LEN]>,
    pub(crate) links: Vec<LineageLink>,
    /// The passphrase slot id (history of slots, RR1).
    pub(crate) slot_id: [u8; ID_LEN],
}

impl ClearHeader {
    pub(crate) fn rank(&self) -> (u32, u64, u64) {
        (self.epoch, self.keyset_generation, self.revision)
    }

    /// The link of epoch `epoch`, with the commitment it proves (the next
    /// link's digest, or `commit` for the last one).
    pub(crate) fn link_at(&self, epoch: u32) -> Option<(&LineageLink, [u8; MAC_LEN])> {
        let index = self.links.iter().position(|link| link.epoch == epoch)?;
        let link = self.links.get(index)?;
        let next = match self.links.get(index.saturating_add(1)) {
            Some(next) => next.digest,
            None => self.commit?,
        };
        Some((link, next))
    }
}

/// A key slot as stored (decoded). `device`/`enc` are S3 fields.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    kind: String,
    id: [u8; ID_LEN],
    device: Option<String>,
    enc: Option<String>,
    nonce: [u8; NONCE_LEN],
    wrapped: Vec<u8>,
}

/// A bundle whose slot opened and whose MAC, roster and lineage verified.
#[derive(Debug)]
pub(crate) struct OpenedBundle {
    pub(crate) header: BundleHeader,
    pub(crate) roster: Roster,
    pub(crate) data_key: DataKey,
    pub(crate) keys: Subkeys,
    /// `C_E` of this epoch, recomputed from the data key (v1 included, E1).
    pub(crate) commit: [u8; MAC_LEN],
    /// The lineage (empty in a v1 bundle, and at epoch 1).
    pub(crate) links: Vec<LineageLink>,
    /// `K_rot` of every link, in link order (v2).
    pub(crate) lineage_keys: Vec<(u32, KeyBytes)>,
    /// Fingerprint of the authenticated content (bundle high-water mark,
    /// F4): SHA-256 of the MAC input and the MAC, so a comment, whitespace,
    /// key order or CRLF added to the file does not change it (S1a review
    /// finding 1).
    pub(crate) fingerprint: [u8; 32],
    slots: Vec<Slot>,
}

impl OpenedBundle {
    /// The passphrase slot id (history of verified slots, RR1).
    pub(crate) fn slot_id(&self) -> Option<[u8; ID_LEN]> {
        self.slots
            .iter()
            .find(|slot| slot.kind == SLOT_PASSPHRASE)
            .map(|slot| slot.id)
    }

    /// `K_rot` of an epoch of the lineage (or of this epoch).
    pub(crate) fn rotate_key(&self, epoch: u32) -> Option<Subkey> {
        if epoch == self.header.epoch {
            return Some(self.keys.rotate.duplicate());
        }
        self.lineage_keys
            .iter()
            .find(|(e, _)| *e == epoch)
            .map(|(_, key)| key.subkey())
    }

    /// A copy (the caller keeps the original): same keys, same content.
    #[cfg(test)]
    pub(crate) fn duplicate(&self) -> Result<Self, SyncError> {
        let data_key = DataKey::from_bytes(self.data_key.bytes())?;
        let keys = crypto::derive_subkeys(&data_key, &self.header.bundle_id, self.header.epoch)?;
        Ok(Self {
            header: self.header.clone(),
            roster: self.roster.clone(),
            data_key,
            keys,
            commit: self.commit,
            links: self.links.clone(),
            lineage_keys: self.lineage_keys.clone(),
            fingerprint: self.fingerprint,
            slots: self.slots.clone(),
        })
    }
}

// ── File layout ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VersionProbe {
    format: Option<String>,
    version: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    format: String,
    version: u32,
    bundle_id: String,
    epoch: u32,
    keyset_generation: u64,
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    mac: String,
    kdf: KdfSection,
    keyslot: Vec<KeyslotSection>,
    roster: RosterSection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    lineage: Vec<LineageSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lineage_keys: Option<RosterSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KdfSection {
    algorithm: String,
    version: u32,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
    salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyslotSection {
    kind: String,
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enc: Option<String>,
    nonce: String,
    wrapped: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterSection {
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineageSection {
    epoch: u32,
    digest: String,
    proof: String,
}

fn corrupt(reason: &'static str) -> SyncError {
    SyncError::Corrupt { what: WHAT, reason }
}

fn decode(text: &str, reason: &'static str) -> Result<Vec<u8>, SyncError> {
    // Bounded by the file cap; base64 never expands.
    B64.decode(text).map_err(|_| corrupt(reason))
}

fn decode_exact<const N: usize>(text: &str, reason: &'static str) -> Result<[u8; N], SyncError> {
    crypto::exact(&decode(text, reason)?, WHAT, reason)
}

/// The v2 part of a parsed file.
struct ParsedLineage {
    commit: [u8; MAC_LEN],
    links: Vec<LineageLink>,
    keys_nonce: [u8; NONCE_LEN],
    keys_ciphertext: Vec<u8>,
}

/// The decoded, unverified content of the file.
struct Parsed {
    header: BundleHeader,
    slots: Vec<Slot>,
    roster_nonce: [u8; NONCE_LEN],
    roster_ciphertext: Vec<u8>,
    lineage: Option<ParsedLineage>,
    mac: [u8; MAC_LEN],
}

/// Shape of a lineage (§13.3.2): at most [`MAX_LINEAGE_LINKS`] links of
/// contiguous, increasing epochs, the last one `epoch − 1`; no link at
/// the first epoch. Anything else was not written by Portico: tampering
/// (there is no "unverifiable" case, R10).
fn check_lineage_shape(epoch: u32, links: &[LineageLink]) -> Result<(), SyncError> {
    if links.len() > MAX_LINEAGE_LINKS {
        return Err(SyncError::BundleTampered);
    }
    let contiguous = links
        .windows(2)
        .all(|pair| matches!(pair, [a, b] if a.epoch.checked_add(1) == Some(b.epoch)));
    if !contiguous {
        return Err(SyncError::BundleTampered);
    }
    match links.last() {
        None if epoch == 1 => Ok(()),
        Some(last) if last.epoch.checked_add(1) == Some(epoch) => Ok(()),
        _ => Err(SyncError::BundleTampered),
    }
}

// One sequence of bounded checks, in the order the module doc gives:
// splitting it would scatter that order.
#[allow(clippy::too_many_lines)]
fn parse(text: &str) -> Result<Parsed, SyncError> {
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(SyncError::TooLarge {
            what: WHAT,
            max: MAX_BUNDLE_BYTES,
        });
    }
    let probe: VersionProbe = toml::from_str(text).map_err(|_| corrupt("not valid TOML"))?;
    if probe.format.as_deref() != Some(BUNDLE_FORMAT) {
        return Err(corrupt("not a Portico sync bundle"));
    }
    let version = probe.version.ok_or_else(|| corrupt("no version"))?;
    if version != BUNDLE_V1 && version != BUNDLE_V2 {
        return Err(SyncError::UnsupportedVersion {
            what: WHAT,
            found: version,
            supported: BUNDLE_V2,
        });
    }
    if version == BUNDLE_V1 && text.len() > MAX_BUNDLE_V1_BYTES {
        return Err(SyncError::TooLarge {
            what: WHAT,
            max: MAX_BUNDLE_V1_BYTES,
        });
    }
    let file: BundleFile = toml::from_str(text).map_err(|_| corrupt("unexpected content"))?;
    if file.kdf.algorithm != KDF_ALGORITHM || file.kdf.version != ARGON2_VERSION {
        return Err(corrupt("unknown key derivation"));
    }
    if file.epoch == 0 || file.keyset_generation == 0 || file.revision == 0 {
        return Err(corrupt("zero epoch, generation or revision"));
    }
    if file.keyslot.is_empty() || file.keyslot.len() > MAX_KEYSLOTS {
        return Err(corrupt("bad number of key slots"));
    }
    let mut slots = Vec::with_capacity(file.keyslot.len());
    for section in &file.keyslot {
        if section.kind != SLOT_PASSPHRASE && section.kind != SLOT_DEVICE {
            return Err(corrupt("unknown key slot kind"));
        }
        let wrapped = decode(&section.wrapped, "bad wrapped key")?;
        if wrapped.len() != KEY_LEN + TAG_LEN {
            return Err(corrupt("bad wrapped key"));
        }
        slots.push(Slot {
            kind: section.kind.clone(),
            id: decode_exact(&section.id, "bad key slot id")?,
            device: section.device.clone(),
            enc: section.enc.clone(),
            nonce: decode_exact(&section.nonce, "bad key slot nonce")?,
            wrapped,
        });
    }
    if slots.iter().filter(|s| s.kind == SLOT_PASSPHRASE).count() != 1 {
        return Err(corrupt("exactly one passphrase slot expected"));
    }
    let roster_ciphertext = decode(&file.roster.ciphertext, "bad roster")?;
    if !crypto::is_bucket(
        roster_ciphertext.len().saturating_sub(TAG_LEN),
        ROSTER_MIN_BUCKET,
    ) {
        return Err(corrupt("bad roster"));
    }
    let lineage = match (version, file.commit.as_deref(), &file.lineage_keys) {
        (BUNDLE_V1, None, None) if file.lineage.is_empty() => None,
        (BUNDLE_V2, Some(commit), Some(keys)) => {
            let links = file
                .lineage
                .iter()
                .map(|link| {
                    Ok(LineageLink {
                        epoch: link.epoch,
                        digest: decode_exact(&link.digest, "bad lineage digest")?,
                        proof: decode_exact(&link.proof, "bad lineage proof")?,
                    })
                })
                .collect::<Result<Vec<_>, SyncError>>()?;
            check_lineage_shape(file.epoch, &links)?;
            let keys_ciphertext = decode(&keys.ciphertext, "bad lineage keys")?;
            if !crypto::is_bucket(
                keys_ciphertext.len().saturating_sub(TAG_LEN),
                ROSTER_MIN_BUCKET,
            ) {
                return Err(corrupt("bad lineage keys"));
            }
            Some(ParsedLineage {
                commit: decode_exact(commit, "bad commit")?,
                links,
                keys_nonce: decode_exact(&keys.nonce, "bad lineage keys nonce")?,
                keys_ciphertext,
            })
        }
        _ => return Err(corrupt("unexpected content")),
    };
    Ok(Parsed {
        header: BundleHeader {
            version,
            bundle_id: decode_exact(&file.bundle_id, "bad bundle id")?,
            epoch: file.epoch,
            keyset_generation: file.keyset_generation,
            revision: file.revision,
            kdf: KdfParams {
                memory_kib: file.kdf.memory_kib,
                iterations: file.kdf.iterations,
                parallelism: file.kdf.parallelism,
            },
            salt: decode_exact(&file.kdf.salt, "bad salt")?,
        },
        slots,
        roster_nonce: decode_exact(&file.roster.nonce, "bad roster nonce")?,
        roster_ciphertext,
        lineage,
        mac: decode_exact(&file.mac, "bad mac")?,
    })
}

/// The clear header of `text` (unauthenticated): what a device judges
/// before it can open the file (§13.3.2, §13.3.4).
pub(crate) fn peek(text: &str) -> Result<ClearHeader, SyncError> {
    let parsed = parse(text)?;
    let slot_id = parsed
        .slots
        .iter()
        .find(|s| s.kind == SLOT_PASSPHRASE)
        .map(|s| s.id)
        .ok_or_else(|| corrupt("exactly one passphrase slot expected"))?;
    let (commit, links) = match parsed.lineage {
        Some(lineage) => (Some(lineage.commit), lineage.links),
        None => (None, Vec::new()),
    };
    Ok(ClearHeader {
        version: parsed.header.version,
        bundle_id: parsed.header.bundle_id,
        epoch: parsed.header.epoch,
        keyset_generation: parsed.header.keyset_generation,
        revision: parsed.header.revision,
        commit,
        links,
        slot_id,
    })
}

// ── Canonical inputs ────────────────────────────────────────────────────

fn kdf_fields(canonical: &mut Canonical, header: &BundleHeader) {
    canonical
        .str(KDF_ALGORITHM)
        .u32(ARGON2_VERSION)
        .u32(header.kdf.memory_kib)
        .u32(header.kdf.iterations)
        .u32(header.kdf.parallelism)
        .bytes(&header.salt);
}

fn slot_aad(header: &BundleHeader, kind: &str, slot_id: &[u8; ID_LEN]) -> Vec<u8> {
    let mut aad = Canonical::new("keyslot");
    aad.bytes(&header.bundle_id)
        .u32(header.epoch)
        .u64(header.keyset_generation)
        .str(kind)
        .bytes(slot_id);
    kdf_fields(&mut aad, header);
    aad.finish().to_vec()
}

fn roster_aad(header: &BundleHeader) -> Vec<u8> {
    let mut aad = Canonical::new("roster");
    aad.bytes(&header.bundle_id)
        .u32(header.epoch)
        .u64(header.keyset_generation)
        .u64(header.revision);
    aad.finish().to_vec()
}

fn lineage_keys_aad(header: &BundleHeader) -> Vec<u8> {
    let mut aad = Canonical::new("lineage-keys");
    aad.bytes(&header.bundle_id)
        .u32(header.epoch)
        .u64(header.keyset_generation)
        .u64(header.revision);
    aad.finish().to_vec()
}

/// The v2 fields the MAC covers.
struct MacLineage<'a> {
    commit: &'a [u8; MAC_LEN],
    links: &'a [LineageLink],
    keys_nonce: &'a [u8; NONCE_LEN],
    keys_ciphertext: &'a [u8],
}

/// The MAC input over the **version read** (R13): a v1 file keeps exactly
/// the S1 encoding; a v2 file adds its commitment, links and sealed
/// lineage keys.
fn mac_input(
    header: &BundleHeader,
    slots: &[Slot],
    roster_nonce: &[u8; NONCE_LEN],
    roster_ciphertext: &[u8],
    lineage: Option<&MacLineage<'_>>,
) -> Vec<u8> {
    let mut input = Canonical::new("bundle-mac");
    input
        .str(BUNDLE_FORMAT)
        .u32(header.version)
        .bytes(&header.bundle_id)
        .u32(header.epoch)
        .u64(header.keyset_generation)
        .u64(header.revision);
    kdf_fields(&mut input, header);
    input.u64(u64::try_from(slots.len()).unwrap_or(u64::MAX));
    for slot in slots {
        input
            .str(&slot.kind)
            .bytes(&slot.id)
            .str(slot.device.as_deref().unwrap_or(""))
            .u32(u32::from(slot.device.is_some()))
            .str(slot.enc.as_deref().unwrap_or(""))
            .u32(u32::from(slot.enc.is_some()))
            .bytes(&slot.nonce)
            .bytes(&slot.wrapped);
    }
    input.bytes(roster_nonce).bytes(roster_ciphertext);
    if let Some(lineage) = lineage {
        input
            .bytes(lineage.commit)
            .u64(u64::try_from(lineage.links.len()).unwrap_or(u64::MAX));
        for link in lineage.links {
            input.u32(link.epoch).bytes(&link.digest).bytes(&link.proof);
        }
        input
            .bytes(lineage.keys_nonce)
            .bytes(lineage.keys_ciphertext);
    }
    input.finish().to_vec()
}

// ── Operations ──────────────────────────────────────────────────────────

/// Unwraps the passphrase slot. Any failure is `WrongPassphrase`.
fn unwrap_slot(
    parsed: &Parsed,
    passphrase: &SecretString,
    bounds: &KdfBounds,
) -> Result<DataKey, SyncError> {
    let slot = parsed
        .slots
        .iter()
        .find(|s| s.kind == SLOT_PASSPHRASE)
        .ok_or_else(|| corrupt("exactly one passphrase slot expected"))?;
    let kek = crypto::derive_kek(
        passphrase,
        &parsed.header.kdf,
        &parsed.header.salt,
        &parsed.header.bundle_id,
        bounds,
    )?;
    let aad = slot_aad(&parsed.header, SLOT_PASSPHRASE, &slot.id);
    let key = crypto::open(kek.bytes(), &slot.nonce, &aad, &slot.wrapped)
        .map_err(|_| SyncError::WrongPassphrase)?;
    DataKey::from_bytes(&key)
}

/// Fingerprint of a bundle: its canonical MAC input (every field of the
/// file, decoded) and its MAC.
fn fingerprint(mac_input: &[u8], mac: &[u8; MAC_LEN]) -> [u8; 32] {
    crypto::content_fingerprint("portico-sync/1 bundle", &[mac_input, mac])
}

/// Checks the whole chain of an opened file: one key per link (same
/// epochs, same order), each link's digest is `C_e` under its `K_rot_e`,
/// each proof verifies towards the next commitment, and `commit` is the
/// `C_E` of the data key. Any failure is tampering: a file that copies
/// authentic links and adds its own cannot prove links it holds no key
/// for (RR1).
fn check_chain(
    bundle_id: &[u8; ID_LEN],
    commit: &[u8; MAC_LEN],
    links: &[LineageLink],
    keys: &[(u32, KeyBytes)],
) -> Result<(), SyncError> {
    if links.len() != keys.len() {
        return Err(SyncError::BundleTampered);
    }
    for (index, (link, (key_epoch, key))) in links.iter().zip(keys).enumerate() {
        if link.epoch != *key_epoch {
            return Err(SyncError::BundleTampered);
        }
        let rotate = key.subkey();
        if crypto::epoch_commitment(&rotate, bundle_id, link.epoch)? != link.digest {
            return Err(SyncError::BundleTampered);
        }
        let next = match links.get(index.saturating_add(1)) {
            Some(next) => &next.digest,
            None => commit,
        };
        if !crypto::verify_lineage_proof(
            &rotate,
            bundle_id,
            link.epoch,
            &link.digest,
            next,
            &link.proof,
        )? {
            return Err(SyncError::BundleTampered);
        }
    }
    Ok(())
}

/// Verifies the MAC and opens the roster (and, in v2, the lineage keys
/// and the chain) with an unwrapped data key.
fn verify(parsed: Parsed, data_key: DataKey) -> Result<OpenedBundle, SyncError> {
    let header = parsed.header;
    let keys = crypto::derive_subkeys(&data_key, &header.bundle_id, header.epoch)?;
    let mac_lineage = parsed.lineage.as_ref().map(|lineage| MacLineage {
        commit: &lineage.commit,
        links: &lineage.links,
        keys_nonce: &lineage.keys_nonce,
        keys_ciphertext: &lineage.keys_ciphertext,
    });
    let input = mac_input(
        &header,
        &parsed.slots,
        &parsed.roster_nonce,
        &parsed.roster_ciphertext,
        mac_lineage.as_ref(),
    );
    if !crypto::verify_mac(&keys.header, &input, &parsed.mac)? {
        return Err(SyncError::BundleTampered);
    }
    let roster_plain = crypto::open(
        keys.roster.bytes(),
        &parsed.roster_nonce,
        &roster_aad(&header),
        &parsed.roster_ciphertext,
    )
    .map_err(|_| SyncError::BundleTampered)?;
    let roster = decode_roster(header.version, &roster_plain)?;
    check_roster(header.version, &roster)?;
    let commit = crypto::epoch_commitment(&keys.rotate, &header.bundle_id, header.epoch)?;
    let (links, lineage_keys) = match parsed.lineage {
        None => (Vec::new(), Vec::new()),
        Some(lineage) => {
            let plain = crypto::open(
                keys.roster.bytes(),
                &lineage.keys_nonce,
                &lineage_keys_aad(&header),
                &lineage.keys_ciphertext,
            )
            .map_err(|_| SyncError::BundleTampered)?;
            let wire: Vec<LineageKeyWire> =
                serde_json::from_slice(&plain).map_err(|_| corrupt("bad lineage keys content"))?;
            let lineage_keys: Vec<(u32, KeyBytes)> =
                wire.into_iter().map(|key| (key.epoch, key.key)).collect();
            if lineage.commit != commit {
                return Err(SyncError::BundleTampered);
            }
            check_chain(&header.bundle_id, &commit, &lineage.links, &lineage_keys)?;
            (lineage.links, lineage_keys)
        }
    };
    Ok(OpenedBundle {
        header,
        roster,
        data_key,
        keys,
        commit,
        links,
        lineage_keys,
        fingerprint: fingerprint(&input, &parsed.mac),
        slots: parsed.slots,
    })
}

fn decode_roster(version: u32, plain: &[u8]) -> Result<Roster, SyncError> {
    if version == BUNDLE_V1 {
        let roster: RosterV1 =
            serde_json::from_slice(plain).map_err(|_| corrupt("bad roster content"))?;
        return Ok(Roster::of(roster.devices));
    }
    let roster: RosterV2 =
        serde_json::from_slice(plain).map_err(|_| corrupt("bad roster content"))?;
    Ok(Roster {
        devices: roster
            .devices
            .into_iter()
            .map(|device| RosterDevice {
                id: device.id,
                name: device.name,
                added_at: device.added_at,
                revoked: false,
            })
            .collect(),
        revoked: roster.revoked,
        policy: roster.policy,
    })
}

fn check_roster(version: u32, roster: &Roster) -> Result<(), SyncError> {
    if roster.devices.len() > MAX_ROSTER_DEVICES {
        return Err(SyncError::TooLarge {
            what: "sync roster",
            max: MAX_ROSTER_DEVICES,
        });
    }
    if roster.revoked.len() > MAX_REVOKED_DEVICES {
        return Err(SyncError::RosterFull {
            max: MAX_REVOKED_DEVICES,
        });
    }
    let mut ids: Vec<&DeviceId> = roster.devices.iter().map(|d| &d.id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| matches!(w, [a, b] if a == b)) {
        return Err(corrupt("duplicate device in the roster"));
    }
    let mut revoked: Vec<&DeviceId> = roster.revoked.iter().map(|d| &d.id).collect();
    revoked.sort_unstable();
    if revoked.windows(2).any(|w| matches!(w, [a, b] if a == b)) {
        return Err(corrupt("duplicate forgotten device in the roster"));
    }
    if roster
        .devices
        .iter()
        .any(|d| revoked.binary_search(&&d.id).is_ok())
    {
        return Err(corrupt("a device is both active and forgotten"));
    }
    if version == BUNDLE_V1 {
        if !roster.revoked.is_empty() || roster.policy != Policy::default() {
            return Err(SyncError::Crypto(
                "a version 1 bundle has no S2 roster fields",
            ));
        }
    } else if roster.devices.iter().any(|d| d.revoked) {
        return Err(corrupt("a version 2 roster lists forgotten devices apart"));
    }
    Ok(())
}

/// Opens `bundle.toml` with the passphrase.
pub(crate) fn open(
    text: &str,
    passphrase: &SecretString,
    bounds: &KdfBounds,
) -> Result<OpenedBundle, SyncError> {
    let parsed = parse(text)?;
    let data_key = unwrap_slot(&parsed, passphrase, bounds)?;
    verify(parsed, data_key)
}

/// Opens `bundle.toml` with a remembered data key (every poll): a key of
/// another bundle or epoch fails the MAC ([`SyncError::BundleTampered`]).
pub(crate) fn open_with_key(text: &str, data_key: &DataKey) -> Result<OpenedBundle, SyncError> {
    let parsed = parse(text)?;
    verify(parsed, DataKey::from_bytes(data_key.bytes())?)
}

/// Whether `passphrase` opens the passphrase slot of `text` (the history
/// of verified slots, RR1): one derivation, nothing else verified. A file
/// that does not parse, or whose KDF is out of bounds, opens nothing.
pub(crate) fn passphrase_opens(text: &str, passphrase: &SecretString, bounds: &KdfBounds) -> bool {
    parse(text).is_ok_and(|parsed| unwrap_slot(&parsed, passphrase, bounds).is_ok())
}

/// What [`render`] writes for a v2 file.
struct LineageOut {
    commit: [u8; MAC_LEN],
    links: Vec<LineageLink>,
    keys: Vec<(u32, KeyBytes)>,
}

/// Renders a bundle; seals the roster (and the lineage keys) and computes
/// the MAC over the version of `header`.
fn render(
    header: BundleHeader,
    slots: Vec<Slot>,
    roster: Roster,
    data_key: DataKey,
    lineage: Option<LineageOut>,
) -> Result<(String, OpenedBundle), SyncError> {
    render_inner(header, slots, roster, data_key, lineage, true)
}

/// [`render`]; `checked = false` (tests only) writes whatever lineage it
/// is given, as a key holder forging a file would.
// One file layout, field by field: splitting it would scatter the MAC
// input from the fields it covers.
#[allow(clippy::too_many_lines)]
fn render_inner(
    header: BundleHeader,
    slots: Vec<Slot>,
    roster: Roster,
    data_key: DataKey,
    lineage: Option<LineageOut>,
    checked: bool,
) -> Result<(String, OpenedBundle), SyncError> {
    check_roster(header.version, &roster)?;
    let keys = crypto::derive_subkeys(&data_key, &header.bundle_id, header.epoch)?;
    let commit = crypto::epoch_commitment(&keys.rotate, &header.bundle_id, header.epoch)?;
    // Written straight into its padded buffer (finding 6).
    let padded = if header.version == BUNDLE_V1 {
        crypto::padded_json(
            &RosterV1 {
                devices: roster.devices.clone(),
            },
            ROSTER_MIN_BUCKET,
            "sync roster",
        )?
    } else {
        crypto::padded_json(
            &RosterV2 {
                devices: roster
                    .devices
                    .iter()
                    .map(|d| DeviceV2 {
                        id: d.id,
                        name: d.name.clone(),
                        added_at: d.added_at,
                    })
                    .collect(),
                revoked: roster.revoked.clone(),
                policy: roster.policy.clone(),
            },
            ROSTER_MIN_BUCKET,
            "sync roster",
        )?
    };
    let (roster_nonce, roster_ciphertext) =
        crypto::seal(keys.roster.bytes(), &roster_aad(&header), padded)?;
    // A forged file (tests) writes the commitment it was given.
    let mut file_commit = commit;
    let (lineage_section, links, lineage_keys) = match (header.version, lineage) {
        (BUNDLE_V1, None) => (None, Vec::new(), Vec::new()),
        (BUNDLE_V2, Some(lineage)) => {
            file_commit = lineage.commit;
            if checked {
                if lineage.commit != commit {
                    return Err(SyncError::Crypto("the commitment does not match the key"));
                }
                check_lineage_shape(header.epoch, &lineage.links)?;
                check_chain(&header.bundle_id, &commit, &lineage.links, &lineage.keys)?;
            }
            let wire: Vec<LineageKeyWire> = lineage
                .keys
                .iter()
                .map(|(epoch, key)| LineageKeyWire {
                    epoch: *epoch,
                    key: key.clone(),
                })
                .collect();
            let padded = crypto::padded_json(&wire, ROSTER_MIN_BUCKET, "sync lineage keys")?;
            let (nonce, ciphertext) =
                crypto::seal(keys.roster.bytes(), &lineage_keys_aad(&header), padded)?;
            (Some((nonce, ciphertext)), lineage.links, lineage.keys)
        }
        _ => return Err(SyncError::Crypto("lineage and version disagree")),
    };
    let mac_lineage = lineage_section
        .as_ref()
        .map(|(nonce, ciphertext)| MacLineage {
            commit: &file_commit,
            links: &links,
            keys_nonce: nonce,
            keys_ciphertext: ciphertext,
        });
    let input = mac_input(
        &header,
        &slots,
        &roster_nonce,
        &roster_ciphertext,
        mac_lineage.as_ref(),
    );
    let mac = crypto::mac(&keys.header, &input)?;
    let file = BundleFile {
        format: BUNDLE_FORMAT.to_owned(),
        version: header.version,
        bundle_id: B64.encode(header.bundle_id),
        epoch: header.epoch,
        keyset_generation: header.keyset_generation,
        revision: header.revision,
        commit: lineage_section.as_ref().map(|_| B64.encode(file_commit)),
        mac: B64.encode(mac),
        kdf: KdfSection {
            algorithm: KDF_ALGORITHM.to_owned(),
            version: ARGON2_VERSION,
            memory_kib: header.kdf.memory_kib,
            iterations: header.kdf.iterations,
            parallelism: header.kdf.parallelism,
            salt: B64.encode(header.salt),
        },
        keyslot: slots
            .iter()
            .map(|slot| KeyslotSection {
                kind: slot.kind.clone(),
                id: B64.encode(slot.id),
                device: slot.device.clone(),
                enc: slot.enc.clone(),
                nonce: B64.encode(slot.nonce),
                wrapped: B64.encode(&slot.wrapped),
            })
            .collect(),
        roster: RosterSection {
            nonce: B64.encode(roster_nonce),
            ciphertext: B64.encode(&roster_ciphertext),
        },
        lineage: links
            .iter()
            .map(|link| LineageSection {
                epoch: link.epoch,
                digest: B64.encode(link.digest),
                proof: B64.encode(link.proof),
            })
            .collect(),
        lineage_keys: lineage_section.map(|(nonce, ciphertext)| RosterSection {
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(&ciphertext),
        }),
    };
    let body =
        toml::to_string_pretty(&file).map_err(|_| SyncError::Crypto("bundle encoding failed"))?;
    let text = format!("{FILE_COMMENT}{body}");
    let cap = if header.version == BUNDLE_V1 {
        MAX_BUNDLE_V1_BYTES
    } else {
        MAX_BUNDLE_BYTES
    };
    if text.len() > cap {
        return Err(SyncError::TooLarge {
            what: WHAT,
            max: cap,
        });
    }
    let opened = OpenedBundle {
        header,
        roster,
        data_key,
        keys,
        commit,
        links,
        lineage_keys,
        fingerprint: fingerprint(&input, &mac),
        slots,
    };
    Ok((text, opened))
}

/// The lineage an opened bundle carries over unchanged (same epoch).
fn same_lineage(opened: &OpenedBundle) -> Option<LineageOut> {
    (opened.header.version == BUNDLE_V2).then(|| LineageOut {
        commit: opened.commit,
        links: opened.links.clone(),
        keys: opened.lineage_keys.clone(),
    })
}

/// A passphrase slot for `header` wrapping `data_key` (fresh slot id and
/// nonce).
fn passphrase_slot(
    header: &BundleHeader,
    passphrase: &NewPassphrase,
    data_key: &DataKey,
    bounds: &KdfBounds,
) -> Result<Slot, SyncError> {
    let kek = crypto::derive_kek(
        passphrase.secret(),
        &header.kdf,
        &header.salt,
        &header.bundle_id,
        bounds,
    )?;
    let id = crypto::random_array::<ID_LEN>()?;
    let aad = slot_aad(header, SLOT_PASSPHRASE, &id);
    let mut plain = Zeroizing::new(Vec::with_capacity(KEY_LEN + TAG_LEN));
    plain.extend_from_slice(data_key.bytes());
    let (nonce, wrapped) = crypto::seal(kek.bytes(), &aad, plain)?;
    Ok(Slot {
        kind: SLOT_PASSPHRASE.to_owned(),
        id,
        device: None,
        enc: None,
        nonce,
        wrapped,
    })
}

/// A new bundle (first device): random bundle id, salt and data key,
/// epoch 1, version 1 (a fleet of 0.6 devices keeps syncing until the
/// first S2 operation, §13.2). The passphrase must meet the minimum
/// strength (the diceware estimate only for a generated one, finding 5).
pub(crate) fn create(
    passphrase: &NewPassphrase,
    first_device: RosterDevice,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    passphrase::enforce(passphrase)?;
    bounds.check(&params)?;
    let header = BundleHeader {
        version: BUNDLE_V1,
        bundle_id: crypto::random_array()?,
        epoch: 1,
        keyset_generation: 1,
        revision: 1,
        kdf: params,
        salt: crypto::random_array()?,
    };
    let data_key = DataKey::generate()?;
    let slot = passphrase_slot(&header, passphrase, &data_key, bounds)?;
    render(
        header,
        vec![slot],
        Roster::of(vec![first_device]),
        data_key,
        None,
    )
}

fn next_counter(n: u64) -> Result<u64, SyncError> {
    n.checked_add(1).ok_or_else(|| corrupt("counter exhausted"))
}

/// New passphrase for an opened bundle: same data key, new salt and slot,
/// `params` (may raise the cost), keyset generation and revision + 1. Used
/// by "change" (after [`open`] with the old passphrase) and by "reset"
/// from a device that holds the data key (owner decision #8; the UI asks
/// for an extra confirmation and the other devices get a blocking alert).
/// The version and the lineage are kept (`C_E` does not depend on the
/// passphrase).
pub(crate) fn rewrap(
    opened: &OpenedBundle,
    new_passphrase: &NewPassphrase,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    passphrase::enforce(new_passphrase)?;
    bounds.check(&params)?;
    let header = BundleHeader {
        keyset_generation: next_counter(opened.header.keyset_generation)?,
        revision: next_counter(opened.header.revision)?,
        kdf: params,
        salt: crypto::random_array()?,
        ..opened.header.clone()
    };
    let data_key = DataKey::from_bytes(opened.data_key.bytes())?;
    let slot = passphrase_slot(&header, new_passphrase, &data_key, bounds)?;
    // S3 device slots bind the old generation in their AAD: dropped here,
    // re-created by their devices.
    render(
        header,
        vec![slot],
        opened.roster.clone(),
        data_key,
        same_lineage(opened),
    )
}

/// "Change passphrase": opens with the old one, then [`rewrap`].
pub(crate) fn change_passphrase(
    text: &str,
    old: &SecretString,
    new: &NewPassphrase,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    // Refuse a weak new passphrase before paying for the old derivation.
    passphrase::enforce(new)?;
    let opened = open(text, old, bounds)?;
    rewrap(&opened, new, params, bounds)
}

/// A new roster (device joined, renamed, left): revision + 1, key slots,
/// version and lineage kept verbatim. Needs only the data key. A v1
/// bundle stays v1 (it cannot hold S2 roster fields: [`to_v2`] first).
pub(crate) fn with_roster(
    opened: &OpenedBundle,
    roster: Roster,
) -> Result<(String, OpenedBundle), SyncError> {
    let header = BundleHeader {
        revision: next_counter(opened.header.revision)?,
        ..opened.header.clone()
    };
    render(
        header,
        opened.slots.clone(),
        roster,
        DataKey::from_bytes(opened.data_key.bytes())?,
        same_lineage(opened),
    )
}

/// The first S2 operation without a rotation ("leave the device list",
/// a policy change): the same epoch, keys and slots in version 2, the
/// next revision, with `roster`. v1 devices flagged `revoked` by S1
/// become forgotten entries. A v2 bundle is only re-rostered.
pub(crate) fn to_v2(
    opened: &OpenedBundle,
    roster: Roster,
) -> Result<(String, OpenedBundle), SyncError> {
    if opened.header.version == BUNDLE_V2 {
        return with_roster(opened, roster);
    }
    let header = BundleHeader {
        version: BUNDLE_V2,
        revision: next_counter(opened.header.revision)?,
        ..opened.header.clone()
    };
    let lineage = LineageOut {
        commit: opened.commit,
        links: opened.links.clone(),
        keys: opened.lineage_keys.clone(),
    };
    render(
        header,
        opened.slots.clone(),
        roster_v2(roster),
        DataKey::from_bytes(opened.data_key.bytes())?,
        Some(lineage),
    )
}

/// A roster in v2 shape: S1 `revoked` flags become forgotten entries.
pub(crate) fn roster_v2(mut roster: Roster) -> Roster {
    let (flagged, active): (Vec<RosterDevice>, Vec<RosterDevice>) =
        std::mem::take(&mut roster.devices)
            .into_iter()
            .partition(|d| d.revoked);
    for device in flagged {
        if roster.revoked_entry(&device.id).is_none() {
            roster.revoked.push(RevokedDevice {
                id: device.id,
                revoked_at: device.added_at,
                revoked_by: device.id,
                revoked_clock: 0,
            });
        }
    }
    roster.devices = active;
    roster.revoked.sort_by_key(|entry| entry.id);
    roster
}

/// A key rotation (S2a, §13.3): epoch + 1, a fresh random data key, a
/// mandatory new passphrase (strength enforced; the caller refuses one
/// that opens a known slot), a new salt, KDF parameters raised to at least
/// `params`, keyset generation and revision + 1, version 2, `roster`
/// (forgotten devices already moved by the caller), and the lineage: the
/// 15 newest links of `opened` plus the link of its epoch, proved under
/// its `K_rot` towards the new commitment. Pure: nothing is written.
pub(crate) fn rotate(
    opened: &OpenedBundle,
    new_passphrase: &NewPassphrase,
    roster: Roster,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    passphrase::enforce(new_passphrase)?;
    let raised = KdfParams {
        memory_kib: opened.header.kdf.memory_kib.max(params.memory_kib),
        iterations: opened.header.kdf.iterations.max(params.iterations),
        parallelism: opened.header.kdf.parallelism.max(params.parallelism),
    };
    bounds.check(&raised)?;
    let epoch = opened
        .header
        .epoch
        .checked_add(1)
        .ok_or_else(|| corrupt("epoch exhausted"))?;
    let header = BundleHeader {
        version: BUNDLE_V2,
        bundle_id: opened.header.bundle_id,
        epoch,
        keyset_generation: next_counter(opened.header.keyset_generation)?,
        revision: next_counter(opened.header.revision)?,
        kdf: raised,
        salt: crypto::random_array()?,
    };
    let data_key = DataKey::generate()?;
    let next_keys = crypto::derive_subkeys(&data_key, &header.bundle_id, epoch)?;
    let next_commit = crypto::epoch_commitment(&next_keys.rotate, &header.bundle_id, epoch)?;
    let keep = MAX_LINEAGE_LINKS.saturating_sub(1);
    let skip = opened.links.len().saturating_sub(keep);
    let mut links: Vec<LineageLink> = opened.links.iter().skip(skip).cloned().collect();
    let mut keys: Vec<(u32, KeyBytes)> = opened.lineage_keys.iter().skip(skip).cloned().collect();
    links.push(LineageLink {
        epoch: opened.header.epoch,
        digest: opened.commit,
        proof: crypto::lineage_proof(
            &opened.keys.rotate,
            &header.bundle_id,
            opened.header.epoch,
            &opened.commit,
            &next_commit,
        )?,
    });
    keys.push((opened.header.epoch, KeyBytes::of(&opened.keys.rotate)));
    let slot = passphrase_slot(&header, new_passphrase, &data_key, bounds)?;
    render(
        header,
        vec![slot],
        roster_v2(roster),
        data_key,
        Some(LineageOut {
            commit: next_commit,
            links,
            keys,
        }),
    )
}

// ── Roster union (§13.4.6) ──────────────────────────────────────────────

/// What [`union_rosters`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UnionReport {
    /// Devices listed forgotten with two different `revoked_clock`: only
    /// tampering can do that (entries are born at a rotation).
    pub(crate) clock_conflicts: Vec<DeviceId>,
}

/// Field-by-field union of two rosters of the same `(epoch, generation,
/// revision)` and lineage (§13.4.6): commutative, associative, idempotent.
/// - `revoked` is sticky: a device forgotten in either copy is forgotten
///   (and leaves `devices`); two clocks for one device keep the larger
///   (the caller then enforces a value it already observed) and are
///   reported.
/// - `policy` is closed in doubt: the higher generation wins, a disabled
///   copy wins a tie.
/// - `devices`: every active device of either copy (one entry per id, the
///   larger `(added_at, name)` on a clash: deterministic).
pub(crate) fn union_rosters(a: &Roster, b: &Roster) -> (Roster, UnionReport) {
    use std::collections::BTreeMap;
    let mut report = UnionReport::default();
    let mut revoked: BTreeMap<DeviceId, RevokedDevice> = BTreeMap::new();
    for entry in a.revoked.iter().chain(&b.revoked) {
        match revoked.get_mut(&entry.id) {
            None => {
                revoked.insert(entry.id, entry.clone());
            }
            Some(existing) => {
                if existing.revoked_clock != entry.revoked_clock
                    && !report.clock_conflicts.contains(&entry.id)
                {
                    report.clock_conflicts.push(entry.id);
                }
                let rank =
                    |e: &RevokedDevice| (e.revoked_clock, e.revoked_at, *e.revoked_by.bytes());
                if rank(entry) > rank(existing) {
                    *existing = entry.clone();
                }
            }
        }
    }
    // S1 flags count as forgotten for the union.
    for device in a.devices.iter().chain(&b.devices).filter(|d| d.revoked) {
        revoked.entry(device.id).or_insert_with(|| RevokedDevice {
            id: device.id,
            revoked_at: device.added_at,
            revoked_by: device.id,
            revoked_clock: 0,
        });
    }
    let mut devices: BTreeMap<DeviceId, RosterDevice> = BTreeMap::new();
    for device in a.devices.iter().chain(&b.devices) {
        if revoked.contains_key(&device.id) {
            continue;
        }
        let candidate = RosterDevice {
            revoked: false,
            ..device.clone()
        };
        match devices.get_mut(&device.id) {
            None => {
                devices.insert(device.id, candidate);
            }
            Some(existing) => {
                let rank = |d: &RosterDevice| (d.added_at, d.name.as_str().to_owned());
                if rank(&candidate) > rank(existing) {
                    *existing = candidate;
                }
            }
        }
    }
    let rank = |p: &Policy| {
        (
            p.generation,
            !p.include_passwords,
            p.changed_at,
            p.changed_by,
        )
    };
    let policy = if rank(&b.policy) > rank(&a.policy) {
        b.policy.clone()
    } else {
        a.policy.clone()
    };
    report.clock_conflicts.sort_unstable();
    (
        Roster {
            devices: devices.into_values().collect(),
            revoked: revoked.into_values().collect(),
            policy,
        },
        report,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::crypto::tests::derivations;
    use super::super::passphrase::tests::{STRONG, STRONG_2, typed};
    use super::*;

    type FileEdit = Box<dyn Fn(&mut BundleFile)>;

    pub(crate) fn pass(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    pub(crate) fn first_device() -> RosterDevice {
        RosterDevice {
            id: DeviceId::from_bytes([1; ID_LEN]),
            name: DeviceName::parse("Laptop").expect("name"),
            added_at: 1_700_000_000,
            revoked: false,
        }
    }

    /// What a key holder can write by hand (forged lineages, §13.3.2): a
    /// v2 file of `epoch` with a fresh data key, a slot under `passphrase`,
    /// and exactly the links, lineage keys and commitment given. Nothing
    /// is checked; the MAC is valid under the new key.
    #[allow(clippy::too_many_arguments)] // A forger chooses every field.
    pub(crate) fn forge(
        bundle_id: [u8; ID_LEN],
        epoch: u32,
        revision: u64,
        roster: Roster,
        links: Vec<LineageLink>,
        keys: Vec<(u32, KeyBytes)>,
        commit: Option<[u8; MAC_LEN]>,
        passphrase: &str,
    ) -> (String, OpenedBundle) {
        let header = BundleHeader {
            version: BUNDLE_V2,
            bundle_id,
            epoch,
            keyset_generation: 1,
            revision,
            kdf: KdfBounds::TEST_PARAMS,
            salt: crypto::random_array().expect("salt"),
        };
        let data_key = DataKey::generate().expect("dk");
        let real = crypto::epoch_commitment(
            &crypto::derive_subkeys(&data_key, &bundle_id, epoch)
                .expect("keys")
                .rotate,
            &bundle_id,
            epoch,
        )
        .expect("commit");
        let slot = passphrase_slot(&header, &typed(passphrase), &data_key, &KdfBounds::TEST)
            .expect("slot");
        render_inner(
            header,
            vec![slot],
            roster,
            data_key,
            Some(LineageOut {
                commit: commit.unwrap_or(real),
                links,
                keys,
            }),
            false,
        )
        .expect("forge")
    }

    pub(crate) fn new_bundle() -> (String, OpenedBundle) {
        create(
            &typed(STRONG),
            first_device(),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("create")
    }

    fn edit(text: &str, edit: impl FnOnce(&mut BundleFile)) -> String {
        let body = text.strip_prefix(FILE_COMMENT).expect("comment");
        let mut file: BundleFile = toml::from_str(body).expect("parse");
        edit(&mut file);
        format!(
            "{FILE_COMMENT}{}",
            toml::to_string_pretty(&file).expect("render")
        )
    }

    fn open_test(text: &str, passphrase: &str) -> Result<OpenedBundle, SyncError> {
        open(text, &pass(passphrase), &KdfBounds::TEST)
    }

    fn b64_flip(text: &str) -> String {
        let mut bytes = B64.decode(text).expect("b64");
        bytes[0] ^= 1;
        B64.encode(bytes)
    }

    #[test]
    fn create_then_open_round_trips() {
        let (text, created) = new_bundle();
        assert!(text.starts_with("# Portico encrypted sync bundle"));
        assert!(!text.contains("Laptop"), "device names are sealed");
        let opened = open_test(&text, STRONG).expect("open");
        assert_eq!(opened.header, created.header);
        assert_eq!(opened.roster, created.roster);
        assert_eq!(opened.data_key.bytes(), created.data_key.bytes());
        assert_eq!(opened.keys.snapshot.bytes(), created.keys.snapshot.bytes());
        assert_eq!(
            opened.fingerprint, created.fingerprint,
            "same authenticated content"
        );
        assert_eq!(opened.header.epoch, 1);
        assert_eq!(opened.header.keyset_generation, 1);
        assert_eq!(opened.header.revision, 1);
        let with_key = open_with_key(&text, &created.data_key).expect("open with key");
        assert_eq!(with_key.roster, created.roster);
        // Two creations never share ids, salts or keys.
        let (_, other) = new_bundle();
        assert_ne!(other.header.bundle_id, created.header.bundle_id);
        assert_ne!(other.header.salt, created.header.salt);
        assert_ne!(other.data_key.bytes(), created.data_key.bytes());
    }

    #[test]
    fn wrong_passphrase_and_slot_tampering_are_indistinguishable() {
        let (text, _) = new_bundle();
        assert_eq!(
            open_test(&text, "abacus abdomen abdominal abide abiding abilities")
                .expect_err("wrong"),
            SyncError::WrongPassphrase
        );
        let slot_bound: Vec<FileEdit> = vec![
            Box::new(|f| f.bundle_id = b64_flip(&f.bundle_id)),
            Box::new(|f| f.epoch += 1),
            Box::new(|f| f.keyset_generation += 1),
            Box::new(|f| f.kdf.salt = b64_flip(&f.kdf.salt)),
            Box::new(|f| f.kdf.memory_kib += 8),
            Box::new(|f| f.kdf.iterations += 1),
            Box::new(|f| f.kdf.parallelism += 1),
            Box::new(|f| f.keyslot[0].id = b64_flip(&f.keyslot[0].id)),
            Box::new(|f| f.keyslot[0].nonce = b64_flip(&f.keyslot[0].nonce)),
            Box::new(|f| f.keyslot[0].wrapped = b64_flip(&f.keyslot[0].wrapped)),
        ];
        for (i, tamper) in slot_bound.iter().enumerate() {
            let altered = edit(&text, |f| tamper(f));
            assert_eq!(
                open_test(&altered, STRONG).expect_err("tampered"),
                SyncError::WrongPassphrase,
                "edit {i}"
            );
        }
    }

    #[test]
    fn tampering_after_the_slot_is_reported_as_tampered() {
        let (text, opened) = new_bundle();
        let header_bound: Vec<FileEdit> = vec![
            Box::new(|f| f.revision += 1),
            Box::new(|f| f.mac = b64_flip(&f.mac)),
            Box::new(|f| f.roster.nonce = b64_flip(&f.roster.nonce)),
            Box::new(|f| f.roster.ciphertext = b64_flip(&f.roster.ciphertext)),
            Box::new(|f| {
                f.keyslot.push(KeyslotSection {
                    kind: SLOT_DEVICE.into(),
                    id: B64.encode([5u8; ID_LEN]),
                    device: Some("aa".into()),
                    enc: None,
                    nonce: B64.encode([0u8; NONCE_LEN]),
                    wrapped: B64.encode([0u8; KEY_LEN + TAG_LEN]),
                });
            }),
        ];
        for (i, tamper) in header_bound.iter().enumerate() {
            let altered = edit(&text, |f| tamper(f));
            assert_eq!(
                open_test(&altered, STRONG).expect_err("tampered"),
                SyncError::BundleTampered,
                "edit {i}"
            );
            assert_eq!(
                open_with_key(&altered, &opened.data_key).expect_err("tampered"),
                SyncError::BundleTampered,
                "edit {i} with key"
            );
        }
        // A key of another bundle fails the MAC too.
        let (_, other) = new_bundle();
        assert_eq!(
            open_with_key(&text, &other.data_key).expect_err("other key"),
            SyncError::BundleTampered
        );
    }

    #[test]
    fn a_flipped_byte_anywhere_is_rejected() {
        let (text, opened) = new_bundle();
        let bytes = text.as_bytes();
        for index in FILE_COMMENT.len()..bytes.len() {
            let mut altered = bytes.to_vec();
            altered[index] ^= 0x01;
            let Ok(altered) = String::from_utf8(altered) else {
                continue;
            };
            assert!(
                open_with_key(&altered, &opened.data_key).is_err(),
                "byte {index} ({:?})",
                char::from(bytes[index])
            );
        }
        // Spot-check the passphrase path (one derivation per try).
        for index in (FILE_COMMENT.len()..bytes.len()).step_by(97) {
            let mut altered = bytes.to_vec();
            altered[index] ^= 0x01;
            if let Ok(altered) = String::from_utf8(altered) {
                assert!(open_test(&altered, STRONG).is_err(), "byte {index}");
            }
        }
    }

    #[test]
    fn versions_formats_and_unknown_fields() {
        let (text, _) = new_bundle();
        for (version, expected) in [(3u32, 3u32), (0, 0), (99, 99)] {
            let altered = edit(&text, |f| f.version = version);
            assert_eq!(
                open_test(&altered, STRONG).expect_err("version"),
                SyncError::UnsupportedVersion {
                    what: WHAT,
                    found: expected,
                    supported: 2
                }
            );
        }
        // A v3 file with fields this version does not know still reports
        // the version.
        let v3 = format!("{}\nfuture_field = 1\n", edit(&text, |f| f.version = 3));
        assert!(matches!(
            open_test(&v3, STRONG),
            Err(SyncError::UnsupportedVersion { found: 3, .. })
        ));
        // A v1 file claiming version 2 lacks the v2 fields: refused before
        // any derivation.
        assert!(matches!(
            open_test(&edit(&text, |f| f.version = 2), STRONG),
            Err(SyncError::Corrupt { .. })
        ));
        let unknown = format!("future_field = 1\n{}", edit(&text, |_| {}));
        assert!(matches!(
            open_test(&unknown, STRONG),
            Err(SyncError::Corrupt { .. })
        ));
        let other = edit(&text, |f| f.format = "portico-vault".into());
        assert!(matches!(
            open_test(&other, STRONG),
            Err(SyncError::Corrupt { .. })
        ));
        for garbage in ["", "garbage = [", "format = 1", "version = 1"] {
            assert!(
                matches!(open_test(garbage, STRONG), Err(SyncError::Corrupt { .. })),
                "{garbage}"
            );
        }
        let cases: Vec<FileEdit> = vec![
            Box::new(|f| f.kdf.algorithm = "scrypt".into()),
            Box::new(|f| f.kdf.version = 16),
            Box::new(|f| f.epoch = 0),
            Box::new(|f| f.keyslot.clear()),
            Box::new(|f| f.keyslot[0].kind = "other".into()),
            Box::new(|f| {
                let slot = f.keyslot[0].clone();
                f.keyslot.push(slot);
            }),
            Box::new(|f| f.keyslot[0].wrapped = B64.encode([0u8; 10])),
            Box::new(|f| f.kdf.salt = B64.encode([0u8; 8])),
            Box::new(|f| f.bundle_id = "not base64!".into()),
            Box::new(|f| f.roster.ciphertext = B64.encode([0u8; 100])),
            Box::new(|f| f.mac = B64.encode([0u8; 31])),
        ];
        for (i, case) in cases.iter().enumerate() {
            let altered = edit(&text, |f| case(f));
            assert!(
                matches!(open_test(&altered, STRONG), Err(SyncError::Corrupt { .. })),
                "case {i}"
            );
        }
    }

    #[test]
    fn size_cap_is_checked_before_parsing() {
        let (text, _) = new_bundle();
        let huge = format!("{text}\n#{}\n", "x".repeat(MAX_BUNDLE_BYTES));
        assert_eq!(
            open_test(&huge, STRONG).expect_err("too large"),
            SyncError::TooLarge {
                what: WHAT,
                max: MAX_BUNDLE_BYTES
            }
        );
    }

    #[test]
    fn hostile_kdf_parameters_are_refused_before_deriving() {
        let (text, _) = new_bundle();
        // The test bundle (64 KiB, 1 pass) is below the production floor.
        let before = derivations();
        assert!(matches!(
            open(&text, &pass(STRONG), &KdfBounds::PRODUCTION),
            Err(SyncError::KdfOutOfBounds {
                param: "memory_kib",
                ..
            })
        ));
        let hostile: [fn(&mut BundleFile); 5] = [
            |f| f.kdf.memory_kib = 8,
            |f| f.kdf.memory_kib = u32::MAX,
            |f| f.kdf.iterations = u32::MAX,
            |f| f.kdf.parallelism = 0,
            |f| f.kdf.parallelism = 1024,
        ];
        for edit_fn in hostile {
            let altered = edit(&text, edit_fn);
            for bounds in [KdfBounds::TEST, KdfBounds::PRODUCTION] {
                let started = std::time::Instant::now();
                assert!(matches!(
                    open(&altered, &pass(STRONG), &bounds),
                    Err(SyncError::KdfOutOfBounds { .. })
                ));
                assert!(started.elapsed() < std::time::Duration::from_millis(200));
            }
        }
        assert_eq!(derivations(), before, "no derivation ever started");
        // Creation refuses out-of-bounds parameters too.
        assert!(matches!(
            create(
                &typed(STRONG),
                first_device(),
                KdfParams::DEFAULT,
                &KdfBounds::TEST
            ),
            Err(SyncError::KdfOutOfBounds { .. })
        ));
    }

    #[test]
    fn weak_passphrases_are_refused_at_create_and_change() {
        let before = derivations();
        assert!(matches!(
            create(
                &typed("hunter2"),
                first_device(),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
        let (text, opened) = new_bundle();
        let after_create = derivations();
        assert_eq!(after_create, before + 1);
        assert!(matches!(
            change_passphrase(
                &text,
                &pass(STRONG),
                &typed("password1"),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
        assert_eq!(derivations(), after_create, "refused before deriving");
        assert!(matches!(
            rewrap(
                &opened,
                &typed("short"),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
    }

    #[test]
    fn change_passphrase_rewraps_the_same_data_key() {
        let (text, created) = new_bundle();
        let new = STRONG_2;
        let (changed, opened) = change_passphrase(
            &text,
            &pass(STRONG),
            &typed(new),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("change");
        assert_eq!(opened.header.keyset_generation, 2);
        assert_eq!(opened.header.revision, 2);
        assert_eq!(opened.header.epoch, 1);
        assert_eq!(opened.header.bundle_id, created.header.bundle_id);
        assert_ne!(opened.header.salt, created.header.salt);
        assert_eq!(opened.data_key.bytes(), created.data_key.bytes());
        assert_eq!(opened.roster, created.roster);
        assert_eq!(
            open_test(&changed, STRONG).expect_err("old"),
            SyncError::WrongPassphrase
        );
        let reopened = open_test(&changed, new).expect("new");
        assert_eq!(reopened.data_key.bytes(), created.data_key.bytes());
        // Wrong old passphrase: refused, nothing produced.
        assert_eq!(
            change_passphrase(
                &text,
                &pass(new),
                &typed(new),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            )
            .expect_err("wrong old"),
            SyncError::WrongPassphrase
        );
        // Splicing the old slot (generation 1) into the new header fails.
        let spliced = {
            let old: BundleFile =
                toml::from_str(text.strip_prefix(FILE_COMMENT).expect("c")).expect("p");
            edit(&changed, |f| {
                f.keyslot = old.keyslot.clone();
                f.kdf.salt = old.kdf.salt.clone();
            })
        };
        assert_eq!(
            open_test(&spliced, STRONG).expect_err("spliced"),
            SyncError::WrongPassphrase
        );
    }

    #[test]
    fn old_bundle_copy_opens_with_old_passphrase_after_change() {
        // Documents the limit of a passphrase change (security review F2):
        // the provider's version history keeps the old file, which still
        // yields the same data key. Only a rotation (S2) protects.
        let (old_text, created) = new_bundle();
        let (_new_text, _) = change_passphrase(
            &old_text,
            &pass(STRONG),
            &typed(STRONG_2),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("change");
        let from_history = open_test(&old_text, STRONG).expect("old copy still opens");
        assert_eq!(from_history.data_key.bytes(), created.data_key.bytes());
    }

    #[test]
    fn reset_from_a_device_holding_the_key() {
        let (text, created) = new_bundle();
        let holder = open_with_key(&text, &created.data_key).expect("open with key");
        let new = STRONG_2;
        let (reset, opened) = rewrap(
            &holder,
            &typed(new),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("reset");
        assert_eq!(opened.header.keyset_generation, 2);
        open_test(&reset, new).expect("new passphrase");
        assert_eq!(
            open_test(&reset, STRONG).expect_err("old"),
            SyncError::WrongPassphrase
        );
    }

    #[test]
    fn roster_updates_bump_the_revision_only() {
        let (_, created) = new_bundle();
        let mut roster = created.roster.clone();
        roster.devices.push(RosterDevice {
            id: DeviceId::from_bytes([2; ID_LEN]),
            name: DeviceName::parse("Desktop").expect("name"),
            added_at: 1_700_000_100,
            revoked: false,
        });
        let (text, updated) = with_roster(&created, roster.clone()).expect("roster");
        assert_eq!(updated.header.revision, 2);
        assert_eq!(updated.header.keyset_generation, 1);
        let opened = open_test(&text, STRONG).expect("open");
        assert_eq!(opened.roster, roster);
        // Duplicate devices and oversized rosters are refused.
        let mut duplicate = roster.clone();
        duplicate.devices.push(roster.devices[0].clone());
        assert!(matches!(
            with_roster(&created, duplicate),
            Err(SyncError::Corrupt { .. })
        ));
        let many = Roster::of(
            (0..=MAX_ROSTER_DEVICES)
                .map(|i| RosterDevice {
                    id: DeviceId::from_bytes([u8::try_from(i).expect("u8"); ID_LEN]),
                    name: DeviceName::parse("d").expect("name"),
                    added_at: 0,
                    revoked: false,
                })
                .collect(),
        );
        assert!(matches!(
            with_roster(&created, many),
            Err(SyncError::TooLarge { .. })
        ));
        // The largest roster still fits the file cap.
        let full = Roster::of(
            (0..MAX_ROSTER_DEVICES)
                .map(|i| RosterDevice {
                    id: DeviceId::from_bytes([u8::try_from(i).expect("u8"); ID_LEN]),
                    name: DeviceName::parse(&"n".repeat(64)).expect("name"),
                    added_at: i64::MAX,
                    revoked: true,
                })
                .collect(),
        );
        let (text, _) = with_roster(&created, full).expect("fits");
        open_test(&text, STRONG).expect("open full");
    }

    #[test]
    fn device_slots_are_kept_and_authenticated() {
        let (_, created) = new_bundle();
        let mut slots = created.slots.clone();
        slots.push(Slot {
            kind: SLOT_DEVICE.into(),
            id: [9; ID_LEN],
            device: Some(DeviceId::from_bytes([1; ID_LEN]).to_string()),
            enc: Some(B64.encode([3u8; 32])),
            nonce: [4; NONCE_LEN],
            wrapped: vec![5; KEY_LEN + TAG_LEN],
        });
        let (text, _) = render(
            created.header.clone(),
            slots,
            created.roster.clone(),
            created.data_key.duplicate(),
            None,
        )
        .expect("render");
        let opened = open_test(&text, STRONG).expect("open");
        assert_eq!(opened.slots.len(), 2);
        let (rerendered, _) = with_roster(&opened, opened.roster.clone()).expect("roster");
        assert_eq!(open_test(&rerendered, STRONG).expect("open").slots.len(), 2);
        let altered = edit(&text, |f| f.keyslot[1].enc = Some(B64.encode([6u8; 32])));
        assert_eq!(
            open_test(&altered, STRONG).expect_err("tampered"),
            SyncError::BundleTampered
        );
    }

    // ── S2a ────────────────────────────────────────────────────────────

    use super::super::passphrase::tests::{STRONG_3, STRONG_4};

    fn rotate_test(opened: &OpenedBundle, passphrase: &str) -> (String, OpenedBundle) {
        rotate(
            opened,
            &typed(passphrase),
            opened.roster.clone(),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("rotate")
    }

    fn device(n: u8, name: &str) -> RosterDevice {
        RosterDevice {
            id: DeviceId::from_bytes([n; ID_LEN]),
            name: DeviceName::parse(name).expect("name"),
            added_at: i64::from(n),
            revoked: false,
        }
    }

    fn revoked(n: u8, clock: u64) -> RevokedDevice {
        RevokedDevice {
            id: DeviceId::from_bytes([n; ID_LEN]),
            revoked_at: 7,
            revoked_by: DeviceId::from_bytes([1; ID_LEN]),
            revoked_clock: clock,
        }
    }

    #[test]
    fn rotate_starts_the_next_epoch_with_a_proved_link() {
        let (_, n) = new_bundle();
        let (text, next) = rotate_test(&n, STRONG_2);
        assert_eq!(next.header.version, BUNDLE_V2);
        assert_eq!(next.header.epoch, 2);
        assert_eq!(next.header.keyset_generation, 2);
        assert_eq!(next.header.revision, 2);
        assert_eq!(next.header.bundle_id, n.header.bundle_id);
        assert_ne!(
            next.data_key.bytes(),
            n.data_key.bytes(),
            "a fresh data key"
        );
        assert_ne!(next.header.salt, n.header.salt);
        // The link of epoch 1 carries C_1 and verifies under K_rot_1 only.
        let link = next.links.last().expect("link");
        assert_eq!((link.epoch, link.digest), (1, n.commit));
        assert!(
            crypto::verify_lineage_proof(
                &n.keys.rotate,
                &n.header.bundle_id,
                1,
                &n.commit,
                &next.commit,
                &link.proof
            )
            .expect("v")
        );
        assert!(
            !crypto::verify_lineage_proof(
                &next.keys.rotate,
                &n.header.bundle_id,
                1,
                &n.commit,
                &next.commit,
                &link.proof
            )
            .expect("v")
        );
        // Neither DK_N nor P opens N + 1; P' does, and reads the lineage
        // keys (sealed under N + 1 only).
        assert_eq!(
            open_with_key(&text, &n.data_key).expect_err("old key"),
            SyncError::BundleTampered
        );
        assert_eq!(
            open_test(&text, STRONG).expect_err("old passphrase"),
            SyncError::WrongPassphrase
        );
        let opened = open_test(&text, STRONG_2).expect("new");
        assert_eq!(opened.lineage_keys.len(), 1);
        assert_eq!(
            opened.rotate_key(1).map(|k| *k.bytes()),
            Some(*n.keys.rotate.bytes())
        );
        assert_eq!(opened.commit, next.commit);
        let clear = peek(&text).expect("peek");
        assert_eq!(clear.commit, Some(next.commit));
        assert_eq!(
            clear.link_at(1).map(|(l, c)| (l.digest, c)),
            Some((n.commit, next.commit))
        );
        // Weak new passphrases are refused before any derivation.
        let before = derivations();
        assert!(matches!(
            rotate(
                &n,
                &typed("hunter2"),
                n.roster.clone(),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
        assert_eq!(derivations(), before);
    }

    #[test]
    fn commitment_is_stable_across_revisions_and_passphrase_changes() {
        let (_, n) = new_bundle();
        let (_, next) = rotate_test(&n, STRONG_2);
        let (_, roster) = with_roster(&next, next.roster.clone()).expect("roster");
        let (changed_text, changed) = rewrap(
            &roster,
            &typed(STRONG_3),
            KdfBounds::TEST_PARAMS,
            &KdfBounds::TEST,
        )
        .expect("rewrap");
        assert_eq!(roster.commit, next.commit);
        assert_eq!(changed.commit, next.commit);
        assert_eq!(changed.links, next.links, "the lineage is kept");
        assert_eq!(changed.header.version, BUNDLE_V2);
        let reopened = open_test(&changed_text, STRONG_3).expect("open");
        assert_eq!(reopened.commit, next.commit);
        // A v1 bundle has the same commitment once converted (E1).
        let (v2_text, v2) = to_v2(&n, n.roster.clone()).expect("v2");
        assert_eq!(v2.commit, n.commit);
        assert_eq!(v2.header.epoch, 1);
        assert_eq!(v2.header.revision, 2);
        assert!(v2.links.is_empty());
        assert_eq!(
            open_test(&v2_text, STRONG).expect("open").header.version,
            BUNDLE_V2
        );
    }

    #[test]
    fn lineage_keeps_the_sixteen_newest_links() {
        let (_, mut current) = new_bundle();
        let phrases = [STRONG, STRONG_2, STRONG_3, STRONG_4];
        for i in 0..18 {
            let phrase = phrases.get(i % phrases.len()).copied().unwrap_or(STRONG);
            let (text, next) = rotate_test(&current, phrase);
            let reopened = open_test(&text, phrase).expect("open");
            assert_eq!(reopened.links, next.links);
            current = next;
        }
        assert_eq!(current.header.epoch, 19);
        assert_eq!(current.links.len(), MAX_LINEAGE_LINKS);
        let epochs: Vec<u32> = current.links.iter().map(|l| l.epoch).collect();
        assert_eq!(epochs, (3..19).collect::<Vec<u32>>());
    }

    #[test]
    fn v1_stays_v1_and_is_encoded_as_in_s1() {
        let (_, created) = new_bundle();
        let mut roster = created.roster.clone();
        roster.devices.push(device(2, "Desktop"));
        let (text, updated) = with_roster(&created, roster).expect("roster");
        assert!(text.contains("version = 1"));
        assert!(!text.contains("commit") && !text.contains("lineage"));
        assert_eq!(updated.header.version, BUNDLE_V1);
        // The sealed roster is exactly the S1 JSON.
        let parsed = parse(&text).expect("parse");
        let plain = crypto::open(
            updated.keys.roster.bytes(),
            &parsed.roster_nonce,
            &roster_aad(&parsed.header),
            &parsed.roster_ciphertext,
        )
        .expect("open roster");
        let json = std::str::from_utf8(&plain).expect("utf8").trim_end();
        let one = format!(
            "{{\"id\":\"{}\",\"name\":\"Laptop\",\"added_at\":1700000000,\"revoked\":false}}",
            DeviceId::from_bytes([1; ID_LEN])
        );
        let two = format!(
            "{{\"id\":\"{}\",\"name\":\"Desktop\",\"added_at\":2,\"revoked\":false}}",
            DeviceId::from_bytes([2; ID_LEN])
        );
        assert_eq!(json, format!("{{\"devices\":[{one},{two}]}}"));
        // A v1 bundle cannot hold S2 roster fields: converted first.
        let mut with_revoked = updated.roster.clone();
        with_revoked.revoked.push(revoked(9, 3));
        assert!(with_roster(&updated, with_revoked.clone()).is_err());
        let (v2_text, _) = to_v2(&updated, with_revoked.clone()).expect("v2");
        let opened = open_test(&v2_text, STRONG).expect("open");
        assert_eq!(opened.roster.revoked, with_revoked.revoked);
        assert_eq!(opened.roster.policy, Policy::default());
    }

    #[test]
    fn roster_v2_round_trips_and_stays_disjoint() {
        let (_, created) = new_bundle();
        let roster = Roster {
            devices: vec![first_device(), device(2, "Desktop")],
            revoked: vec![revoked(3, 41), revoked(4, 0)],
            policy: Policy {
                include_passwords: true,
                generation: 3,
                changed_by: Some(DeviceId::from_bytes([2; ID_LEN])),
                changed_at: 9,
            },
        };
        let (text, _) = to_v2(&created, roster.clone()).expect("v2");
        assert_eq!(open_test(&text, STRONG).expect("open").roster, roster);
        let mut both = roster.clone();
        both.revoked.push(revoked(2, 1));
        assert!(matches!(
            to_v2(&created, both),
            Err(SyncError::Corrupt { .. })
        ));
        // S1 flags become forgotten entries in v2.
        let mut flagged = Roster::of(vec![first_device()]);
        let mut old = device(5, "Old");
        old.revoked = true;
        flagged.devices.push(old);
        let converted = roster_v2(flagged);
        assert_eq!(converted.devices.len(), 1);
        assert_eq!(converted.revoked.len(), 1);
        // The forgotten list is capped (never pruned): beyond, refused.
        let full = Roster {
            devices: vec![first_device()],
            revoked: (0..=MAX_REVOKED_DEVICES)
                .map(|i| RevokedDevice {
                    id: DeviceId::from_bytes({
                        let mut id = [0xEE; ID_LEN];
                        id[..8].copy_from_slice(&(i as u64).to_be_bytes());
                        id
                    }),
                    revoked_at: 0,
                    revoked_by: DeviceId::from_bytes([1; ID_LEN]),
                    revoked_clock: 0,
                })
                .collect(),
            policy: Policy::default(),
        };
        assert!(matches!(
            to_v2(&created, full.clone()),
            Err(SyncError::RosterFull { .. })
        ));
        let mut fits = full;
        fits.revoked.pop();
        let (text, _) = to_v2(&created, fits).expect("1024 entries fit");
        assert!(text.len() <= MAX_BUNDLE_BYTES);
    }

    #[test]
    fn commit_and_lineage_are_covered_by_the_mac() {
        let (_, n) = new_bundle();
        let (text, next) = rotate_test(&n, STRONG_2);
        let edits: Vec<FileEdit> = vec![
            Box::new(|f| f.commit = f.commit.as_deref().map(b64_flip)),
            Box::new(|f| f.lineage[0].digest = b64_flip(&f.lineage[0].digest)),
            Box::new(|f| f.lineage[0].proof = b64_flip(&f.lineage[0].proof)),
            Box::new(|f| {
                if let Some(keys) = f.lineage_keys.as_mut() {
                    keys.ciphertext = b64_flip(&keys.ciphertext);
                }
            }),
            Box::new(|f| f.version = 1),
        ];
        for (i, tamper) in edits.iter().enumerate() {
            let altered = edit(&text, |f| tamper(f));
            assert!(open_with_key(&altered, &next.data_key).is_err(), "edit {i}");
        }
        // Lineage shape is refused before any derivation.
        let before = derivations();
        let shapes: Vec<FileEdit> = vec![
            Box::new(|f| f.lineage.clear()),
            Box::new(|f| f.lineage[0].epoch = 0),
            Box::new(|f| {
                let link = f.lineage[0].clone();
                f.lineage.insert(0, LineageSection { epoch: 5, ..link });
            }),
            Box::new(|f| {
                let link = f.lineage[0].clone();
                f.lineage = vec![link; MAX_LINEAGE_LINKS + 1];
            }),
        ];
        for (i, shape) in shapes.iter().enumerate() {
            let altered = edit(&text, |f| shape(f));
            assert_eq!(
                open_test(&altered, STRONG_2).expect_err("shape"),
                SyncError::BundleTampered,
                "shape {i}"
            );
            assert_eq!(
                peek(&altered).expect_err("shape"),
                SyncError::BundleTampered
            );
        }
        assert_eq!(derivations(), before);
    }

    #[test]
    fn a_forged_chain_never_opens() {
        let (_, n) = new_bundle();
        let (genuine, next) = rotate_test(&n, STRONG_2);
        // Copy the authentic link and add a link the forger cannot prove.
        let clear = peek(&genuine).expect("peek");
        let mut links = clear.links.clone();
        links.push(LineageLink {
            epoch: 2,
            digest: next.commit,
            proof: [3; MAC_LEN],
        });
        let fake = {
            let dk = DataKey::generate().expect("dk");
            KeyBytes::of(
                &crypto::derive_subkeys(&dk, &[0; ID_LEN], 2)
                    .expect("k")
                    .rotate,
            )
        };
        let keys = vec![(1, KeyBytes::of(&n.keys.rotate)), (2, fake)];
        let (forged, _) = forge(
            n.header.bundle_id,
            3,
            1,
            n.roster.clone(),
            links,
            keys,
            None,
            STRONG,
        );
        assert_eq!(
            open_test(&forged, STRONG).expect_err("forged"),
            SyncError::BundleTampered
        );
        // A forged commitment does not open either.
        let (bad_commit, _) = forge(
            n.header.bundle_id,
            2,
            1,
            n.roster.clone(),
            clear.links.clone(),
            vec![(1, KeyBytes::of(&n.keys.rotate))],
            Some(next.commit),
            STRONG,
        );
        assert_eq!(
            open_test(&bad_commit, STRONG).expect_err("forged"),
            SyncError::BundleTampered
        );
    }

    #[test]
    fn passphrase_opens_checks_one_slot() {
        let (text, _) = new_bundle();
        assert!(passphrase_opens(&text, &pass(STRONG), &KdfBounds::TEST));
        assert!(!passphrase_opens(&text, &pass(STRONG_2), &KdfBounds::TEST));
        assert!(!passphrase_opens(
            "garbage",
            &pass(STRONG),
            &KdfBounds::TEST
        ));
        assert!(!passphrase_opens(
            &text,
            &pass(STRONG),
            &KdfBounds::PRODUCTION
        ));
    }

    mod union {
        use proptest::prelude::*;

        use super::super::*;

        fn id(n: u8) -> DeviceId {
            DeviceId::from_bytes([n; ID_LEN])
        }

        fn roster_strategy() -> impl Strategy<Value = Roster> {
            (
                proptest::collection::btree_map(0u8..6, 0i64..3, 0..4),
                proptest::collection::btree_map(4u8..9, 0u64..4, 0..3),
                (0u64..3, any::<bool>()),
            )
                .prop_map(|(devices, revoked, (generation, on))| Roster {
                    devices: devices
                        .into_iter()
                        .map(|(n, added_at)| RosterDevice {
                            id: id(n),
                            name: DeviceName::parse(&format!("d{n}")).expect("name"),
                            added_at,
                            revoked: false,
                        })
                        .collect(),
                    revoked: revoked
                        .into_iter()
                        .map(|(n, clock)| RevokedDevice {
                            id: id(n),
                            revoked_at: 0,
                            revoked_by: id(1),
                            revoked_clock: clock,
                        })
                        .collect(),
                    policy: Policy {
                        include_passwords: on,
                        generation,
                        changed_by: None,
                        changed_at: 0,
                    },
                })
        }

        fn union(a: &Roster, b: &Roster) -> Roster {
            union_rosters(a, b).0
        }

        proptest! {
            #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

            #[test]
            fn roster_union_laws(a in roster_strategy(), b in roster_strategy(), c in roster_strategy()) {
                prop_assert_eq!(union(&a, &b), union(&b, &a), "commutative");
                prop_assert_eq!(union(&union(&a, &b), &c), union(&a, &union(&b, &c)), "associative");
                let aa = union(&a, &a);
                prop_assert_eq!(union(&aa, &a), aa.clone(), "idempotent");
                let ab = union(&a, &b);
                // Sticky: forgotten anywhere, forgotten in the union, and
                // never active.
                for entry in a.revoked.iter().chain(&b.revoked) {
                    prop_assert!(ab.revoked_entry(&entry.id).is_some());
                    prop_assert!(ab.devices.iter().all(|d| d.id != entry.id));
                }
                // No active device lost.
                for device in a.devices.iter().chain(&b.devices) {
                    prop_assert!(
                        ab.devices.iter().any(|d| d.id == device.id)
                            || ab.revoked_entry(&device.id).is_some()
                    );
                }
                // Closed policy: the highest generation, off on a tie.
                let generation = a.policy.generation.max(b.policy.generation);
                prop_assert_eq!(ab.policy.generation, generation);
                let off_at_top = [&a, &b]
                    .iter()
                    .any(|r| r.policy.generation == generation && !r.policy.include_passwords);
                prop_assert_eq!(ab.policy.include_passwords, !off_at_top);
            }
        }

        #[test]
        fn two_clocks_for_one_forgotten_device_are_reported() {
            let mut a = Roster::default();
            let mut b = Roster::default();
            a.revoked.push(RevokedDevice {
                id: id(7),
                revoked_at: 0,
                revoked_by: id(1),
                revoked_clock: 3,
            });
            b.revoked.push(RevokedDevice {
                id: id(7),
                revoked_at: 0,
                revoked_by: id(1),
                revoked_clock: 5,
            });
            let (merged, report) = union_rosters(&a, &b);
            assert_eq!(report.clock_conflicts, vec![id(7)]);
            assert_eq!(
                merged.revoked_entry(&id(7)).map(|e| e.revoked_clock),
                Some(5)
            );
        }
    }

    #[test]
    fn errors_and_debug_hold_no_secret() {
        let (text, opened) = new_bundle();
        let debug = format!("{opened:?}");
        assert!(!debug.contains(STRONG));
        assert!(debug.contains("<redacted>"));
        let error = open_test(&text, "abacus abdomen abdominal abide abiding abilities")
            .expect_err("wrong");
        assert!(!format!("{error} {error:?}").contains("abacus"));
    }
}
