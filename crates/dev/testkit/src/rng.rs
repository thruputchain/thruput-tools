//! Deterministic per-agent randomness.
//!
//! Each agent gets its own stream derived from the scenario seed and the
//! agent's identity, so adding or reordering agents does not perturb another
//! agent's sequence. The generator is a small `SplitMix64`, kept in-crate so
//! the byte-for-byte sequence stays stable across dependency updates.

/// A deterministic pseudo-random stream for one agent.
#[derive(Debug, Clone)]
pub struct AgentRng {
    state: u64,
}

impl AgentRng {
    /// Derive an agent's stream from the scenario `seed` plus the agent's
    /// `index` and `name`.
    pub(crate) fn derive(seed: &str, index: usize, name: &str) -> Self {
        // FNV-1a over the three inputs, then used to seed `SplitMix64`.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut mix = |bytes: &[u8]| {
            for &byte in bytes {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        mix(seed.as_bytes());
        mix(&index.to_le_bytes());
        mix(name.as_bytes());
        Self::from_u64(hash)
    }

    /// Create a stream directly from a 64-bit seed.
    pub fn from_u64(seed: u64) -> Self {
        // Avoid an all-zero state, which reads like an uninitialized generator.
        Self { state: seed | 1 }
    }

    /// Next 64-bit value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniformly pick an index in `0..len`, or `None` when `len` is zero.
    pub fn choose_index(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let len_u64 = u64::try_from(len).unwrap_or(u64::MAX);
        let pick = self.next_u64() % len_u64;
        Some(usize::try_from(pick).unwrap_or(len - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRng;

    #[test]
    fn derivation_is_deterministic_and_agent_specific() {
        let mut first = AgentRng::derive("seed-v1", 0, "alice");
        let mut again = AgentRng::derive("seed-v1", 0, "alice");
        let first_seq: Vec<u64> = (0..4).map(|_| first.next_u64()).collect();
        let again_seq: Vec<u64> = (0..4).map(|_| again.next_u64()).collect();
        assert_eq!(
            first_seq, again_seq,
            "same seed and identity yields the same stream"
        );

        let mut other = AgentRng::derive("seed-v1", 1, "bob");
        let other_seq: Vec<u64> = (0..4).map(|_| other.next_u64()).collect();
        assert_ne!(
            first_seq, other_seq,
            "a different agent yields a different stream"
        );
    }

    #[test]
    fn choose_index_stays_in_range() {
        let mut rng = AgentRng::from_u64(42);
        for _ in 0..100 {
            let index = rng.choose_index(5).expect("nonzero length yields an index");
            assert!(index < 5);
        }
        assert_eq!(rng.choose_index(0), None);
    }
}
