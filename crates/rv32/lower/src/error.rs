//! Error type for the rv32 lowerer.

/// Errors produced while parsing, validating, or lowering an rv32 contract ELF.
#[derive(Debug, thiserror::Error)]
pub enum LowerError {
    /// The input ELF failed structural parsing or a layout/validation rule.
    /// These are user-facing: the contract or its build is malformed.
    #[error("{0}")]
    Invalid(String),
    /// `.text` exceeds the artifact format's code-size cap. The contract is
    /// well-formed, just too big — a distinct case from [`Self::Invalid`] so
    /// consumers can give size-reduction guidance instead of treating the ELF
    /// as malformed.
    #[error(
        ".text is {size} bytes; the current artifact format caps code at {max} bytes \
         ({over} bytes over)",
        over = .size - .max
    )]
    CodeTooLarge {
        /// Actual `.text` size in bytes.
        size: usize,
        /// The artifact format's code-size cap in bytes.
        max: usize,
    },
    /// A data section ends past the VM's data address space. Under the
    /// stack-first layout, data sections sit above the reserved stack, so
    /// shrinking the stack reserve is a legitimate fix alongside reducing
    /// static data.
    #[error(
        "section {section} ends at {end}, exceeding the {max}-byte VM data memory \
         by {over} bytes",
        over = .end - .max
    )]
    DataTooLarge {
        /// Name of the offending section.
        section: String,
        /// End address of the section in guest data space.
        end: usize,
        /// The VM's data address space size in bytes.
        max: usize,
    },
    /// The freshly-built artifact failed its self-check. This indicates a bug
    /// in the writer (or the format constants), not bad input.
    #[error("verify: {0}")]
    Verify(String),
    /// I/O error reading the ELF or writing the artifact / metadata.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<groundhog_rv32_artifact::ArtifactError> for LowerError {
    fn from(error: groundhog_rv32_artifact::ArtifactError) -> Self {
        match error {
            groundhog_rv32_artifact::ArtifactError::Invalid(message) => {
                LowerError::Invalid(message)
            }
            groundhog_rv32_artifact::ArtifactError::Verify(message) => LowerError::Verify(message),
        }
    }
}

/// Result alias for the lowerer.
pub type Result<T> = core::result::Result<T, LowerError>;

/// Construct an [`LowerError::Invalid`] from anything string-like.
pub(crate) fn invalid(message: impl Into<String>) -> LowerError {
    LowerError::Invalid(message.into())
}
