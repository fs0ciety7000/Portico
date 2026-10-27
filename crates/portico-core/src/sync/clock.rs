//! Causality for the folder sync (S1a): device ids, dots, version vectors,
//! multi-value registers and the local dot issuer.
//!
//! - A **dot** `(device, counter)` names one write. Counters are issued by
//!   [`LocalClock`] and are never reused: a dot always means the same value.
//! - A **version vector** maps each device to the highest counter seen; it
//!   is the causal context of a whole state ("every dot ≤ n of device d is
//!   known"). Full-state snapshots keep contexts contiguous, so a version
//!   vector is exact (no dot cloud is needed).
//! - A **multi-value register** ([`MvRegister`]) holds the surviving dots of
//!   one record key with their values. The merge keeps a dot present on
//!   both sides, or present on one side and not covered by the other
//!   side's context; a dot covered by a context but absent from that state
//!   was overwritten (or collected) there and is dropped. Equal values are
//!   grouped for display ([`MvRegister::siblings`]) without losing dots,
//!   which keeps the merge commutative, associative and idempotent.
//! - **No reuse after restore** (security review F5): [`LocalClock`] refuses
//!   to issue dots once it has observed a context or a published clock of
//!   its own device that is ahead of it ([`SyncError::RestoredState`]); the
//!   engine then re-joins under a new [`DeviceId`].
//!
//! The wall clock appears only as a display stamp (`at`, unix seconds).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::SyncError;
use super::crypto::{self, ID_LEN};

/// Random 128-bit device identity (32 lowercase hex characters on disk).
/// Not secret: it names a file in the sync folder.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId([u8; ID_LEN]);

impl DeviceId {
    /// A fresh random id.
    pub(crate) fn generate() -> Result<Self, SyncError> {
        crypto::random_array().map(Self)
    }

    pub(crate) const fn from_bytes(bytes: [u8; ID_LEN]) -> Self {
        Self(bytes)
    }

    pub(crate) fn bytes(&self) -> &[u8; ID_LEN] {
        &self.0
    }

    /// Parses exactly 32 lowercase hex characters.
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 2 * ID_LEN {
            return None;
        }
        let mut bytes = [0u8; ID_LEN];
        let (pairs, _) = text.as_bytes().as_chunks::<2>();
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            *byte = (digit(*high)? << 4) | digit(*low)?;
        }
        Some(Self(bytes))
    }

    /// The 8-character prefix shown next to device names (F16).
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(8);
        text
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({self})")
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| serde::de::Error::custom("invalid device id"))
    }
}

/// Most characters of a device name.
pub(crate) const MAX_DEVICE_NAME_CHARS: usize = 64;

/// A device's display name (security review F16): 1 to 64 characters after
/// trimming, no control, bidi or invisible character. Display only, always
/// shown with the id prefix ([`DeviceName::label`]): two devices may share
/// a name, and a name proves nothing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct DeviceName(String);

impl DeviceName {
    pub(crate) fn parse(text: &str) -> Result<Self, SyncError> {
        let name = text.trim();
        let invalid = |reason| Err(SyncError::InvalidDeviceName { reason });
        if name.is_empty() {
            return invalid("empty name");
        }
        if name.chars().count() > MAX_DEVICE_NAME_CHARS {
            return invalid("longer than 64 characters");
        }
        if name
            .chars()
            .any(|c| c.is_control() || crate::snippets::is_hidden_format(c))
        {
            return invalid("contains a control, bidi or invisible character");
        }
        Ok(Self(name.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// `"MacBook Air (3f9c0ae1)"`.
    pub(crate) fn label(&self, id: &DeviceId) -> String {
        format!("{} ({})", self.0, id.short())
    }
}

impl<'de> Deserialize<'de> for DeviceName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&text).map_err(|_| serde::de::Error::custom("invalid device name"))
    }
}

/// One write: the `counter`-th dot issued by `device` (counters start at 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct Dot {
    #[serde(rename = "d")]
    pub(crate) device: DeviceId,
    #[serde(rename = "c")]
    pub(crate) counter: u64,
}

