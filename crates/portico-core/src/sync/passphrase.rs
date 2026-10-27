//! Sync passphrase: generator and enforced strength minimum (S1a, owner
//! decision #11, security review F18).
//!
//! The sync folder is exposed to offline attackers by construction, so the
//! core refuses weak passphrases ([`super::SyncError::PassphraseWeak`]) at
//! creation, change and reset, whatever the UI did.
//!
//! - **Generator**: [`GENERATED_WORDS`] words drawn uniformly (rejection
//!   sampling over the OS random generator) from the EFF large wordlist
//!   (7776 words, ≈12.9 bits each, so ≈77.5 bits), separated by spaces.
//!   The list is embedded (`eff_large_wordlist.txt`, 62 KB, words only;
//!   © Electronic Frontier Foundation, CC BY 3.0 US,
//!   <https://www.eff.org/dice>); a test pins its SHA-256.
//! - **Estimate**: when every word is from the list, the diceware model
//!   `distinct words × log2(7776)` (the attacker knows the list: a
//!   general-purpose estimator would overrate four list words); otherwise
//!   the `zxcvbn` estimate on the first [`ESTIMATE_MAX_CHARS`] characters
//!   (which bounds its cost).
//! - **Minimum**: [`MIN_ESTIMATED_BITS`] bits, which [`MIN_DICEWARE_WORDS`]
//!   distinct list words reach on their own.
//!
//! Nothing here logs or returns the passphrase; [`PassphraseStrength`]
//! holds numbers only.

use std::sync::LazyLock;

use secrecy::{ExposeSecret, SecretString};

use super::SyncError;

/// Words of a generated passphrase.
pub const GENERATED_WORDS: usize = 6;
/// Minimum estimated strength of a sync passphrase, in bits.
pub const MIN_ESTIMATED_BITS: u32 = 60;
/// Distinct wordlist words that are always enough (5 × 12.9 ≈ 64.6 bits).
pub const MIN_DICEWARE_WORDS: usize = 5;
/// `zxcvbn` only looks at this many characters (its cost grows fast).
pub const ESTIMATE_MAX_CHARS: usize = 100;

const WORDLIST_TEXT: &str = include_str!("eff_large_wordlist.txt");
/// log2(7776).
const BITS_PER_WORD: f64 = 12.924_812_503_605_78;
/// log2(10): zxcvbn reports log10 of the guesses.
const BITS_PER_DECIMAL_DIGIT: f64 = std::f64::consts::LOG2_10;

/// The EFF large wordlist, sorted (byte order), one word per entry.
static WORDLIST: LazyLock<Vec<&'static str>> = LazyLock::new(|| WORDLIST_TEXT.lines().collect());

/// Strength of a candidate passphrase, as the UI meter shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassphraseStrength {
    /// Estimated entropy, in bits (rounded down).
    pub estimated_bits: u32,
    /// Distinct EFF wordlist words, when the passphrase is made only of
    /// them (separated by whitespace); 0 otherwise.
    pub diceware_words: usize,
    /// Meets the minimum: the core accepts it.
    pub acceptable: bool,
}

/// A fresh passphrase of [`GENERATED_WORDS`] random words.
pub fn generate_passphrase() -> crate::Result<SecretString> {
    Ok(generate_with(&mut os_u32)?)
}

fn os_u32() -> Result<u32, SyncError> {
    let mut bytes = [0u8; 4];
    super::crypto::fill_random(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn generate_with(
    next: &mut impl FnMut() -> Result<u32, SyncError>,
) -> Result<SecretString, SyncError> {
    let longest = WORDLIST.iter().map(|w| w.len()).max().unwrap_or(0);
    // Sized up front: no reallocation leaves a partial copy behind.
    let mut phrase =
        String::with_capacity(GENERATED_WORDS.saturating_mul(longest.saturating_add(1)));
    for i in 0..GENERATED_WORDS {
        let index = uniform_index(WORDLIST.len(), next)?;
        let word = WORDLIST
            .get(index)
            .ok_or(SyncError::Crypto("wordlist index"))?;
        if i > 0 {
            phrase.push(' ');
        }
        phrase.push_str(word);
    }
    Ok(SecretString::from(phrase))
}

/// A uniform index in `0..n` by rejection sampling over 32-bit draws (no
/// modulo bias).
fn uniform_index(
    n: usize,
    next: &mut impl FnMut() -> Result<u32, SyncError>,
) -> Result<usize, SyncError> {
    let n = u32::try_from(n).map_err(|_| SyncError::Crypto("range too large"))?;
    if n == 0 {
        return Err(SyncError::Crypto("empty range"));
    }
    // The largest multiple of n that fits: draws at or above it are retried.
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let draw = next()?;
        if draw < zone {
            return usize::try_from(draw % n).map_err(|_| SyncError::Crypto("index"));
        }
    }
}

/// Distinct list words when `passphrase` is only list words, else 0.
fn diceware_words(passphrase: &str) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for token in passphrase.split_whitespace() {
        if WORDLIST.binary_search(&token).is_err() {
            return 0;
        }
        if !seen.contains(&token) {
            seen.push(token);
        }
    }
    seen.len()
}

