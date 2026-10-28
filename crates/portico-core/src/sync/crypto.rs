//! Cryptographic primitives of the encrypted folder sync (S1a).
//!
//! ```text
//! passphrase ──Argon2id(salt 16 B, m, t, p)──► ikm (32 B)
//!            ──HKDF-SHA256(salt = bundle_id, info = "portico-sync/1 kek")──► KEK
//! KEK ──XChaCha20-Poly1305 (AAD = slot AAD)──► wraps DK (32 random bytes per epoch)
//! DK ──HKDF-SHA256(salt = bundle_id ‖ epoch_be32, info = "portico-sync/1 <name>")──►
//!     snapshot | secret | header | compare | roster | local | rotate
//! ```
//!
//! - **Epoch commitment and lineage** (S2a, `docs/SYNC.md` §13.2): the
//!   `rotate` subkey `K_rot_e` only authenticates the key lineage. The
//!   commitment `C_e = HMAC(K_rot_e, canonical("epoch", bundle_id, e))` is
//!   the same for every holder of `DK_e`, stable across revisions and
//!   passphrase changes; a link `e → e + 1` is proved by
//!   `HMAC(K_rot_e, canonical("lineage", bundle_id, e, C_e, e + 1, C_{e+1}))`:
//!   only a holder of `DK_e` can start epoch `e + 1`.
//!
//! - **KDF bounds** are sync-specific ([`KdfBounds::PRODUCTION`]: default
//!   256 MiB / t=3 / p=1, floor 128 MiB / t=3 / p=1, ceiling 512 MiB / t=6 /
//!   p=4) and are checked **before** anything is allocated. The Argon2
//!   memory is then reserved with `try_reserve_exact`, so a request the
//!   machine cannot serve is an error ([`SyncError::KdfMemory`]), never an
//!   abort, and it is zeroized after use.
//! - **Subkeys** are independent HKDF outputs with distinct `info` labels: a
//!   key is never used for two purposes (AEAD of snapshots vs roster vs
//!   local replica, MAC of the header, comparison HMAC, inner secret AEAD).
//! - **AEAD**: XChaCha20-Poly1305 under a fresh random 24-byte nonce per
//!   seal (no counter to persist, so a crash cannot reuse a nonce).
//! - **Canonical encoding** ([`Canonical`]): every AAD and MAC input is a
//!   sequence of `u64be length ‖ bytes` fields after a length-prefixed
//!   domain and label, so two different inputs never encode alike.
//! - **MAC** compare is constant time (`subtle`).
//! - **Padding**: plaintexts are padded with spaces to a power-of-two bucket
//!   ([`bucket_len`]) so the folder learns only log2 of the size.
//!
//! Keys live in [`Zeroizing`] arrays inside types whose `Debug` is
//! redacted; every function here is pure (no I/O).

use argon2::{Algorithm, Argon2, Block, Version};
use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use super::SyncError;

/// Length of every symmetric key.
pub(crate) const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce length.
pub(crate) const NONCE_LEN: usize = 24;
/// Poly1305 tag length.
pub(crate) const TAG_LEN: usize = 16;
/// Argon2 salt length (written and accepted).
pub(crate) const SALT_LEN: usize = 16;
/// Bundle, device and slot ids.
pub(crate) const ID_LEN: usize = 16;
/// HMAC-SHA256 tag length.
pub(crate) const MAC_LEN: usize = 32;

/// Domain of every label, AAD and MAC input (format generation 1).
const DOMAIN: &[u8] = b"portico-sync/1";

/// Smallest padding bucket of snapshots and replicas.
pub(crate) const MIN_BUCKET: usize = 16 * 1024;
/// Largest padded plaintext (a snapshot file is capped at 8 MiB of base64).
pub(crate) const MAX_BUCKET: usize = 4 * 1024 * 1024;

/// Argon2id cost parameters of a sync bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KdfParams {
    /// Memory, in KiB.
    pub(crate) memory_kib: u32,
    /// Passes over memory.
    pub(crate) iterations: u32,
    /// Lanes.
    pub(crate) parallelism: u32,
}

impl KdfParams {
    /// New bundles: 256 MiB, 3 passes, 1 lane (about one second). Four
    /// times the vault's cost: the folder is exposed to offline attackers
    /// by construction.
    pub(crate) const DEFAULT: Self = Self {
        memory_kib: 256 * 1024,
        iterations: 3,
        parallelism: 1,
    };
}

/// Accepted range of [`KdfParams`], checked before any allocation. Not
/// shared with the vault (`vault::KdfBounds`): the threat model differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KdfBounds {
    pub(crate) min: KdfParams,
    pub(crate) max: KdfParams,
}

impl KdfBounds {
    /// Floor 128 MiB / t=3 / p=1 (a hostile `bundle.toml` cannot weaken the
    /// derivation); ceiling 512 MiB / t=6 / p=4 (nor freeze or swap the
    /// machine).
    pub(crate) const PRODUCTION: Self = Self {
        min: KdfParams {
            memory_kib: 128 * 1024,
            iterations: 3,
            parallelism: 1,
        },
        max: KdfParams {
            memory_kib: 512 * 1024,
            iterations: 6,
            parallelism: 4,
        },
    };

