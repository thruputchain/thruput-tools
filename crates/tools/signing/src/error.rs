//! Error types for signing and transaction construction.

/// Errors returned by `groundhog-signing`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SigningError {
    /// The provided secret key bytes were invalid.
    #[error("invalid secret key bytes")]
    InvalidSecretKey,
    /// The provided keypair bytes were invalid.
    #[error("invalid keypair bytes")]
    InvalidKeypair,
    /// Failed to encode an XDR value.
    #[error("xdr encode failed: {0}")]
    XdrEncode(String),
    /// The dev-wallet artifact is not an RV32 artifact.
    #[error("unsupported dev-wallet artifact extension for {0}; expected .rv32a")]
    UnsupportedDevWalletArtifact(std::path::PathBuf),
}
