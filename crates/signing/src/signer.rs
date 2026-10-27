//! Signing abstraction used by Thruput transaction builders.

use crate::SigningError;

/// Produces Thruput-compatible signatures over raw message bytes.
///
/// The trait is synchronous and covers in-process keys.
pub trait Signer {
    /// Returns the signer's public key in Thruput XDR form.
    fn public_key(&self) -> thruput_xdr::types::PublicKey;

    /// Signs the provided message bytes.
    fn sign(&self, message: &[u8]) -> Result<thruput_xdr::types::Signature, SigningError>;
}