    /// Cheap bounds for tests (64 KiB, 1 pass): same code path, fast.
    #[cfg(test)]
    pub(crate) const TEST: Self = Self {
        min: KdfParams {
            memory_kib: 64,
            iterations: 1,
            parallelism: 1,
        },
        max: KdfParams {
            memory_kib: 1024,
            iterations: 2,
            parallelism: 4,
        },
    };

    /// Parameters of new bundles under these bounds.
    #[cfg(test)]
    pub(crate) const TEST_PARAMS: KdfParams = KdfParams {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    };

    pub(crate) fn check(&self, params: &KdfParams) -> Result<(), SyncError> {
        let fields = [
            (
                "memory_kib",
                params.memory_kib,
                self.min.memory_kib,
                self.max.memory_kib,
            ),
            (
                "iterations",
                params.iterations,
                self.min.iterations,
                self.max.iterations,
            ),
            (
                "parallelism",
                params.parallelism,
                self.min.parallelism,
                self.max.parallelism,
            ),
        ];
        for (param, value, min, max) in fields {
            if !(min..=max).contains(&value) {
                return Err(SyncError::KdfOutOfBounds {
                    param,
                    value,
                    min,
                    max,
                });
            }
        }
        Ok(())
    }
}

macro_rules! redacted_key {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        pub(crate) struct $name(Zeroizing<[u8; KEY_LEN]>);

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }

        impl $name {
            pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
                &self.0
            }
        }
    };
}

redacted_key!(
    /// Key-encryption key derived from the passphrase; wraps the data key.
    Kek
);
redacted_key!(
    /// Random data key of one epoch. Remembered in the system keychain by
    /// the engine (S1b), never written to a file in clear.
    DataKey
);
redacted_key!(
    /// One purpose-bound subkey of the data key.
    Subkey
);

impl DataKey {
    /// A fresh random data key.
    pub(crate) fn generate() -> Result<Self, SyncError> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        fill_random(&mut *key)?;
        Ok(Self(key))
    }

    /// Rebuilds a data key from bytes the caller already holds in a
    /// zeroizing buffer (the keychain copy, an unwrapped slot).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, SyncError> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        if bytes.len() != KEY_LEN {
            return Err(SyncError::Crypto("a data key has 32 bytes"));
        }
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    #[cfg(test)]
    pub(crate) fn duplicate(&self) -> Self {
        Self(Zeroizing::new(*self.0))
    }
}

/// The purpose-bound subkeys of one `(bundle, epoch, data key)`.
#[derive(Debug)]
pub(crate) struct Subkeys {
    /// AEAD of device snapshots.
    pub(crate) snapshot: Subkey,
    /// Inner AEAD of each synced secret (S2).
    // Derived now so the key schedule is fixed; first used by S2.
    #[allow(dead_code)]
    pub(crate) secret: Subkey,
    /// HMAC of `bundle.toml`.
    pub(crate) header: Subkey,
    /// HMAC of local secret values for change detection (S2).
    // Derived now so the key schedule is fixed; first used by S2.
    #[allow(dead_code)]
    pub(crate) compare: Subkey,
    /// AEAD of the roster inside `bundle.toml`.
    pub(crate) roster: Subkey,
    /// AEAD of this device's local replica.
    pub(crate) local: Subkey,
    /// Epoch commitment and lineage proofs (S2a): authenticates links of
    /// the key lineage, never encrypts anything.
    pub(crate) rotate: Subkey,
}

/// HKDF labels of the subkeys, all distinct (tested).
pub(crate) const SUBKEY_LABELS: [&str; 7] = [
    "portico-sync/1 snapshot",
    "portico-sync/1 secret",
    "portico-sync/1 header",
    "portico-sync/1 compare",
    "portico-sync/1 roster",
    "portico-sync/1 local",
    "portico-sync/1 rotate",
];
const KEK_LABEL: &str = "portico-sync/1 kek";

