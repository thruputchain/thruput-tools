//! Deterministic local-dev account signer derivation.
//!
//! These keys are intentionally well-known and only suitable for local dev
//! nodes. Backend bootstrap owns wallet addresses; this module only derives
//! signer identity.

use sha2::{Digest, Sha256};

use crate::LocalSigner;

/// Default seed for deterministic local-dev account signers.
pub const DEFAULT_DEV_ACCOUNT_SEED: &str = "groundhog-dev-accounts";

/// Default number of local-dev accounts started by `groundhog node`.
pub const DEFAULT_DEV_ACCOUNT_COUNT: usize = 10;

/// Default storage-token funding target per local-dev account.
///
/// Sized for wallet bootstrap plus several deploy/call cycles under the
/// default hogd-dev cost model; this is not a live-balance promise.
pub const DEFAULT_DEV_ACCOUNT_STORAGE_TOKENS: u64 = 1_000_000_000;

/// Derive the 32-byte Ed25519 secret-key seed for one local-dev account.
pub fn derive_dev_account_secret(seed: &str, index: usize) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(b":");
    hasher.update(format!("account-{index}").as_bytes());
    hasher.finalize().into()
}

/// Derive a local signer for one local-dev account.
pub fn derive_dev_account_signer(seed: &str, index: usize) -> LocalSigner {
    LocalSigner::from_secret_key(derive_dev_account_secret(seed, index))
}

/// Derive a local signer using the default local-dev seed.
pub fn derive_default_dev_account_signer(index: usize) -> LocalSigner {
    derive_dev_account_signer(DEFAULT_DEV_ACCOUNT_SEED, index)
}
