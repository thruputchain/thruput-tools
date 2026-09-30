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
        hasher.update(b"hogsim nonce namespace v1");
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

#[derive(Debug, thiserror::Error)]
pub enum NonceError {
    #[error("nonce namespace {value} exceeds the high-32-bit namespace limit")]
    NamespaceTooLarge { value: u64 },
}