/// Causal context: device → highest contiguous counter known.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct VersionVector(BTreeMap<DeviceId, u64>);

impl VersionVector {
    pub(crate) fn get(&self, device: &DeviceId) -> u64 {
        self.0.get(device).copied().unwrap_or(0)
    }

    pub(crate) fn covers(&self, dot: &Dot) -> bool {
        dot.counter <= self.get(&dot.device)
    }

    /// Records `dot` (contexts stay contiguous: dots are issued in order).
    pub(crate) fn insert(&mut self, dot: Dot) {
        let entry = self.0.entry(dot.device).or_insert(0);
        *entry = (*entry).max(dot.counter);
    }

    /// Pointwise maximum.
    pub(crate) fn join(&mut self, other: &Self) {
        for (device, &counter) in &other.0 {
            let entry = self.0.entry(*device).or_insert(0);
            *entry = (*entry).max(counter);
        }
    }

    /// Pointwise minimum over `devices` (the causally stable part of
    /// several contexts, for tombstone collection).
    pub(crate) fn meet<'a>(contexts: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut contexts = contexts.into_iter();
        let Some(first) = contexts.next() else {
            return Self::default();
        };
        let mut stable = first.clone();
        for context in contexts {
            stable.0.retain(|device, counter| {
                *counter = (*counter).min(context.get(device));
                *counter > 0
            });
        }
        stable
    }

    /// `self ≥ other` pointwise.
    pub(crate) fn dominates(&self, other: &Self) -> bool {
        other
            .0
            .iter()
            .all(|(device, &counter)| self.get(device) >= counter)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&DeviceId, &u64)> {
        self.0.iter()
    }
}

/// A surviving write of a register: its display stamp and value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry<V> {
    /// Unix seconds on the writing device. Display only.
    pub(crate) at: i64,
    pub(crate) value: V,
}

/// Multi-value register over the dots that survive in one state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MvRegister<V> {
    entries: BTreeMap<Dot, Entry<V>>,
}

impl<V> Default for MvRegister<V> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }
}

/// One distinct value of a register and every surviving dot that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sibling<'a, V> {
    pub(crate) value: &'a V,
    /// Ascending; never empty.
    pub(crate) dots: Vec<Dot>,
    /// Most recent display stamp among `dots`.
    pub(crate) at: i64,
}

impl<V: Clone + PartialEq> MvRegister<V> {
    pub(crate) fn from_entries(entries: BTreeMap<Dot, Entry<V>>) -> Self {
        Self { entries }
    }

