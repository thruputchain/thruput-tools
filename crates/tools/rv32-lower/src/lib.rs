//! `groundhog-rv32-lower` — turn a freestanding `riscv32im-unknown-none-elf`
//! contract binary into a Groundhog **v3 RV32 artifact** (`.rv32a`).
//!
//! This is the production successor to `scripts/rv32_lower.py` in hogd: the
//! `wasm-opt` analog for the rv32 build path. It is a focused, standalone tool
//! (and library) — `groundhog build` shells out to the `rv32-lower` binary
//! rather than embedding it, so the artifact format has exactly one Rust
//! implementation.
//!
//! Pipeline ([`lower`]):
//!   1. [`elf`] — parse the ELF32 section and symbol tables.
//!   2. [`lower::lower`] — validate the layout and materialize the code +
//!      64 KiB initial-memory image; discover `#[contract]` dispatch entries.
//!   3. [`artifact`] — pack the v3 header + sections, then self-verify.
//!
//! See `docs/rv32/rv32-author-toolchain.md` for the surrounding design and the
//! planned split (format crate → protocol-rs; this tool → its own repo).

pub mod artifact;
pub mod elf;
pub mod error;
pub mod lower;
pub mod metadata;

pub use artifact::DispatchEntry;
pub use error::{LowerError, Result};
pub use lower::{Lowered, lower};
pub use metadata::{MetadataInput, render_metadata};
