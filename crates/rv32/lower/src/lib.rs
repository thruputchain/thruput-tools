//! `groundhog-rv32-lower` — turn a freestanding `riscv32im-unknown-none-elf`
//! contract binary into a Groundhog **v3 RV32 artifact** (`.rv32a`).
//!
//! This is the production successor to `scripts/rv32_lower.py` in hogd: the
//! `wasm-opt` analog for the rv32 build path. It is a focused lowerer with two
//! entrypoints: a standalone `rv32-lower` binary for scripts and interop checks,
//! and a library API used by `groundhog build --target rv32`. The artifact
//! format still has exactly one Rust implementation.
//!
//! Pipeline ([`lower`]):
//!   1. [`elf`] — parse the ELF32 section and symbol tables.
//!   2. [`lower::lower`] — validate the layout and materialize the code +
//!      64 KiB initial-memory image; discover `#[contract]` dispatch entries.
//!   3. `groundhog-rv32-artifact` — pack the v3 header + sections, then
//!      self-verify.
//!
//! See `docs/rv32/rv32-author-toolchain.md` for the surrounding design.

pub mod elf;
pub mod error;
pub mod lower;
pub mod metadata;

pub use error::{LowerError, Result};
pub use groundhog_rv32_artifact::{DispatchEntry, FORMAT_FINGERPRINT, compute_format_fingerprint};
pub use lower::{Lowered, lower};
pub use metadata::{MetadataInput, render_metadata, summary_json};
