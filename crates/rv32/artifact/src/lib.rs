//! `groundhog-rv32-artifact` — the Groundhog v3 RV32 artifact format.
//!
//! This crate is the single Rust source of truth for the on-disk `.rv32a`
//! format the hogd runtime parses: constants, field offsets, the format
//! fingerprint, the writer, and the validator.

pub mod error;
pub mod format;

pub use error::{ArtifactError, Result};
pub use format::{
    DISPATCH_ENTRY_BYTES, DispatchEntry, FORMAT_FINGERPRINT, FORMAT_VERSION, HEADER_BYTES,
    INSTRUCTION_BYTES, MAGIC, MAX_CODE_BYTES, MAX_DISPATCH_BYTES, MAX_DISPATCH_ENTRIES,
    MAX_MEMORY_BYTES, build_artifact, build_dispatch_table, compute_format_fingerprint, sha256,
    sha256_hex, verify_artifact,
};
