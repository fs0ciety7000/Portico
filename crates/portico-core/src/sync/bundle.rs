//! `portico-sync/bundle.toml`: KDF parameters, key slots, sealed roster,
//! header MAC (S1a, `docs/SYNC.md` §2–3).
//!
//! ```toml
//! format = "portico-sync"
//! version = 1
//! bundle_id = "base64(16)"
//! epoch = 1                 # data key generation (rotation: S2/S3)
//! keyset_generation = 1     # +1 at every key slot change (passphrase change/reset)
//! revision = 1              # +1 at every write of this file
//! mac = "base64(32)"        # HMAC-SHA256 under the header subkey
//! [kdf]                     # argon2id v19: memory_kib, iterations, parallelism, salt
//! [[keyslot]]               # kind = "passphrase": id, nonce, wrapped
//! [roster]                  # nonce, ciphertext (sealed device list)
//! ```
//!
//! - **Open** checks, in this order: size cap, format and version, strict
//!   parsing (`deny_unknown_fields`), field sizes, KDF bounds (**before**
//!   any derivation), slot unwrap, header MAC (constant time), roster.
//! - **Wrong passphrase vs tampering**: a slot that does not open gives
//!   [`SyncError::WrongPassphrase`], whether the passphrase is wrong or any
//!   slot-bound field (bundle id, epoch, keyset generation, slot kind and
//!   id, KDF parameters, salt, nonce, wrapped key) was altered: no oracle.
//!   A slot that opens with a MAC or roster that does not verify gives
//!   [`SyncError::BundleTampered`]. The functions are pure: nothing is
//!   written, whatever the outcome.
//! - **Key slot AAD** binds the keyset generation (security review F4), so
//!   an older slot cannot be spliced into a newer header.
//! - **Change of passphrase** re-wraps the same data key under a new salt
//!   and slot id (keyset generation + 1). It does **not** protect against
//!   an already-leaked passphrase: an older copy of this file (provider
//!   version history) still opens with the old passphrase and yields the
//!   same data key (F2, documented and tested). Only a rotation (new
//!   epoch, S2) does.
//! - Device slots (`kind = "device"`, S3) are parsed, covered by the MAC
//!   and kept verbatim; S1 never uses them.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::SyncError;
use super::clock::{DeviceId, DeviceName};
use super::crypto::{
    self, Canonical, DataKey, ID_LEN, KEY_LEN, KdfBounds, KdfParams, MAC_LEN, NONCE_LEN, SALT_LEN,
    Subkeys, TAG_LEN,
};
use super::passphrase;

/// Value of the `format` field.
pub(crate) const BUNDLE_FORMAT: &str = "portico-sync";
/// Current (and only readable) format version.
pub(crate) const BUNDLE_VERSION: u32 = 1;
/// Larger files are refused before parsing.
pub(crate) const MAX_BUNDLE_BYTES: usize = 64 * 1024;
/// Most key slots (one passphrase slot + S3 device slots).
const MAX_KEYSLOTS: usize = 32;
/// Most devices in the roster.
pub(crate) const MAX_ROSTER_DEVICES: usize = 64;
/// Smallest padding bucket of the sealed roster.
const ROSTER_MIN_BUCKET: usize = 1024;

const KDF_ALGORITHM: &str = "argon2id";
const ARGON2_VERSION: u32 = 0x13;
const SLOT_PASSPHRASE: &str = "passphrase";
const SLOT_DEVICE: &str = "device";
const WHAT: &str = "bundle.toml";

const FILE_COMMENT: &str = "# Portico encrypted sync bundle. Do not edit: any change makes it unreadable.\n\
# See docs/SYNC.md in the Portico repository.\n\n";

/// One device of the sealed roster.
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

/// The device list, sealed inside `bundle.toml` (names never in clear).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Roster {
    pub(crate) devices: Vec<RosterDevice>,
}

