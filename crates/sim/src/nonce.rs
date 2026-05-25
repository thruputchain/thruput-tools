use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonceNamespace(u32);

impl NonceNamespace {
    pub fn new(value: u64) -> Result<Self, NonceError> {
        let value = u32::try_from(value).map_err(|_| NonceError::NamespaceTooLarge { value })?;
        Ok(Self(value))
    }

    pub fn generate(seed: &str, scenario_name: &str, run_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"hoglab nonce namespace v1");
        hasher.update(seed.as_bytes());
        hasher.update(b"\0");
        hasher.update(scenario_name.as_bytes());
        hasher.update(b"\0");
        hasher.update(run_id.as_bytes());
        let digest = hasher.finalize();
        Self(u32::from_be_bytes([
            digest[0], digest[1], digest[2], digest[3],
        ]))
    }

    pub fn get(self) -> u32 {
        self.0
    }

    pub fn make_nonce(self, sequence: u32) -> u64 {
        (u64::from(self.0) << 32) | u64::from(sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonceSequence {
    namespace: NonceNamespace,
    next: u32,
}

impl NonceSequence {
    pub fn new(namespace: NonceNamespace) -> Self {
        Self { namespace, next: 0 }
    }

    pub fn next_nonce(&mut self) -> Result<u64, NonceError> {
        let sequence = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(NonceError::SequenceExhausted)?;
        Ok(self.namespace.make_nonce(sequence))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NonceError {
    #[error("nonce namespace {value} exceeds the high-32-bit namespace limit")]
    NamespaceTooLarge { value: u64 },
    #[error("nonce sequence exhausted the low 32 bits")]
    SequenceExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_uses_high_32_bits() {
        let namespace = NonceNamespace::new(0x1234_5678).unwrap();

        assert_eq!(namespace.make_nonce(0), 0x1234_5678_0000_0000);
        assert_eq!(namespace.make_nonce(0x90ab_cdef), 0x1234_5678_90ab_cdef);
    }

    #[test]
    fn rejects_namespace_that_does_not_fit_high_32_bits() {
        let err = NonceNamespace::new(u64::from(u32::MAX) + 1)
            .expect_err("namespace should be limited to u32");

        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn generated_namespace_is_stable_for_same_run_identity() {
        let a = NonceNamespace::generate("seed", "scenario", "run-1");
        let b = NonceNamespace::generate("seed", "scenario", "run-1");
        let c = NonceNamespace::generate("seed", "scenario", "run-2");

        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
