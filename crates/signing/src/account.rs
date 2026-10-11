//! Pubkey-account helpers.

use groundhog_xdr::auth_claim::{AuthClaim, PubkeyAccountPreimage};
use groundhog_xdr::types::{ContractAddress, PublicKey};

/// Default nonce used for the first pubkey-account derived from a key.
pub const DEFAULT_PUBKEY_ACCOUNT_NONCE: u64 = 0;

/// A pubkey-backed account with the nonce used in address derivation.
#[derive(Clone, Debug)]
pub struct PubkeyAccount {
    /// The underlying public key.
    pub public_key: PublicKey,
    /// Nonce used in the pubkey-account preimage.
    pub nonce: u64,
}

impl PubkeyAccount {
    /// Constructs a pubkey account from a public key and nonce.
    pub fn new(public_key: PublicKey, nonce: u64) -> Self {
        Self { public_key, nonce }
    }

    /// Constructs an Ed25519 pubkey account from raw public key bytes.
    pub fn from_public_key_bytes(public_key: [u8; 32], nonce: u64) -> Self {
        Self::new(
            PublicKey::Ed25519 {
                ed25519: groundhog_xdr::types::Ed25519PublicKey::from(public_key),
            },
            nonce,
        )
    }

    /// Returns the canonical pubkey-account preimage.
    pub fn preimage(&self) -> PubkeyAccountPreimage {
        PubkeyAccountPreimage {
            pubkey: self.public_key.clone(),
            nonce: self.nonce,
        }
    }

    /// Returns the pubkey-signature auth claim for this account.
    pub fn auth_claim(&self) -> AuthClaim {
        AuthClaim::kPubkeySignature {
            pubkey_preimage: self.preimage(),
        }
    }

    /// Derives the canonical contract address for this pubkey account.
    ///
    /// This is `SHA256(DomainSeparator("PubkeyAccount") || preimage)` over the
    /// CXS encodings of both, matching what hogd's auth verifier derives. Every
    /// `PublicKey` variant encodes, so derivation cannot fail.
    pub fn address(&self) -> ContractAddress {
        groundhog_domain_separator::sha2::pubkey_account_address(&self.preimage())
    }
}