    pub(crate) fn entries(&self) -> &BTreeMap<Dot, Entry<V>> {
        &self.entries
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A local write: the new dot replaces every surviving dot (all of them
    /// are in the writer's context, so they are overwritten everywhere).
    pub(crate) fn write(&mut self, dot: Dot, at: i64, value: V) {
        self.entries.clear();
        self.entries.insert(dot, Entry { at, value });
    }

    /// Merges `other` (with its state's context) into `self` (with its
    /// own context). Contexts are joined by the caller, once per state.
    ///
    /// Returns how many dots both sides hold with **different** values.
    /// Honest devices never produce one (a dot names one write); only a
    /// device holding the key and misbehaving can. The local value is kept
    /// and the caller raises an alert.
    pub(crate) fn merge(
        &mut self,
        own_context: &VersionVector,
        other: &Self,
        other_context: &VersionVector,
    ) -> usize {
        self.entries
            .retain(|dot, _| other.entries.contains_key(dot) || !other_context.covers(dot));
        let mut divergent: usize = 0;
        for (dot, entry) in &other.entries {
            match self.entries.get(dot) {
                Some(own) if own.value != entry.value => divergent = divergent.saturating_add(1),
                None if !own_context.covers(dot) => {
                    self.entries.insert(*dot, entry.clone());
                }
                // Same write on both sides, or overwritten here.
                _ => {}
            }
        }
        divergent
    }

    /// Distinct values, each with its dots (equal values collapse), in
    /// order of their smallest dot.
    pub(crate) fn siblings(&self) -> Vec<Sibling<'_, V>> {
        let mut siblings: Vec<Sibling<'_, V>> = Vec::new();
        for (dot, entry) in &self.entries {
            if let Some(sibling) = siblings.iter_mut().find(|s| *s.value == entry.value) {
                sibling.dots.push(*dot);
                sibling.at = sibling.at.max(entry.at);
            } else {
                siblings.push(Sibling {
                    value: &entry.value,
                    dots: vec![*dot],
                    at: entry.at,
                });
            }
        }
        siblings
    }

    /// More than one distinct value survives: a conflict for the user.
    pub(crate) fn is_conflict(&self) -> bool {
        let mut values = self.entries.values().map(|e| &e.value);
        let Some(first) = values.next() else {
            return false;
        };
        values.any(|v| v != first)
    }

    /// The value when there is exactly one distinct value.
    pub(crate) fn single(&self) -> Option<&V> {
        let first = &self.entries.values().next()?.value;
        (!self.is_conflict()).then_some(first)
    }

    /// Drops every dot covered by `stable` when **all** surviving values
    /// satisfy `collectable` (tombstones). Returns whether the register
    /// is now empty. A conflict that involves a live value is never
    /// collected: that would resolve it silently.
    pub(crate) fn collect_if(
        &mut self,
        stable: &VersionVector,
        collectable: impl Fn(&V) -> bool,
    ) -> bool {
        let all_collectable = self
            .entries
            .iter()
            .all(|(dot, entry)| collectable(&entry.value) && stable.covers(dot));
        if all_collectable {
            self.entries.clear();
        }
        self.entries.is_empty()
    }
}

/// Issues this device's dots. The counter is persisted (inside the sealed
/// replica, S1b) **before** any dot is published, so it never goes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalClock {
    device: DeviceId,
    counter: u64,
    /// Set once a newer state of this device was observed: no more dots.
    restored: bool,
}

impl LocalClock {
    pub(crate) fn new(device: DeviceId, counter: u64) -> Self {
        Self {
            device,
            counter,
            restored: false,
        }
    }

    pub(crate) fn device(&self) -> DeviceId {
        self.device
    }

    pub(crate) fn counter(&self) -> u64 {
        self.counter
    }

    /// Checks a context received from the folder (any snapshot): if it
    /// knows a dot of this device beyond the local counter, the local
    /// state was restored or cloned. The clock then refuses every new dot.
    pub(crate) fn observe(&mut self, context: &VersionVector) -> Result<(), SyncError> {
        self.observe_counter(context.get(&self.device))
    }

    /// Same check against the `clock` published in this device's own
    /// snapshot file.
    pub(crate) fn observe_counter(&mut self, published: u64) -> Result<(), SyncError> {
        if published > self.counter {
            self.restored = true;
        }
        self.ensure_not_restored()
    }

    fn ensure_not_restored(&self) -> Result<(), SyncError> {
        if self.restored {
            return Err(SyncError::RestoredState {
                device: self.device.short(),
            });
        }
        Ok(())
    }

