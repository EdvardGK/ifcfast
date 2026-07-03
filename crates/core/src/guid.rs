//! IFC compressed-GUID minting for the write axis (GH #133).
//!
//! `IfcGloballyUniqueId` packs a 128-bit UUID into 22 characters of a
//! base-64 alphabet, where the FIRST character carries only the top 2
//! bits (2 + 21×6 = 128) — so the first char is always `0`..`3`. A naive
//! "22 × 6-bit chars" encoder produces syntactically plausible but
//! spec-invalid ids that strict validators reject.
//!
//! Minting: fresh ids are derived from a splitmix64 stream seeded either
//! from ambient entropy (default) or an explicit caller seed (tests,
//! reproducible builds). Determinism is opt-in, NOT the default: two
//! independent runs minting the same id for semantically identical ops
//! becomes a real GlobalId collision the moment both outputs are merged
//! into a federation. Generated values get RFC-4122 v4 variant/version
//! bits so a minted id decompresses to an honest random UUID.

/// The IFC base-64 alphabet (differs from RFC 4648: `_` and `$` in the
/// last two slots).
const ALPHABET: &[u8; 64] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_$";

/// Encode a 128-bit value as a 22-char IFC compressed GUID.
pub fn encode_guid(v: u128) -> String {
    let mut out = String::with_capacity(22);
    // First char: top 2 bits only.
    out.push(ALPHABET[((v >> 126) & 0x3) as usize] as char);
    for i in 1..22 {
        let shift = 126 - 6 * i;
        out.push(ALPHABET[((v >> shift) & 0x3F) as usize] as char);
    }
    out
}

/// Decode a 22-char IFC compressed GUID back to its 128 bits. `None` on
/// bad length, a character outside the alphabet, or a first character
/// above `3` (which would overflow 128 bits).
pub fn decode_guid(s: &str) -> Option<u128> {
    let b = s.as_bytes();
    if b.len() != 22 {
        return None;
    }
    let mut v: u128 = 0;
    for (i, &c) in b.iter().enumerate() {
        let idx = ALPHABET.iter().position(|&a| a == c)? as u128;
        if i == 0 {
            if idx > 3 {
                return None;
            }
            v = idx;
        } else {
            v = (v << 6) | idx;
        }
    }
    Some(v)
}

/// splitmix64 — tiny, well-distributed PRNG step; no `rand` dependency.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// A GlobalId allocator for one write invocation.
pub struct GuidMinter {
    state: u64,
}

impl GuidMinter {
    /// `seed == None` (the default) mixes ambient entropy so independent
    /// invocations never repeat a stream; `Some(seed)` is the explicit
    /// reproducibility opt-in.
    pub fn new(seed: Option<u64>) -> GuidMinter {
        let state = match seed {
            Some(s) => s,
            None => {
                // Entropy without a rand dependency: RandomState's keys are
                // randomized per instance, and the current time decorrelates
                // processes further.
                use std::hash::{BuildHasher, Hasher};
                let h = std::collections::hash_map::RandomState::new()
                    .build_hasher()
                    .finish();
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                h ^ t.rotate_left(32)
            }
        };
        GuidMinter { state }
    }

    /// Mint a fresh GlobalId that `taken` does not already contain,
    /// advancing the stream past collisions.
    pub fn mint(&mut self, taken: &std::collections::HashSet<String>) -> String {
        loop {
            let hi = splitmix64(&mut self.state);
            let lo = splitmix64(&mut self.state);
            let mut v = ((hi as u128) << 64) | lo as u128;
            // RFC 4122 version 4 + variant bits, so the id decompresses to
            // an honest random UUID.
            v = (v & !(0xF000u128 << 48)) | (0x4000u128 << 48);
            v = (v & !(0xC0u128 << 56)) | (0x80u128 << 56);
            let s = encode_guid(v);
            if !taken.contains(&s) {
                return s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        for v in [
            0u128,
            1,
            u128::MAX,
            0x1234_5678_9ABC_DEF0_1234_5678_9ABC_DEF0,
        ] {
            assert_eq!(decode_guid(&encode_guid(v)), Some(v));
        }
    }

    #[test]
    fn first_char_carries_two_bits() {
        // u128::MAX starts with '3' (0b11), never a higher alphabet char.
        assert!(encode_guid(u128::MAX).starts_with('3'));
        assert_eq!(decode_guid("z234567890123456789012"), None); // first char > 3
    }

    #[test]
    fn known_vector() {
        // 0 encodes to all-'0' (alphabet position 0 everywhere).
        assert_eq!(encode_guid(0), "0000000000000000000000");
    }

    #[test]
    fn mint_avoids_taken_and_is_seeded() {
        let mut taken = std::collections::HashSet::new();
        let mut m1 = GuidMinter::new(Some(42));
        let a = m1.mint(&taken);
        taken.insert(a.clone());
        let b = m1.mint(&taken);
        assert_ne!(a, b);
        // Same seed reproduces the stream.
        let mut m2 = GuidMinter::new(Some(42));
        assert_eq!(m2.mint(&std::collections::HashSet::new()), a);
        // Minted ids are valid 22-char compressed GUIDs.
        assert!(decode_guid(&a).is_some());
    }
}
