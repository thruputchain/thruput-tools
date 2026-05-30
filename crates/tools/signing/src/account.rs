//! Pubkey-account helpers.

use crate::SigningError;
use groundhog_xdr::auth_claim::{AuthClaim, PubkeyAccountPreimage};
use groundhog_xdr::types::{ContractAddress, PublicKey, SHA256Hash};
use sha2::{Digest, Sha256};

/// Default nonce used for the first pubkey-account derived from a key.
pub const DEFAULT_PUBKEY_ACCOUNT_NONCE: u64 = 0;

/// Domain separation string used when deriving pubkey-account addresses.
pub const PUBKEY_ACCOUNT_TAG_STR: &str = "PUBKEY_ACCOUNT_TAG";

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
    pub fn address(&self) -> Result<ContractAddress, SigningError> {
        let preimage_bytes = xdrs::txs::encode(&self.preimage())
            .map_err(|e| SigningError::XdrEncode(e.to_string()))?;
        let mut digest: [u8; 32] = Sha256::digest(&preimage_bytes).into();
        let tag_bytes: [u8; 32] = Sha256::digest(PUBKEY_ACCOUNT_TAG_STR.as_bytes()).into();
        let tag = SHA256Hash::from(tag_bytes);

        for (digest_byte, tag_byte) in digest.iter_mut().zip(tag.as_ref().iter()) {
            *digest_byte ^= tag_byte;
        }

        Ok(ContractAddress::from(digest))
    }
}