    /// The next dot. Fails after a restore was observed, or on overflow.
    pub(crate) fn next(&mut self) -> Result<Dot, SyncError> {
        self.ensure_not_restored()?;
        self.counter = self.counter.checked_add(1).ok_or(SyncError::Corrupt {
            what: "local state",
            reason: "dot counter exhausted",
        })?;
        Ok(Dot {
            device: self.device,
            counter: self.counter,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    pub(crate) fn device(n: u8) -> DeviceId {
        DeviceId::from_bytes([n; ID_LEN])
    }

    fn dot(n: u8, counter: u64) -> Dot {
        Dot {
            device: device(n),
            counter,
        }
    }

    #[test]
    fn device_ids_round_trip_through_hex_and_serde() {
        let id = DeviceId::generate().expect("id");
        let text = id.to_string();
        assert_eq!(text.len(), 32);
        assert_eq!(DeviceId::from_hex(&text), Some(id));
        assert_eq!(id.short(), &text[..8]);
        assert_eq!(
            serde_json::to_string(&id).expect("ser"),
            format!("\"{text}\"")
        );
        let back: DeviceId = serde_json::from_str(&format!("\"{text}\"")).expect("de");
        assert_eq!(back, id);
        for bad in [
            "",
            "00",
            &text.to_uppercase(),
            &format!("{text}0"),
            &"g".repeat(32),
        ] {
            assert_eq!(DeviceId::from_hex(bad), None, "{bad}");
        }
        assert!(serde_json::from_str::<DeviceId>("\"zz\"").is_err());
        assert_ne!(DeviceId::generate().expect("id"), id);
    }

    #[test]
    fn device_names_are_validated() {
        let name = DeviceName::parse("  MacBook Air  ").expect("valid");
        assert_eq!(name.as_str(), "MacBook Air");
        assert_eq!(name.label(&device(0xab)), "MacBook Air (abababab)");
        DeviceName::parse(&"é".repeat(64)).expect("64 chars");
        for bad in [
            "",
            "   ",
            &"x".repeat(65),
            "evil\u{202e}gpj.exe",
            "zero\u{200b}width",
            "bell\u{7}",
            "line\nbreak",
            "esc\u{1b}[31m",
        ] {
            assert!(
                matches!(
                    DeviceName::parse(bad),
                    Err(SyncError::InvalidDeviceName { .. })
                ),
                "{bad:?}"
            );
        }
        assert!(serde_json::from_str::<DeviceName>("\"a\\u202eb\"").is_err());
        assert_eq!(
            serde_json::from_str::<DeviceName>("\"Desk\"")
                .expect("de")
                .as_str(),
            "Desk"
        );
    }

    #[test]
    fn version_vector_join_meet_cover() {
        let mut a = VersionVector::default();
        a.insert(dot(1, 3));
        a.insert(dot(2, 1));
        a.insert(dot(1, 2)); // never goes back
        assert_eq!(a.get(&device(1)), 3);
        let mut b = VersionVector::default();
        b.insert(dot(2, 5));
        b.insert(dot(3, 1));
        let mut joined = a.clone();
        joined.join(&b);
        assert_eq!(
            (
                joined.get(&device(1)),
                joined.get(&device(2)),
                joined.get(&device(3))
            ),
            (3, 5, 1)
        );
        assert!(joined.dominates(&a) && joined.dominates(&b));
        assert!(!a.dominates(&b));
        let stable = VersionVector::meet([&a, &b]);
        assert_eq!(stable.get(&device(1)), 0);
        assert_eq!(stable.get(&device(2)), 1);
        assert_eq!(stable.len(), 1);
        assert_eq!(VersionVector::meet([]), VersionVector::default());
        assert!(a.covers(&dot(1, 3)) && !a.covers(&dot(1, 4)) && !a.covers(&dot(9, 1)));
    }

    #[test]
    fn concurrent_writes_become_siblings_and_equal_values_collapse() {
        let mut ctx_a = VersionVector::default();
        let mut a = MvRegister::default();
        a.write(dot(1, 1), 10, "x");
        ctx_a.insert(dot(1, 1));
        let mut ctx_b = VersionVector::default();
        let mut b = MvRegister::default();
        b.write(dot(2, 1), 20, "y");
        ctx_b.insert(dot(2, 1));
        let mut merged = a.clone();
        merged.merge(&ctx_a, &b, &ctx_b);
        assert!(merged.is_conflict());
        assert_eq!(merged.siblings().len(), 2);
        assert_eq!(merged.single(), None);

        let mut c = MvRegister::default();
        c.write(dot(3, 1), 30, "x");
        let mut ctx_c = VersionVector::default();
        ctx_c.insert(dot(3, 1));
        let mut equal = a.clone();
        equal.merge(&ctx_a, &c, &ctx_c);
        assert!(!equal.is_conflict());
        assert_eq!(equal.single(), Some(&"x"));
        let siblings = equal.siblings();
        assert_eq!(siblings.len(), 1);
        assert_eq!(siblings[0].dots, vec![dot(1, 1), dot(3, 1)]);
        assert_eq!(siblings[0].at, 30);
    }

    #[test]
    fn a_write_that_saw_the_siblings_resolves_them_everywhere() {
        let mut ctx = VersionVector::default();
        let mut reg = MvRegister::default();
        reg.write(dot(1, 1), 0, "x");
        ctx.insert(dot(1, 1));
        let mut other = MvRegister::default();
        other.write(dot(2, 1), 0, "y");
        let mut other_ctx = VersionVector::default();
        other_ctx.insert(dot(2, 1));
        reg.merge(&ctx, &other, &other_ctx);
        ctx.join(&other_ctx);
        assert!(reg.is_conflict());
        reg.write(dot(1, 2), 0, "z");
        ctx.insert(dot(1, 2));
        // The old side still holds y: it is covered by our context.
        let mut stale = other.clone();
        stale.merge(&other_ctx, &reg, &ctx);
        assert_eq!(stale.single(), Some(&"z"));
        reg.merge(&ctx, &other, &other_ctx);
        assert_eq!(reg.single(), Some(&"z"));
    }

    #[test]
    fn collection_never_resolves_a_conflict_silently() {
        let mut reg = MvRegister::default();
        reg.entries.insert(
            dot(1, 1),
            Entry {
                at: 0,
                value: "tomb",
            },
        );
        reg.entries.insert(
            dot(2, 1),
            Entry {
                at: 0,
                value: "live",
            },
        );
        let mut stable = VersionVector::default();
        stable.insert(dot(1, 1));
        stable.insert(dot(2, 1));
        assert!(!reg.collect_if(&stable, |v| *v == "tomb"));
        assert_eq!(reg.entries().len(), 2);
        let mut tomb = MvRegister::default();
        tomb.write(dot(1, 2), 0, "tomb");
        assert!(
            !tomb.collect_if(&stable, |v| *v == "tomb"),
            "not stable yet"
        );
        stable.insert(dot(1, 2));
        assert!(tomb.collect_if(&stable, |v| *v == "tomb"));
    }

    #[test]
    fn restored_data_dir_never_reuses_a_dot() {
        let me = device(1);
        let mut clock = LocalClock::new(me, 0);
        let mut issued = BTreeSet::new();
        for _ in 0..5 {
            assert!(issued.insert(clock.next().expect("dot")));
        }
        let mut published = VersionVector::default();
        published.insert(Dot {
            device: me,
            counter: clock.counter(),
        });
        // The data dir is restored from a backup taken at counter 3.
        let mut restored = LocalClock::new(me, 3);
        // Before looking at the folder it would reuse dot 4: the engine
        // must observe first (S1b does so at start and on every pass).
        assert!(matches!(
            restored.observe(&published),
            Err(SyncError::RestoredState { .. })
        ));
        assert!(matches!(
            restored.next(),
            Err(SyncError::RestoredState { .. })
        ));
        // Observing an older or equal context afterwards does not unlock it.
        assert!(restored.observe(&VersionVector::default()).is_err());
        assert!(restored.next().is_err());
        // Same through the published clock of the own snapshot file.
        let mut restored = LocalClock::new(me, 3);
        assert!(restored.observe_counter(5).is_err());
        assert!(restored.next().is_err());
        // Re-join under a new identity: its dots can never equal old ones.
        let mut rejoined = LocalClock::new(DeviceId::generate().expect("id"), 0);
        for _ in 0..10 {
            assert!(issued.insert(rejoined.next().expect("dot")));
        }
        // A context that is not ahead is fine.
        let mut current = LocalClock::new(me, 5);
        current.observe(&published).expect("in sync");
        current.observe_counter(5).expect("in sync");
        assert_eq!(current.next().expect("dot").counter, 6);
    }

    #[test]
    fn clock_overflow_is_an_error() {
        let mut clock = LocalClock::new(device(1), u64::MAX);
        assert!(matches!(clock.next(), Err(SyncError::Corrupt { .. })));
    }
}
