//! Signing abstraction used by Groundhog transaction builders.

use crate::SigningError;

/// Produces Groundhog-compatible signatures over raw message bytes.
///
/// A future async companion is expected for hardware or remote signers;
/// keep this trait focused on the synchronous local path.
pub trait Signer {
    /// Returns the signer's public key in Groundhog XDR form.
    fn public_key(&self) -> thruput_xdr::types::PublicKey;

    /// Signs the provided message bytes.
    fn sign(&self, message: &[u8]) -> Result<thruput_xdr::types::Signature, SigningError>;
}
