//! Deterministic dev account derivation.

use groundhog_rpc_api_types::dev::DevAccount;
use groundhog_rpc_api_types::hex::{Hex0x, HexBytes};
use groundhog_signing::{DEFAULT_PUBKEY_ACCOUNT_NONCE, LocalSigner, PubkeyAccount, Signer};
use sha2::{Digest, Sha256};

/// Well-known seed for dev account derivation.
const DEV_SEED: &str = "groundhog-dev-accounts";

/// Default number of pre-funded accounts.
pub const DEFAULT_ACCOUNT_COUNT: usize = 10;

/// Default storage tokens per account.
pub const DEFAULT_STORAGE_TOKENS: u64 = 1000;

/// Derive a deterministic ed25519 keypair for dev account N.
pub fn derive_dev_signer(index: usize) -> LocalSigner {
    let mut hasher = Sha256::new();
    hasher.update(DEV_SEED.as_bytes());
    hasher.update(b":");
    hasher.update(format!("account-{index}").as_bytes());
    let secret: [u8; 32] = hasher.finalize().into();
    LocalSigner::from_secret_key(secret)
}

/// Derive all dev accounts.
pub fn derive_dev_accounts(count: usize, storage_tokens: u64) -> Vec<DevAccount> {
    (0..count)
        .map(|i| {
            let signer = derive_dev_signer(i);
            let pubkey = signer.public_key();
            let account = PubkeyAccount::new(pubkey, DEFAULT_PUBKEY_ACCOUNT_NONCE);
            let address = account
                .address()
                .expect("bug: dev account address derivation failed");

            DevAccount {
                address: Hex0x(address),
                public_key: HexBytes(signer.verifying_key_bytes().to_vec()),
                private_key: HexBytes(signer.to_keypair_bytes()[..32].to_vec()),
                storage_tokens,
            }
        })
        .collect()
}