/// Passphrase → KEK. Checks `params` against `bounds` first, then
/// allocates the Argon2 memory fallibly; nothing is allocated for
/// out-of-bounds parameters.
pub(crate) fn derive_kek(
    passphrase: &SecretString,
    params: &KdfParams,
    salt: &[u8; SALT_LEN],
    bundle_id: &[u8; ID_LEN],
    bounds: &KdfBounds,
) -> Result<Kek, SyncError> {
    bounds.check(params)?;
    #[cfg(test)]
    tests::DERIVATIONS.with(|count| count.set(count.get().saturating_add(1)));
    let argon_params = argon2::Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        Some(KEY_LEN),
    )
    // Inside the bounds but refused by Argon2 (memory < 8 × lanes).
    .map_err(|_| SyncError::Crypto("invalid key derivation parameters"))?;
    let block_count = argon_params.block_count();
    // Memory limit (S1a review finding 6): `try_reserve_exact` only fails
    // when the allocator refuses. Under Linux overcommit it usually
    // succeeds and the pages are committed while Argon2 fills them, so a
    // machine short of memory may swap or meet the OOM killer instead of
    // getting `KdfMemory`. What bounds the damage is the ceiling checked
    // above (512 MiB in total, whatever the lanes): a hostile
    // `bundle.toml` cannot ask for more. No runtime probing of free memory
    // (racy and platform specific).
    let mut memory: Zeroizing<Vec<Block>> = Zeroizing::new(Vec::new());
    memory
        .try_reserve_exact(block_count)
        .map_err(|_| SyncError::KdfMemory {
            memory_kib: params.memory_kib,
        })?;
    // Within the reserved capacity: no reallocation.
    memory.resize(block_count, Block::new());
    let mut ikm = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params)
        .hash_password_into_with_memory(
            passphrase.expose_secret().as_bytes(),
            salt,
            &mut *ikm,
            &mut **memory,
        )
        .map_err(|_| SyncError::Crypto("key derivation failed"))?;
    let mut kek = Zeroizing::new([0u8; KEY_LEN]);
    hkdf_expand(&*ikm, bundle_id, KEK_LABEL, &mut kek)?;
    Ok(Kek(kek))
}

/// DK → the seven subkeys of `(bundle_id, epoch)`.
pub(crate) fn derive_subkeys(
    data_key: &DataKey,
    bundle_id: &[u8; ID_LEN],
    epoch: u32,
) -> Result<Subkeys, SyncError> {
    let mut salt = [0u8; ID_LEN + 4];
    let (id_part, epoch_part) = salt.split_at_mut(ID_LEN);
    id_part.copy_from_slice(bundle_id);
    epoch_part.copy_from_slice(&epoch.to_be_bytes());
    let derive = |label: &str| -> Result<Subkey, SyncError> {
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        hkdf_expand(data_key.bytes(), &salt, label, &mut key)?;
        Ok(Subkey(key))
    };
    let [snapshot, secret, header, compare, roster, local, rotate] = SUBKEY_LABELS;
    Ok(Subkeys {
        snapshot: derive(snapshot)?,
        secret: derive(secret)?,
        header: derive(header)?,
        compare: derive(compare)?,
        roster: derive(roster)?,
        local: derive(local)?,
        rotate: derive(rotate)?,
    })
}

impl Subkey {
    /// A copy (the rotate subkey is kept in the lineage memory).
    pub(crate) fn duplicate(&self) -> Self {
        Self(Zeroizing::new(*self.0))
    }
}

/// 32 key bytes that travel inside sealed JSON (the `K_rot` of the
/// lineage: `lineage_keys` in `bundle.toml`, the lineage memory of the
/// replica). Base64 on the wire, encoded through a stack buffer and decoded
/// straight into a zeroizing array: no intermediate `String` keeps a copy.
/// `Debug` is redacted; equality is constant time.
#[derive(Clone)]
pub(crate) struct KeyBytes(Zeroizing<[u8; KEY_LEN]>);

impl KeyBytes {
    pub(crate) fn of(key: &Subkey) -> Self {
        Self(Zeroizing::new(*key.bytes()))
    }

    pub(crate) fn subkey(&self) -> Subkey {
        Subkey(Zeroizing::new(*self.0))
    }
}

impl std::fmt::Debug for KeyBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyBytes(<redacted>)")
    }
}

impl PartialEq for KeyBytes {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.0.as_slice().ct_eq(other.0.as_slice()))
    }
}

impl Eq for KeyBytes {}

/// Base64 length of 32 bytes.
const KEY_B64_LEN: usize = 44;

impl serde::Serialize for KeyBytes {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        use serde::ser::Error as _;
        let mut text = Zeroizing::new([0u8; KEY_B64_LEN]);
        let len = base64::engine::general_purpose::STANDARD
            .encode_slice(*self.0, &mut *text)
            .map_err(|_| S::Error::custom("key encoding"))?;
        let encoded = text
            .get(..len)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .ok_or_else(|| S::Error::custom("key encoding"))?;
        serializer.serialize_str(encoded)
    }
}

