//! `thruput-rv32-artifact` — the Thruput v6 RV32 artifact format.
//!
//! This crate is the single Rust source of truth for the on-disk `.rv32a`
//! format the hogd runtime parses: constants, field offsets, the format
//! fingerprint, the writer, and the validator.

pub mod error;
pub mod format;

pub use error::{ArtifactError, Result};
pub use format::{
    ArtifactSpec, DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, DeclaredSizes, FORMAT_FINGERPRINT,
    FORMAT_VERSION, HEADER_BYTES, INSTRUCTION_BYTES, MAGIC, artifact_spec, build_artifact,
    build_artifact_with_spec, compute_format_fingerprint, declared_sizes, sha256, sha256_hex,
    verify_artifact,
};