/// Verified, non-secret header of an opened bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleHeader {
    pub(crate) bundle_id: [u8; ID_LEN],
    pub(crate) epoch: u32,
    pub(crate) keyset_generation: u64,
    pub(crate) revision: u64,
    pub(crate) kdf: KdfParams,
    pub(crate) salt: [u8; SALT_LEN],
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

/// A bundle whose slot opened and whose MAC and roster verified.
#[derive(Debug)]
pub(crate) struct OpenedBundle {
    pub(crate) header: BundleHeader,
    pub(crate) roster: Roster,
    pub(crate) data_key: DataKey,
    pub(crate) keys: Subkeys,
    /// SHA-256 of the file bytes (bundle high-water mark, F4).
    pub(crate) fingerprint: [u8; 32],
    slots: Vec<Slot>,
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
    mac: String,
    kdf: KdfSection,
    keyslot: Vec<KeyslotSection>,
    roster: RosterSection,
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

/// The decoded, unverified content of the file.
struct Parsed {
    header: BundleHeader,
    slots: Vec<Slot>,
    roster_nonce: [u8; NONCE_LEN],
    roster_ciphertext: Vec<u8>,
    mac: [u8; MAC_LEN],
}

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
    if version != BUNDLE_VERSION {
        return Err(SyncError::UnsupportedVersion {
            what: WHAT,
            found: version,
            supported: BUNDLE_VERSION,
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
    Ok(Parsed {
        header: BundleHeader {
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
        mac: decode_exact(&file.mac, "bad mac")?,
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

fn mac_input(
    header: &BundleHeader,
    slots: &[Slot],
    roster_nonce: &[u8; NONCE_LEN],
    roster_ciphertext: &[u8],
) -> Vec<u8> {
    let mut input = Canonical::new("bundle-mac");
    input
        .str(BUNDLE_FORMAT)
        .u32(BUNDLE_VERSION)
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

/// Verifies the MAC and opens the roster with an unwrapped data key.
fn verify(parsed: Parsed, text: &str, data_key: DataKey) -> Result<OpenedBundle, SyncError> {
    let keys = crypto::derive_subkeys(&data_key, &parsed.header.bundle_id, parsed.header.epoch)?;
    let input = mac_input(
        &parsed.header,
        &parsed.slots,
        &parsed.roster_nonce,
        &parsed.roster_ciphertext,
    );
    if !crypto::verify_mac(&keys.header, &input, &parsed.mac)? {
        return Err(SyncError::BundleTampered);
    }
    let roster_plain = crypto::open(
        keys.roster.bytes(),
        &parsed.roster_nonce,
        &roster_aad(&parsed.header),
        &parsed.roster_ciphertext,
    )
    .map_err(|_| SyncError::BundleTampered)?;
    let roster: Roster =
        serde_json::from_slice(&roster_plain).map_err(|_| corrupt("bad roster content"))?;
    check_roster(&roster)?;
    Ok(OpenedBundle {
        header: parsed.header,
        roster,
        data_key,
        keys,
        fingerprint: crypto::sha256(text.as_bytes()),
        slots: parsed.slots,
    })
}

fn check_roster(roster: &Roster) -> Result<(), SyncError> {
    if roster.devices.len() > MAX_ROSTER_DEVICES {
        return Err(SyncError::TooLarge {
            what: "sync roster",
            max: MAX_ROSTER_DEVICES,
        });
    }
    let mut ids: Vec<&DeviceId> = roster.devices.iter().map(|d| &d.id).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|w| matches!(w, [a, b] if a == b)) {
        return Err(corrupt("duplicate device in the roster"));
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
    verify(parsed, text, data_key)
}

/// Opens `bundle.toml` with a remembered data key (every poll): a key of
/// another bundle or epoch fails the MAC ([`SyncError::BundleTampered`]).
pub(crate) fn open_with_key(text: &str, data_key: &DataKey) -> Result<OpenedBundle, SyncError> {
    let parsed = parse(text)?;
    verify(parsed, text, DataKey::from_bytes(data_key.bytes())?)
}

/// Renders a bundle; seals the roster and computes the MAC.
fn render(
    header: BundleHeader,
    slots: Vec<Slot>,
    roster: Roster,
    data_key: DataKey,
) -> Result<(String, OpenedBundle), SyncError> {
    check_roster(&roster)?;
    let keys = crypto::derive_subkeys(&data_key, &header.bundle_id, header.epoch)?;
    let roster_json = Zeroizing::new(
        serde_json::to_vec(&roster).map_err(|_| SyncError::Crypto("roster encoding failed"))?,
    );
    let padded = crypto::padded(&roster_json, ROSTER_MIN_BUCKET, "sync roster")?;
    let (roster_nonce, roster_ciphertext) =
        crypto::seal(keys.roster.bytes(), &roster_aad(&header), padded)?;
    let input = mac_input(&header, &slots, &roster_nonce, &roster_ciphertext);
    let mac = crypto::mac(&keys.header, &input)?;
    let file = BundleFile {
        format: BUNDLE_FORMAT.to_owned(),
        version: BUNDLE_VERSION,
        bundle_id: B64.encode(header.bundle_id),
        epoch: header.epoch,
        keyset_generation: header.keyset_generation,
        revision: header.revision,
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
    };
    let body =
        toml::to_string_pretty(&file).map_err(|_| SyncError::Crypto("bundle encoding failed"))?;
    let text = format!("{FILE_COMMENT}{body}");
    if text.len() > MAX_BUNDLE_BYTES {
        return Err(SyncError::TooLarge {
            what: WHAT,
            max: MAX_BUNDLE_BYTES,
        });
    }
    let opened = OpenedBundle {
        header,
        roster,
        data_key,
        keys,
        fingerprint: crypto::sha256(text.as_bytes()),
        slots,
    };
    Ok((text, opened))
}

/// A passphrase slot for `header` wrapping `data_key` (fresh slot id and
/// nonce).
fn passphrase_slot(
    header: &BundleHeader,
    passphrase: &SecretString,
    data_key: &DataKey,
    bounds: &KdfBounds,
) -> Result<Slot, SyncError> {
    let kek = crypto::derive_kek(
        passphrase,
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
/// epoch 1. The passphrase must meet the minimum strength.
pub(crate) fn create(
    passphrase: &SecretString,
    first_device: RosterDevice,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    passphrase::enforce(passphrase)?;
    bounds.check(&params)?;
    let header = BundleHeader {
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
        Roster {
            devices: vec![first_device],
        },
        data_key,
    )
}

/// New passphrase for an opened bundle: same data key, new salt and slot,
/// `params` (may raise the cost), keyset generation and revision + 1. Used
/// by "change" (after [`open`] with the old passphrase) and by "reset"
/// from a device that holds the data key (owner decision #8; the UI asks
/// for an extra confirmation and the other devices get a blocking alert).
pub(crate) fn rewrap(
    opened: &OpenedBundle,
    new_passphrase: &SecretString,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    passphrase::enforce(new_passphrase)?;
    bounds.check(&params)?;
    let next = |n: u64| n.checked_add(1).ok_or_else(|| corrupt("counter exhausted"));
    let header = BundleHeader {
        keyset_generation: next(opened.header.keyset_generation)?,
        revision: next(opened.header.revision)?,
        kdf: params,
        salt: crypto::random_array()?,
        ..opened.header.clone()
    };
    let data_key = DataKey::from_bytes(opened.data_key.bytes())?;
    let slot = passphrase_slot(&header, new_passphrase, &data_key, bounds)?;
    // S3 device slots bind the old generation in their AAD: dropped here,
    // re-created by their devices.
    render(header, vec![slot], opened.roster.clone(), data_key)
}

/// "Change passphrase": opens with the old one, then [`rewrap`].
pub(crate) fn change_passphrase(
    text: &str,
    old: &SecretString,
    new: &SecretString,
    params: KdfParams,
    bounds: &KdfBounds,
) -> Result<(String, OpenedBundle), SyncError> {
    // Refuse a weak new passphrase before paying for the old derivation.
    passphrase::enforce(new)?;
    let opened = open(text, old, bounds)?;
    rewrap(&opened, new, params, bounds)
}

/// A new roster (device joined, renamed or forgotten): revision + 1, key
/// slots kept verbatim. Needs only the data key.
pub(crate) fn with_roster(
    opened: &OpenedBundle,
    roster: Roster,
) -> Result<(String, OpenedBundle), SyncError> {
    let header = BundleHeader {
        revision: opened
            .header
            .revision
            .checked_add(1)
            .ok_or_else(|| corrupt("counter exhausted"))?,
        ..opened.header.clone()
    };
    render(
        header,
        opened.slots.clone(),
        roster,
        DataKey::from_bytes(opened.data_key.bytes())?,
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::crypto::tests::derivations;
    use super::super::passphrase::tests::STRONG;
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

    pub(crate) fn new_bundle() -> (String, OpenedBundle) {
        create(
            &pass(STRONG),
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
        assert_eq!(opened.fingerprint, crypto::sha256(text.as_bytes()));
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
        for (version, expected) in [(2u32, 2u32), (0, 0), (99, 99)] {
            let altered = edit(&text, |f| f.version = version);
            assert_eq!(
                open_test(&altered, STRONG).expect_err("version"),
                SyncError::UnsupportedVersion {
                    what: WHAT,
                    found: expected,
                    supported: 1
                }
            );
        }
        // A v2 file with fields v1 does not know still reports the version.
        let v2 = format!("{}\nfuture_field = 1\n", edit(&text, |f| f.version = 2));
        assert!(matches!(
            open_test(&v2, STRONG),
            Err(SyncError::UnsupportedVersion { found: 2, .. })
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
                &pass(STRONG),
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
                &pass("hunter2"),
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
                &pass("password1"),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
        assert_eq!(derivations(), after_create, "refused before deriving");
        assert!(matches!(
            rewrap(
                &opened,
                &pass("short"),
                KdfBounds::TEST_PARAMS,
                &KdfBounds::TEST
            ),
            Err(SyncError::PassphraseWeak { .. })
        ));
    }

    #[test]
    fn change_passphrase_rewraps_the_same_data_key() {
        let (text, created) = new_bundle();
        let new = "zoom zone zippy zipper zigzag zesty";
        let (changed, opened) = change_passphrase(
            &text,
            &pass(STRONG),
            &pass(new),
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
                &pass(new),
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
            &pass("zoom zone zippy zipper zigzag zesty"),
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
        let new = "zoom zone zippy zipper zigzag zesty";
        let (reset, opened) = rewrap(
            &holder,
            &pass(new),
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
        let many = Roster {
            devices: (0..=MAX_ROSTER_DEVICES)
                .map(|i| RosterDevice {
                    id: DeviceId::from_bytes([u8::try_from(i).expect("u8"); ID_LEN]),
                    name: DeviceName::parse("d").expect("name"),
                    added_at: 0,
                    revoked: false,
                })
                .collect(),
        };
        assert!(matches!(
            with_roster(&created, many),
            Err(SyncError::TooLarge { .. })
        ));
        // The largest roster still fits the file cap.
        let full = Roster {
            devices: (0..MAX_ROSTER_DEVICES)
                .map(|i| RosterDevice {
                    id: DeviceId::from_bytes([u8::try_from(i).expect("u8"); ID_LEN]),
                    name: DeviceName::parse(&"n".repeat(64)).expect("name"),
                    added_at: i64::MAX,
                    revoked: true,
                })
                .collect(),
        };
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