impl<'de> serde::Deserialize<'de> for KeyBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = KeyBytes;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("32 key bytes in base64")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<KeyBytes, E> {
                use base64::Engine as _;
                if text.len() != KEY_B64_LEN {
                    return Err(E::custom("bad key length"));
                }
                // 44 base64 characters decode to at most 33 bytes.
                let mut buffer = Zeroizing::new([0u8; KEY_LEN + 1]);
                let len = base64::engine::general_purpose::STANDARD
                    .decode_slice(text, &mut *buffer)
                    .map_err(|_| E::custom("bad key encoding"))?;
                let bytes = buffer
                    .get(..len)
                    .filter(|bytes| bytes.len() == KEY_LEN)
                    .ok_or_else(|| E::custom("bad key length"))?;
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                key.copy_from_slice(bytes);
                Ok(KeyBytes(key))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// Commitment of epoch `epoch` (S2a, §13.2): `HMAC(K_rot_e,
/// canonical("epoch", bundle_id, e))`. Not secret: it names the epoch's
/// lineage without revealing anything about its keys.
pub(crate) fn epoch_commitment(
    rotate: &Subkey,
    bundle_id: &[u8; ID_LEN],
    epoch: u32,
) -> Result<[u8; MAC_LEN], SyncError> {
    let mut input = Canonical::new("epoch");
    input.bytes(bundle_id).u32(epoch);
    mac(rotate, input.finish())
}

/// The canonical input of the proof of link `epoch → epoch + 1`.
fn lineage_input(
    bundle_id: &[u8; ID_LEN],
    epoch: u32,
    commitment: &[u8; MAC_LEN],
    next_commitment: &[u8; MAC_LEN],
) -> Vec<u8> {
    let mut input = Canonical::new("lineage");
    input
        .bytes(bundle_id)
        .u32(epoch)
        .bytes(commitment)
        .u32(epoch.saturating_add(1))
        .bytes(next_commitment);
    input.finish().to_vec()
}

/// Proof that the holder of `DK_epoch` started epoch `epoch + 1` whose
/// commitment is `next_commitment` (only `K_rot_epoch` can make it).
pub(crate) fn lineage_proof(
    rotate: &Subkey,
    bundle_id: &[u8; ID_LEN],
    epoch: u32,
    commitment: &[u8; MAC_LEN],
    next_commitment: &[u8; MAC_LEN],
) -> Result<[u8; MAC_LEN], SyncError> {
    mac(
        rotate,
        &lineage_input(bundle_id, epoch, commitment, next_commitment),
    )
}

/// Constant-time check of a lineage proof.
pub(crate) fn verify_lineage_proof(
    rotate: &Subkey,
    bundle_id: &[u8; ID_LEN],
    epoch: u32,
    commitment: &[u8; MAC_LEN],
    next_commitment: &[u8; MAC_LEN],
    proof: &[u8],
) -> Result<bool, SyncError> {
    verify_mac(
        rotate,
        &lineage_input(bundle_id, epoch, commitment, next_commitment),
        proof,
    )
}

fn hkdf_expand(
    ikm: &[u8],
    salt: &[u8],
    label: &str,
    out: &mut Zeroizing<[u8; KEY_LEN]>,
) -> Result<(), SyncError> {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(label.as_bytes(), &mut **out)
        .map_err(|_| SyncError::Crypto("key expansion failed"))
}

/// Canonical, unambiguous byte encoding of AAD and MAC inputs: a
/// length-prefixed domain and label, then length-prefixed fields.
#[derive(Debug)]
pub(crate) struct Canonical(Vec<u8>);

impl Canonical {
    pub(crate) fn new(label: &str) -> Self {
        let mut canonical = Self(Vec::with_capacity(256));
        canonical.bytes(DOMAIN).bytes(label.as_bytes());
        canonical
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        // `usize` → `u64` cannot fail on supported targets; the fallback
        // keeps the encoding total without a panic path.
        let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.0.extend_from_slice(&len.to_be_bytes());
        self.0.extend_from_slice(bytes);
        self
    }

    pub(crate) fn str(&mut self, text: &str) -> &mut Self {
        self.bytes(text.as_bytes())
    }

    pub(crate) fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    pub(crate) fn finish(&self) -> &[u8] {
        &self.0
    }
}

/// The AEAD failed to open: wrong key, wrong AAD or altered bytes. The
/// caller maps it to the error its context calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AeadFailure;

/// Seals `plaintext` under a fresh random nonce. The buffer is encrypted
/// in place; it should have `TAG_LEN` spare capacity (no reallocation
/// then leaves a plaintext copy behind).
pub(crate) fn seal(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    mut plaintext: Zeroizing<Vec<u8>>,
) -> Result<([u8; NONCE_LEN], Vec<u8>), SyncError> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    XChaCha20Poly1305::new(key.into())
        .encrypt_in_place(&XNonce::from(nonce), aad, &mut *plaintext)
        .map_err(|_| SyncError::Crypto("encryption failed"))?;
    // The buffer now holds ciphertext only: hand it out without a copy.
    Ok((nonce, std::mem::take(&mut *plaintext)))
}

/// Opens a sealed message; the plaintext comes back zeroizing.
pub(crate) fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>, AeadFailure> {
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    XChaCha20Poly1305::new(key.into())
        .decrypt_in_place(&XNonce::from(*nonce), aad, &mut *buffer)
        .map_err(|_| AeadFailure)?;
    Ok(buffer)
}

/// HMAC-SHA256 of `message`.
pub(crate) fn mac(key: &Subkey, message: &[u8]) -> Result<[u8; MAC_LEN], SyncError> {
    let mut hmac = <Hmac<Sha256> as hmac::KeyInit>::new_from_slice(key.bytes())
        .map_err(|_| SyncError::Crypto("invalid MAC key"))?;
    hmac.update(message);
    Ok(hmac.finalize().into_bytes().into())
}

