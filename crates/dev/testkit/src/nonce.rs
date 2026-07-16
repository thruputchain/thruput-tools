//! Transaction nonce namespaces.
//!
//! Every scenario derives a run-unique 32-bit namespace from its seed. Each
//! agent's account uses that namespace as the high 32 bits of its `u64` nonce
//! and its own per-account sequence as the low 32 bits, so concurrent agents
//! and reruns against a fresh target never collide.

use sha2::{Digest, Sha256};

/// The high 32 bits of an agent's transaction nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonceNamespace(u32);

impl NonceNamespace {
    /// Derive a deterministic namespace from a scenario seed.
    pub fn from_seed(seed: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"groundhog-testkit nonce namespace v1");
        hasher.update(seed.as_bytes());
        let digest = hasher.finalize();
        Self(u32::from_be_bytes([
            digest[0], digest[1], digest[2], digest[3],
        ]))
    }

    /// Combine this namespace with a per-account `sequence` into a full nonce.
    pub fn make_nonce(self, sequence: u32) -> u64 {
        (u64::from(self.0) << 32) | u64::from(sequence)
    }
}

#[cfg(test)]
mod tests {
    use super::NonceNamespace;

    #[test]
    fn nonce_packs_namespace_into_high_bits() {
        let namespace = NonceNamespace::from_seed("seed-v1");
        let high = namespace.make_nonce(0);
        assert_eq!(
            high & 0xffff_ffff,
            0,
            "sequence 0 leaves the low bits clear"
        );
        assert_eq!(namespace.make_nonce(0x90ab_cdef), high | 0x90ab_cdef);
    }

    #[test]
    fn derivation_is_deterministic_and_seed_specific() {
        assert_eq!(
            NonceNamespace::from_seed("seed-a"),
            NonceNamespace::from_seed("seed-a")
        );
        assert_ne!(
            NonceNamespace::from_seed("seed-a"),
            NonceNamespace::from_seed("seed-b")
        );
    }
}
