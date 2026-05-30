//! Error type for the rv32 lowerer.

/// Errors produced while parsing, validating, or lowering an rv32 contract ELF.
#[derive(Debug, thiserror::Error)]
pub enum LowerError {
    /// The input ELF failed structural parsing or a layout/validation rule.
    /// These are user-facing: the contract or its build is malformed.
    #[error("{0}")]
    Invalid(String),
    /// The freshly-built artifact failed its self-check. This indicates a bug
    /// in the writer (or the format constants), not bad input.
    #[error("verify: {0}")]
    Verify(String),
    /// I/O error reading the ELF or writing the artifact / metadata.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result alias for the lowerer.
pub type Result<T> = core::result::Result<T, LowerError>;

/// Construct an [`LowerError::Invalid`] from anything string-like.
pub(crate) fn invalid(message: impl Into<String>) -> LowerError {
    LowerError::Invalid(message.into())
}

/// Construct an [`LowerError::Verify`] from anything string-like.
pub(crate) fn verify(message: impl Into<String>) -> LowerError {
    LowerError::Verify(message.into())
}