/// Constant-time verification of an HMAC-SHA256 tag (any length mismatch
/// fails).
pub(crate) fn verify_mac(key: &Subkey, message: &[u8], tag: &[u8]) -> Result<bool, SyncError> {
    let expected = mac(key, message)?;
    Ok(tag.len() == MAC_LEN && bool::from(expected.as_slice().ct_eq(tag)))
}

/// SHA-256 of bytes (wordlist pin, tests).
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Fingerprint of **authenticated** content for the high-water marks
/// (S1a review finding 1): SHA-256 over `domain` and `parts`, each
/// prefixed by its length (`u64be`), so two inputs never hash alike. The
/// callers pass only what the AEAD tag or the MAC covers (decoded fields),
/// never the file bytes: a comment, whitespace, key order or CRLF in the
/// TOML envelope must not change it.
pub(crate) fn content_fingerprint(domain: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in std::iter::once(domain.as_bytes()).chain(parts.iter().copied()) {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Counts the bytes a serializer writes, without storing them.
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Exact length of the JSON encoding of `value` (nothing is allocated).
pub(crate) fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> Result<usize, SyncError> {
    let mut counter = CountingWriter(0);
    serde_json::to_writer(&mut counter, value).map_err(|_| SyncError::Crypto("encoding failed"))?;
    Ok(counter.0)
}

/// JSON of `value` written straight into a zeroizing buffer sized up front
/// for its padding bucket and the AEAD tag (S1a review finding 6): the
/// buffer never reallocates, so no plaintext fragment is left behind in
/// freed memory. Padded with spaces like [`padded`].
pub(crate) fn padded_json<T: serde::Serialize + ?Sized>(
    value: &T,
    min_bucket: usize,
    what: &'static str,
) -> Result<Zeroizing<Vec<u8>>, SyncError> {
    let len = json_len(value)?;
    let bucket = bucket_len(len, min_bucket).ok_or(SyncError::TooLarge {
        what,
        max: MAX_BUCKET,
    })?;
    let mut buffer = Zeroizing::new(Vec::with_capacity(bucket.saturating_add(TAG_LEN)));
    serde_json::to_writer(&mut *buffer, value).map_err(|_| SyncError::Crypto("encoding failed"))?;
    // A serializer that is not deterministic would have outgrown the
    // reserved capacity: refuse rather than seal.
    if buffer.len() != len {
        return Err(SyncError::Crypto("encoding is not deterministic"));
    }
    buffer.resize(bucket, b' ');
    Ok(buffer)
}

/// Fills `out` from the operating system's random generator.
pub(crate) fn fill_random(out: &mut [u8]) -> Result<(), SyncError> {
    use rand::TryRng;
    rand::rngs::SysRng
        .try_fill_bytes(out)
        .map_err(|_| SyncError::Crypto("the system random generator failed"))
}

/// A random `[u8; N]` (ids, salts).
pub(crate) fn random_array<const N: usize>() -> Result<[u8; N], SyncError> {
    let mut out = [0u8; N];
    fill_random(&mut out)?;
    Ok(out)
}

/// Padded length of a `len`-byte plaintext: the next power of two, at
/// least `min_bucket`; `None` above [`MAX_BUCKET`].
pub(crate) fn bucket_len(len: usize, min_bucket: usize) -> Option<usize> {
    let bucket = len.max(min_bucket).checked_next_power_of_two()?;
    (bucket <= MAX_BUCKET).then_some(bucket)
}

/// Whether `len` is a bucket [`bucket_len`] can produce for `min_bucket`.
pub(crate) fn is_bucket(len: usize, min_bucket: usize) -> bool {
    len.is_power_of_two() && len >= min_bucket && len <= MAX_BUCKET
}

/// Copies `content` into a fresh zeroizing buffer padded with spaces to its
/// bucket, with room for the tag (sealing then never reallocates).
#[cfg(test)]
pub(crate) fn padded(
    content: &[u8],
    min_bucket: usize,
    what: &'static str,
) -> Result<Zeroizing<Vec<u8>>, SyncError> {
    let bucket = bucket_len(content.len(), min_bucket).ok_or(SyncError::TooLarge {
        what,
        max: MAX_BUCKET,
    })?;
    let mut buffer = Zeroizing::new(Vec::with_capacity(bucket.saturating_add(TAG_LEN)));
    buffer.extend_from_slice(content);
    buffer.resize(bucket, b' ');
    Ok(buffer)
}

/// Fixed-size array from a decoded field, or a `Corrupt` error.
pub(crate) fn exact<const N: usize>(
    bytes: &[u8],
    what: &'static str,
    reason: &'static str,
) -> Result<[u8; N], SyncError> {
    <[u8; N]>::try_from(bytes).map_err(|_| SyncError::Corrupt { what, reason })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;

    use super::*;

    thread_local! {
        /// Argon2 derivations started on this thread (bounds tests).
        pub(crate) static DERIVATIONS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn derivations() -> usize {
        DERIVATIONS.with(Cell::get)
    }

    fn pass(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    fn kek(passphrase: &str, salt: [u8; SALT_LEN], id: [u8; ID_LEN]) -> [u8; KEY_LEN] {
        *derive_kek(
            &pass(passphrase),
            &KdfBounds::TEST_PARAMS,
            &salt,
            &id,
            &KdfBounds::TEST,
        )
        .expect("derive")
        .bytes()
    }

    #[test]
    fn production_parameters_match_the_design() {
        assert_eq!(KdfParams::DEFAULT.memory_kib, 262_144);
        assert_eq!(KdfParams::DEFAULT.iterations, 3);
        assert_eq!(KdfParams::DEFAULT.parallelism, 1);
        let bounds = KdfBounds::PRODUCTION;
        assert_eq!(bounds.min.memory_kib, 131_072);
        assert_eq!(bounds.max.memory_kib, 524_288);
        assert_eq!((bounds.min.iterations, bounds.max.iterations), (3, 6));
        assert_eq!((bounds.min.parallelism, bounds.max.parallelism), (1, 4));
        bounds
            .check(&KdfParams::DEFAULT)
            .expect("default inside bounds");
        // Not the vault's bounds: those allow 64 MiB.
        assert!(bounds.min.memory_kib > crate::vault::KdfParams::DEFAULT.memory_kib);
    }

    #[test]
    fn kdf_bounds_refuse_below_floor_and_above_ceiling_before_any_allocation() {
        let hostile = [
            (
                KdfParams {
                    memory_kib: 128 * 1024 - 1,
                    ..KdfParams::DEFAULT
                },
                "memory_kib",
            ),
            (
                KdfParams {
                    memory_kib: 8,
                    ..KdfParams::DEFAULT
                },
                "memory_kib",
            ),
            (
                KdfParams {
                    memory_kib: 512 * 1024 + 1,
                    ..KdfParams::DEFAULT
                },
                "memory_kib",
            ),
            (
                KdfParams {
                    memory_kib: u32::MAX,
                    ..KdfParams::DEFAULT
                },
                "memory_kib",
            ),
            (
                KdfParams {
                    iterations: 2,
                    ..KdfParams::DEFAULT
                },
                "iterations",
            ),
            (
                KdfParams {
                    iterations: 7,
                    ..KdfParams::DEFAULT
                },
                "iterations",
            ),
            (
                KdfParams {
                    iterations: u32::MAX,
                    ..KdfParams::DEFAULT
                },
                "iterations",
            ),
            (
                KdfParams {
                    parallelism: 0,
                    ..KdfParams::DEFAULT
                },
                "parallelism",
            ),
            (
                KdfParams {
                    parallelism: 5,
                    ..KdfParams::DEFAULT
                },
                "parallelism",
            ),
        ];
        for (params, expected) in hostile {
            let before = derivations();
            let started = std::time::Instant::now();
            let error = derive_kek(
                &pass("x"),
                &params,
                &[0; SALT_LEN],
                &[0; ID_LEN],
                &KdfBounds::PRODUCTION,
            )
            .expect_err("out of bounds");
            assert!(
                matches!(error, SyncError::KdfOutOfBounds { param, .. } if param == expected),
                "{params:?} → {error:?}"
            );
            assert_eq!(derivations(), before, "no derivation started");
            assert!(started.elapsed() < std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn argon2_rejections_inside_bounds_are_errors_not_panics() {
        // Bounds that let through what Argon2 refuses (memory < 8 × lanes).
        let loose = KdfBounds {
            min: KdfParams {
                memory_kib: 1,
                iterations: 1,
                parallelism: 1,
            },
            max: KdfParams {
                memory_kib: 64,
                iterations: 1,
                parallelism: 64,
            },
        };
        let params = KdfParams {
            memory_kib: 8,
            iterations: 1,
            parallelism: 64,
        };
        let error = derive_kek(&pass("x"), &params, &[0; SALT_LEN], &[0; ID_LEN], &loose)
            .expect_err("argon2 refuses");
        assert_eq!(
            error,
            SyncError::Crypto("invalid key derivation parameters")
        );
    }

    #[test]
    fn kek_depends_on_passphrase_salt_and_bundle() {
        let base = kek("correct horse", [1; SALT_LEN], [2; ID_LEN]);
        assert_eq!(base, kek("correct horse", [1; SALT_LEN], [2; ID_LEN]));
        assert_ne!(base, kek("correct horsf", [1; SALT_LEN], [2; ID_LEN]));
        assert_ne!(base, kek("correct horse", [3; SALT_LEN], [2; ID_LEN]));
        assert_ne!(base, kek("correct horse", [1; SALT_LEN], [3; ID_LEN]));
    }

    #[test]
    fn hkdf_matches_rfc5869_test_case_1() {
        // RFC 5869 A.1 (SHA-256): checks the primitive and our wiring.
        let ikm = [0x0b_u8; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let mut okm = [0u8; 42];
        Hkdf::<Sha256>::new(Some(&salt), &ikm)
            .expand(&info, &mut okm)
            .expect("expand");
        let expected =
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";
        assert_eq!(hex(&okm), expected);
    }

    #[test]
    fn hmac_matches_rfc4231_test_case_2() {
        let key = Subkey(Zeroizing::new({
            let mut k = [0u8; KEY_LEN];
            k[..4].copy_from_slice(b"Jefe");
            k
        }));
        // RFC 4231 uses the 4-byte key "Jefe"; HMAC pads keys with zeros up
        // to the block size, so the zero-extended 32-byte key is identical.
        let tag = mac(&key, b"what do ya want for nothing?").expect("mac");
        assert_eq!(
            hex(&tag),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(verify_mac(&key, b"what do ya want for nothing?", &tag).expect("verify"));
        let mut altered = tag;
        altered[31] ^= 1;
        assert!(!verify_mac(&key, b"what do ya want for nothing?", &altered).expect("verify"));
        assert!(!verify_mac(&key, b"what do ya want for nothing?", &tag[..31]).expect("verify"));
        assert!(!verify_mac(&key, b"what do ya want for nothing!", &tag).expect("verify"));
    }

    #[test]
    fn xchacha_matches_the_draft_test_vector() {
        // draft-irtf-cfrg-xchacha-03, A.3.1.
        let key: [u8; KEY_LEN] = std::array::from_fn(|i| 0x80 + u8::try_from(i).expect("u8"));
        let nonce: [u8; NONCE_LEN] = std::array::from_fn(|i| 0x40 + u8::try_from(i).expect("u8"));
        let aad = unhex("50515253c0c1c2c3c4c5c6c7");
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let mut buffer = plaintext.to_vec();
        XChaCha20Poly1305::new((&key).into())
            .encrypt_in_place(&XNonce::from(nonce), &aad, &mut buffer)
            .expect("encrypt");
        let (ciphertext, tag) = buffer.split_at(plaintext.len());
        assert!(
            hex(ciphertext)
                .starts_with("bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb")
        );
        assert_eq!(hex(tag), "c0875924c1c7987947deafd8780acf49");
        let opened = open(&key, &nonce, &aad, &buffer).expect("open");
        assert_eq!(opened.as_slice(), plaintext);
    }

    #[test]
    fn seal_open_round_trip_and_every_alteration_fails() {
        let key = [7u8; KEY_LEN];
        let aad = b"aad";
        let (nonce, sealed) = seal(&key, aad, Zeroizing::new(b"hello".to_vec())).expect("seal");
        assert_eq!(sealed.len(), 5 + TAG_LEN);
        assert_eq!(
            open(&key, &nonce, aad, &sealed).expect("open").as_slice(),
            b"hello"
        );
        for i in 0..sealed.len() {
            let mut altered = sealed.clone();
            altered[i] ^= 0x01;
            assert_eq!(
                open(&key, &nonce, aad, &altered),
                Err(AeadFailure),
                "byte {i}"
            );
        }
        for i in 0..NONCE_LEN {
            let mut altered = nonce;
            altered[i] ^= 0x80;
            assert_eq!(open(&key, &altered, aad, &sealed), Err(AeadFailure));
        }
        assert_eq!(open(&key, &nonce, b"aae", &sealed), Err(AeadFailure));
        assert_eq!(open(&[8; KEY_LEN], &nonce, aad, &sealed), Err(AeadFailure));
        assert_eq!(
            open(&key, &nonce, aad, &sealed[..TAG_LEN - 1]),
            Err(AeadFailure)
        );
    }

    #[test]
    fn nonces_are_unique_across_ten_thousand_seals() {
        let key = [1u8; KEY_LEN];
        let mut seen = BTreeSet::new();
        for _ in 0..10_000 {
            let (nonce, _) = seal(&key, b"", Zeroizing::new(Vec::new())).expect("seal");
            assert!(seen.insert(nonce));
        }
    }

    #[test]
    fn subkeys_are_distinct_and_bound_to_bundle_and_epoch() {
        let labels: BTreeSet<&str> = SUBKEY_LABELS.into_iter().collect();
        assert_eq!(labels.len(), SUBKEY_LABELS.len());
        assert!(!labels.contains(KEK_LABEL));
        let dk = DataKey::from_bytes(&[9; KEY_LEN]).expect("dk");
        let keys = derive_subkeys(&dk, &[1; ID_LEN], 1).expect("subkeys");
        let all = [
            &keys.snapshot,
            &keys.secret,
            &keys.header,
            &keys.compare,
            &keys.roster,
            &keys.local,
            &keys.rotate,
        ];
        let distinct: BTreeSet<[u8; KEY_LEN]> = all.iter().map(|k| *k.bytes()).collect();
        assert_eq!(distinct.len(), 7, "the rotate subkey is distinct (S2a)");
        assert!(!distinct.contains(dk.bytes()));
        let other_epoch = derive_subkeys(&dk, &[1; ID_LEN], 2).expect("subkeys");
        let other_bundle = derive_subkeys(&dk, &[2; ID_LEN], 1).expect("subkeys");
        assert_ne!(keys.snapshot.bytes(), other_epoch.snapshot.bytes());
        assert_ne!(keys.snapshot.bytes(), other_bundle.snapshot.bytes());
    }

    #[test]
    fn epoch_commitment_and_lineage_proof_are_bound() {
        let dk = DataKey::from_bytes(&[9; KEY_LEN]).expect("dk");
        let keys = derive_subkeys(&dk, &[1; ID_LEN], 1).expect("subkeys");
        let commit = epoch_commitment(&keys.rotate, &[1; ID_LEN], 1).expect("c");
        assert_eq!(
            commit,
            epoch_commitment(&keys.rotate, &[1; ID_LEN], 1).expect("c"),
            "deterministic for every holder of the data key"
        );
        assert_ne!(
            commit,
            epoch_commitment(&keys.rotate, &[1; ID_LEN], 2).expect("c")
        );
        assert_ne!(
            commit,
            epoch_commitment(&keys.rotate, &[2; ID_LEN], 1).expect("c")
        );
        let next = [7u8; MAC_LEN];
        let proof = lineage_proof(&keys.rotate, &[1; ID_LEN], 1, &commit, &next).expect("p");
        assert!(
            verify_lineage_proof(&keys.rotate, &[1; ID_LEN], 1, &commit, &next, &proof).expect("v")
        );
        let other = derive_subkeys(&dk, &[1; ID_LEN], 2).expect("subkeys");
        assert!(
            !verify_lineage_proof(&other.rotate, &[1; ID_LEN], 1, &commit, &next, &proof)
                .expect("v"),
            "only K_rot of that epoch proves its link"
        );
        assert!(
            !verify_lineage_proof(
                &keys.rotate,
                &[1; ID_LEN],
                1,
                &commit,
                &[8; MAC_LEN],
                &proof
            )
            .expect("v")
        );
    }

    #[test]
    fn canonical_encoding_is_unambiguous() {
        let encode = |label: &str, fields: &[&[u8]]| {
            let mut c = Canonical::new(label);
            for f in fields {
                c.bytes(f);
            }
            c.finish().to_vec()
        };
        // Moving bytes across a field boundary changes the encoding.
        assert_ne!(encode("x", &[b"ab", b"c"]), encode("x", &[b"a", b"bc"]));
        assert_ne!(encode("x", &[b"", b"a"]), encode("x", &[b"a", b""]));
        assert_ne!(encode("ab", &[b"c"]), encode("a", &[b"bc"]));
        assert_ne!(encode("x", &[b"a"]), encode("x", &[b"a", b""]));
        let mut c = Canonical::new("t");
        c.u32(1).u64(2).str("s");
        assert_eq!(
            c.finish().len(),
            (8 + DOMAIN.len()) + (8 + 1) + (8 + 4) + (8 + 8) + (8 + 1)
        );
    }

    #[test]
    fn padding_buckets_are_powers_of_two_with_a_floor_and_a_ceiling() {
        assert_eq!(bucket_len(0, MIN_BUCKET), Some(16 * 1024));
        assert_eq!(bucket_len(1, MIN_BUCKET), Some(16 * 1024));
        assert_eq!(bucket_len(16 * 1024, MIN_BUCKET), Some(16 * 1024));
        assert_eq!(bucket_len(16 * 1024 + 1, MIN_BUCKET), Some(32 * 1024));
        assert_eq!(bucket_len(100_000, MIN_BUCKET), Some(128 * 1024));
        assert_eq!(bucket_len(MAX_BUCKET, MIN_BUCKET), Some(MAX_BUCKET));
        assert_eq!(bucket_len(MAX_BUCKET + 1, MIN_BUCKET), None);
        assert_eq!(bucket_len(usize::MAX, MIN_BUCKET), None);
        assert_eq!(bucket_len(33, 64), Some(64));
        let buffer = padded(b"{}", MIN_BUCKET, "snapshot").expect("pad");
        assert_eq!(buffer.len(), MIN_BUCKET);
        assert!(buffer.capacity() >= MIN_BUCKET + TAG_LEN);
        assert!(buffer[2..].iter().all(|&b| b == b' '));
        assert!(is_bucket(MIN_BUCKET, MIN_BUCKET));
        assert!(!is_bucket(MIN_BUCKET + 1, MIN_BUCKET));
        assert!(!is_bucket(MIN_BUCKET / 2, MIN_BUCKET));
        assert!(!is_bucket(MAX_BUCKET * 2, MIN_BUCKET));
        assert!(matches!(
            padded(&vec![0; MAX_BUCKET + 1], MIN_BUCKET, "snapshot"),
            Err(SyncError::TooLarge { .. })
        ));
    }

    #[test]
    fn key_debug_is_redacted() {
        let dk = DataKey::from_bytes(&[0x41; KEY_LEN]).expect("dk");
        let keys = derive_subkeys(&dk, &[1; ID_LEN], 1).expect("subkeys");
        let text = format!("{dk:?} {keys:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("65, 65"), "{text}");
        assert!(DataKey::from_bytes(&[0; 31]).is_err());
    }

    pub(crate) fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
            .collect()
    }
}
