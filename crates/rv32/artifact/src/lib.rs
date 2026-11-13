//! `thruput-rv32-artifact` — the Thruput v6 RV32 artifact format.
//!
//! This crate is the single Rust source of truth for the on-disk `.rv32a`
//! format the hogd runtime parses: constants, field offsets, the format
//! fingerprint, the writer, and the validator.

pub mod error;
pub mod extension;
pub mod format;

pub use error::{ArtifactError, Result};
pub use extension::{
    ArtifactExtension, ArtifactSpec, EXTENSION_KIND_CONTRACT_SPEC, decode_spec_extension,
    encode_spec_extension,
};
pub use format::{
    DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, DeclaredSizes, FORMAT_FINGERPRINT,
    FORMAT_VERSION, HEADER_BYTES, INSTRUCTION_BYTES, LEGACY_FORMAT_VERSION_V5, MAGIC,
    READABLE_FORMAT_VERSIONS, SYSCALL_API_MAJOR, SyscallApiVersion, artifact_extensions,
    artifact_spec, build_artifact, build_artifact_with_extensions, build_artifact_with_spec,
    compute_format_fingerprint, declared_sizes, declared_syscall_api, sha256, sha256_hex,
    verify_artifact,
};
