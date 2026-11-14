//! `thruput-rv32-lower` — turn a freestanding `riscv32im-unknown-none-elf`
//! contract binary into a Thruput **v5 RV32 artifact** (`.rv32a`).
//!
//! This focused lowerer validates the contract ELF and emits the canonical RV32
//! artifact format. It has two entrypoints: a standalone `rv32-lower` binary
//! for scripts and interop checks, and a library API used by `thruput build`.
//! The artifact format has exactly one Rust implementation.
//!
//! Pipeline ([`lower()`]):
//!   1. [`elf`] — parse the ELF32 section table.
//!   2. [`lower::lower`] — validate the layout and materialize the code plus
//!      the minimal nonzero initial-memory span and its load base. Dispatch
//!      lives in the guest (since v4), so no symbol scanning is needed and
//!      stripped builds are valid.
//!   3. `thruput-rv32-artifact` — pack the v7 header + sections, then
//!      self-verify.
//!
//! See `docs/architecture.md` ("From contract source to invocation") for the
//! build and invocation flow.

pub mod elf;
pub mod error;
pub mod lower;
pub mod metadata;

pub use error::{LowerError, Result};
pub use lower::{
    ALLOC_ALLOWLIST, INTERFACE_ID_BYTES, INTERFACE_TABLE_ANCHOR_SECTION, INTERFACE_TABLE_SECTION,
    LowerOptions, Lowered, SYSCALL_API_RECORD_BYTES, SYSCALL_API_SECTION, lower, lower_with,
};
pub use metadata::{MetadataInput, render_metadata, summary_json};
pub use thruput_rv32_artifact::{
    FORMAT_FINGERPRINT, SyscallApiVersion, compute_format_fingerprint,
};
