//! Error type for the v4 artifact format.

/// Errors produced while building or validating a v4 RV32 artifact.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    /// A build input violated a format/layout rule.
    #[error("{0}")]
    Invalid(String),
    /// A freshly-built artifact failed its self-check.
    #[error("verify: {0}")]
    Verify(String),
}

/// Result alias for the artifact format.
pub type Result<T> = core::result::Result<T, ArtifactError>;

/// Construct an [`ArtifactError::Invalid`].
pub(crate) fn invalid(message: impl Into<String>) -> ArtifactError {
    ArtifactError::Invalid(message.into())
}

/// Construct an [`ArtifactError::Verify`].
pub(crate) fn verify(message: impl Into<String>) -> ArtifactError {
    ArtifactError::Verify(message.into())
}
