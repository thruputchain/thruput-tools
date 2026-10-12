//! Local Ed25519 signer backed by `ed25519-dalek`.

use crate::SigningError;
use ed25519_dalek::Signer as DalekSigner;

/// Local signer using an in-memory Ed25519 key.
///
/// Secret key material is owned by `ed25519-dalek::SigningKey`, which
/// zeroizes on drop.
#[derive(Debug)]
pub struct LocalSigner {
    signing_key: ed25519_dalek::SigningKey,
}

impl LocalSigner {
    /// Constructs a local signer from a 32-byte secret key.
    pub fn from_secret_key(secret: [u8; 32]) -> Self {
        Self {
            signing_key: ed25519_dalek::SigningKey::from_bytes(&secret),
        }
    }

    /// Constructs a local signer from 64-byte keypair bytes.
    pub fn from_keypair_bytes(bytes: [u8; 64]) -> Result<Self, SigningError> {
        let signing_key = ed25519_dalek::SigningKey::from_keypair_bytes(&bytes)
            .map_err(|_| SigningError::InvalidKeypair)?;
        Ok(Self { signing_key })
    }

    /// Returns the signer as 64-byte keypair bytes.
    pub fn to_keypair_bytes(&self) -> [u8; 64] {
        self.signing_key.to_keypair_bytes()
    }

    /// Returns the raw 32-byte verifying key.
    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }
}

impl crate::Signer for LocalSigner {
    fn public_key(&self) -> thruput_xdr::types::PublicKey {
        thruput_xdr::types::PublicKey::Ed25519 {
            ed25519: thruput_xdr::types::Ed25519PublicKey::from(self.verifying_key_bytes()),
        }
    }

    fn sign(&self, message: &[u8]) -> Result<thruput_xdr::types::Signature, SigningError> {
        let sig_bytes = self.signing_key.sign(message).to_bytes();
        Ok(thruput_xdr::types::Signature::Ed25519 {
            ed25519: thruput_xdr::types::Ed25519Signature::from(sig_bytes),
        })
    }
}
