//! Pubkey-account helpers.

/// Hashes a public key, scheme included:
/// `SHA256(cxs(DomainSeparator("PublicKey")) || cxs(PublicKey))`.
///
/// Pubkey-account preimages and `kPubkeySignature` claims name keys by this
/// hash, and a signed transaction's `pubkey_claim_key` must hash to it.
pub use thruput_domain_separator::sha2::public_key_hash;
use thruput_xdr::auth_claim::{AuthClaim, PubkeyAccountPreimage};
use thruput_xdr::types::{ContractAddress, PublicKey, PublicKeyHash};

/// Default nonce used for the first pubkey-account derived from a key.
pub const DEFAULT_PUBKEY_ACCOUNT_NONCE: u64 = 0;

/// A pubkey-backed account with the nonce used in address derivation.
///
/// The account's address and auth claim commit to the key by its
/// [`PublicKeyHash`]; the full key travels beside the signatures as
/// `SignedTransaction::pubkey_claim_key`.
#[derive(Clone, Debug)]
pub struct PubkeyAccount {
    /// The underlying public key, of any supported scheme.
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
                ed25519: thruput_xdr::types::Ed25519PublicKey::from(public_key),
            },
            nonce,
        )
    }

    /// Returns the hash naming this account's key.
    pub fn public_key_hash(&self) -> PublicKeyHash {
        public_key_hash(&self.public_key)
    }

    /// Returns the canonical pubkey-account preimage.
    pub fn preimage(&self) -> PubkeyAccountPreimage {
        PubkeyAccountPreimage {
            pubkey_hash: self.public_key_hash(),
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
        thruput_domain_separator::sha2::pubkey_account_address(&self.preimage())
    }
}
