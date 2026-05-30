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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dev_account_derivation_is_stable() {
        let public_keys: Vec<_> = (0..DEFAULT_DEV_ACCOUNT_COUNT)
            .map(|index| {
                hex::encode(derive_default_dev_account_signer(index).verifying_key_bytes())
            })
            .collect();

        assert_eq!(
            public_keys,
            [
                "591ff2bdaaf5a0cbfd31f7b0f88c1ddee39fd9e106ba46303bde718459960633",
                "f5495b73debe0193757220005043944a3f03ff20cea7c4559db158eb1389e93f",
                "b8c09ac34f6c3ff58ddb549b19751d4e8b52f217fe6138f47322989c55a279a5",
                "8c8cb82ec23c847434193b00949c37d50313ef4e73e6dc4904028072a236dfbc",
                "45e85a804e73df05e59cdbcabf2252938b3b8b87a274ef60b2eff0b79cdd76dc",
                "178f7e68819b387699d2b5acfc8cda87de03ff92db1944f83a114a1946945d37",
                "7ea77f21cf1e56066f95d01a5bdd42d34087bd8a02261a0cbf67b2eed93393d0",
                "ee21e4725d8b38a54e42a0eb83782cdcfcaae89afb665b53c7a941a3835bccf0",
                "e4788cde106e2144b3d111af4adf56170a6e25d2636616f891f304af38397174",
                "404b21c4c455673bb0c841a55cf5f154aa646a83e0ea9722f9a87c64f1864564",
            ]
        );
    }
}
