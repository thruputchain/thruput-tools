//! `thruput-rv32-artifact` — the Groundhog v5 RV32 artifact format.
//!
//! This crate is the single Rust source of truth for the on-disk `.rv32a`
//! format the hogd runtime parses: constants, field offsets, the format
//! fingerprint, the writer, and the validator.

pub mod error;
pub mod format;

pub use error::{ArtifactError, Result};
pub use format::{
    DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, DeclaredSizes, FORMAT_FINGERPRINT,
    FORMAT_VERSION, HEADER_BYTES, INSTRUCTION_BYTES, MAGIC, build_artifact,
    compute_format_fingerprint, declared_sizes, sha256, sha256_hex, verify_artifact,
};