/// Estimates the strength of `passphrase` (see the module doc).
pub fn assess_passphrase(passphrase: &SecretString) -> PassphraseStrength {
    let text = passphrase.expose_secret();
    let prefix_end = text
        .char_indices()
        .nth(ESTIMATE_MAX_CHARS)
        .map_or(text.len(), |(index, _)| index);
    let prefix = text.get(..prefix_end).unwrap_or(text);
    let words = diceware_words(text);
    let bits = if words > 0 {
        // Word counts are tiny: the conversion is exact.
        f64::from(u32::try_from(words).unwrap_or(u32::MAX)) * BITS_PER_WORD
    } else if prefix.is_empty() {
        0.0
    } else {
        zxcvbn::zxcvbn(prefix, &[]).guesses_log10() * BITS_PER_DECIMAL_DIGIT
    };
    // Clamped to [0, 1024] first, so the cast cannot truncate or wrap.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let estimated_bits = if bits.is_finite() {
        bits.clamp(0.0, 1024.0).floor() as u32
    } else {
        0
    };
    PassphraseStrength {
        estimated_bits,
        diceware_words: words,
        acceptable: estimated_bits >= MIN_ESTIMATED_BITS || words >= MIN_DICEWARE_WORDS,
    }
}

/// Refuses a passphrase below the minimum (create, change, reset).
pub(crate) fn enforce(passphrase: &SecretString) -> Result<(), SyncError> {
    let strength = assess_passphrase(passphrase);
    if strength.acceptable {
        return Ok(());
    }
    Err(SyncError::PassphraseWeak {
        estimated_bits: strength.estimated_bits,
        required_bits: MIN_ESTIMATED_BITS,
        required_words: MIN_DICEWARE_WORDS,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fixed, acceptable passphrase for tests (six list words).
    const WORDLIST_LEN: usize = 7776;

    pub(crate) const STRONG: &str = "abacus abdomen abdominal abide abiding ability";

    fn pass(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    #[test]
    fn embedded_wordlist_is_the_eff_large_list() {
        assert_eq!(WORDLIST.len(), WORDLIST_LEN);
        assert!(WORDLIST.windows(2).all(|w| w[0] < w[1]), "sorted, unique");
        assert!(
            WORDLIST
                .iter()
                .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b == b'-'))
        );
        assert_eq!(WORDLIST.first(), Some(&"abacus"));
        assert_eq!(WORDLIST.last(), Some(&"zoom"));
        // SHA-256 of the words-only file (the dice column of the original
        // eff_large_wordlist.txt removed).
        assert_eq!(
            super::super::crypto::tests::hex(&super::super::crypto::sha256(
                WORDLIST_TEXT.as_bytes()
            )),
            "6d557f0693958fb5e650b68b5bee585eb82cf4da32965505c789e924743bc522"
        );
        assert!((BITS_PER_WORD - (7776f64).log2()).abs() < 1e-9);
    }

    #[test]
    fn generated_passphrases_have_six_list_words_and_pass_the_minimum() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..50 {
            let phrase = generate_passphrase().expect("generate");
            let text = phrase.expose_secret();
            let words: Vec<&str> = text.split(' ').collect();
            assert_eq!(words.len(), GENERATED_WORDS);
            assert!(words.iter().all(|w| WORDLIST.binary_search(w).is_ok()));
            let strength = assess_passphrase(&phrase);
            // Repeated words are rare but possible: still ≥ 5 distinct
            // almost always; the bits estimate covers the rest.
            assert!(strength.acceptable, "{strength:?}");
            enforce(&phrase).expect("acceptable");
            seen.insert(text.to_owned());
        }
        assert_eq!(seen.len(), 50, "no repeats");
    }

    #[test]
    fn uniform_index_rejects_the_biased_zone() {
        // n = 7776: zone = u32::MAX - (u32::MAX % 7776).
        let n = 7776u32;
        let zone = u32::MAX - (u32::MAX % n);
        let mut draws = vec![u32::MAX, zone, 7777].into_iter();
        let mut next = || draws.next().ok_or(SyncError::Crypto("exhausted"));
        assert_eq!(uniform_index(7776, &mut next), Ok(1));
        let mut limited = {
            let mut calls = 0;
            move || {
                calls += 1;
                if calls > 100 {
                    Err(SyncError::Crypto("exhausted"))
                } else {
                    Ok(u32::MAX)
                }
            }
        };
        assert!(
            uniform_index(7776, &mut limited).is_err(),
            "never accepts a biased draw"
        );
        assert!(uniform_index(0, &mut || Ok(0)).is_err());
        let mut failing = || Err(SyncError::Crypto("rng"));
        assert!(generate_with(&mut failing).is_err());
    }

    #[test]
    fn uniform_index_distribution_is_flat() {
        // Chi-square sanity on the real OS generator: 16 buckets, 32 000
        // draws; the 99.99 % quantile of χ²(15) is ≈ 44.3.
        let buckets = 16usize;
        let draws = 32_000usize;
        let mut counts = vec![0u32; buckets];
        for _ in 0..draws {
            let index = uniform_index(buckets, &mut os_u32).expect("draw");
            counts[index] += 1;
        }
        let expected = f64::from(u32::try_from(draws / buckets).expect("fits"));
        let chi2: f64 = counts
            .iter()
            .map(|&c| (f64::from(c) - expected).powi(2) / expected)
            .sum();
        assert!(chi2 < 44.3, "chi2 = {chi2}, counts = {counts:?}");
        // Every word index is reachable at the ends of the range.
        let mut low = || Ok(0);
        assert_eq!(uniform_index(WORDLIST_LEN, &mut low), Ok(0));
        let mut high = || Ok(u32::MAX - (u32::MAX % 7776) - 1);
        assert_eq!(uniform_index(WORDLIST_LEN, &mut high), Ok(7775));
    }

    #[test]
    fn strength_threshold() {
        for weak in [
            "",
            "password",
            "hunter2",
            "correct horse",
            "Tr0ub4dor&3",
            "abacus abacus abacus abacus abacus abacus",
            "abacus abdomen abdominal abide",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let strength = assess_passphrase(&pass(weak));
            assert!(!strength.acceptable, "{weak:?} → {strength:?}");
            let error = enforce(&pass(weak)).expect_err("weak");
            assert!(matches!(
                error,
                SyncError::PassphraseWeak {
                    required_bits: 60,
                    ..
                }
            ));
            assert!(!error.to_string().contains(weak) || weak.is_empty());
        }
        // Five distinct list words are enough by rule.
        let five = assess_passphrase(&pass("abacus abdomen abdominal abide abiding"));
        assert_eq!(five.diceware_words, 5);
        assert!(five.acceptable);
        assert!(five.estimated_bits >= 64);
        assert!(assess_passphrase(&pass(STRONG)).acceptable);
        // A long random-looking passphrase passes on the zxcvbn estimate.
        let random = assess_passphrase(&pass("vT9#qLm2!xR7&wZp4@Kd8^"));
        assert_eq!(random.diceware_words, 0);
        assert!(random.acceptable, "{random:?}");
        // Mixed list / non-list words count no diceware words.
        assert_eq!(diceware_words("abacus zoom Zoom"), 0);
        assert_eq!(diceware_words("abacus\tzoom  abacus"), 2);
    }

    #[test]
    fn estimate_cost_is_bounded_on_long_inputs() {
        let long = "x7!".repeat(5_000);
        let started = std::time::Instant::now();
        let strength = assess_passphrase(&pass(&long));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let _ = strength;
        let unicode = "é".repeat(500);
        let _ = assess_passphrase(&pass(&unicode));
    }

    #[test]
    fn strength_debug_holds_no_passphrase() {
        let strength = assess_passphrase(&pass("hunter2-secret-value"));
        assert!(!format!("{strength:?}").contains("hunter2"));
    }
}
