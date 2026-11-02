//! ELF validation and the top-level [`lower`] entry point.
//!
//! ELF layout checks live here and in [`elf`]; output validation uses
//! `thruput-rv32-artifact`. Since v4, dispatch lives in the guest (`_start`
//! reads the method id seeded in registers), so the lowerer no longer scans the
//! symbol table: the entry contract is `e_entry == 0` (enforced in [`elf::parse_elf`]) plus the
//! `.text` layout rules below. Stripped release builds are valid inputs.

use std::collections::HashSet;

use crate::elf::{self, Section};
use crate::error::{LowerError, Result, invalid};
use thruput_rv32_artifact::{
    DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, build_artifact, verify_artifact,
};

/// Allocatable sections an rv32 contract image may contain. Anything else with
/// the ALLOC flag is rejected.
pub const ALLOC_ALLOWLIST: [&str; 9] = [
    ".text",
    ".rodata",
    ".srodata",
    ".data",
    ".sdata",
    ".bss",
    ".sbss",
    INTERFACE_TABLE_SECTION,
    INTERFACE_TABLE_ANCHOR_SECTION,
];

/// The contract's interface table: one [`INTERFACE_ID_BYTES`]-byte interface
/// id per `#[contract]` impl block, emitted by the SDK macros and `KEEP`'d by
/// the SDK linker script (`sdk::interface_table`). Always present in the ELF;
/// copied into VM memory only when [`INTERFACE_TABLE_ANCHOR_SECTION`] is,
/// since the table has a reader exactly then. Names mirror the SDK's linker
/// script.
pub const INTERFACE_TABLE_SECTION: &str = "thruput_rv32_interfaces";

/// Width of one interface-table record: an interface id is the leading eight
/// bytes of a hash, like an entrypoint id.
pub const INTERFACE_ID_BYTES: usize = 8;

/// The interface table's reader anchor: an ordinary garbage-collectable word
/// that survives the link only if the SDK's table reader was instantiated (an
/// ERC-165 impl). Its presence with nonzero size is what admits
/// [`INTERFACE_TABLE_SECTION`] into the image.
pub const INTERFACE_TABLE_ANCHOR_SECTION: &str = "thruput_rv32_interfaces_anchor";

/// Low data addresses reserved as a null guard. A static landing at address 0
/// would make a Rust `&'static` to it a readable null pointer. The SDK linker
/// keeps data above this guard; the lowerer rejects any data section that
/// reaches into it.
pub const NULL_GUARD_BYTES: u32 = 8;

/// Name prefix shared by every version of the raw contract-spec section.
///
/// Matched as a prefix, not an exact name: the raw section name carries a
/// wire-format version (`thruputspecrawv5`), and the lowerer drops these
/// bytes rather than decoding them. Recognizing every version keeps a spec
/// format bump from turning into a lowering failure — or worse, spec content
/// copied into the deployed memory image — over bytes that are discarded
/// either way. Mirrors `thruput_spec_tool::RAW_SECTION_PREFIX`; kept local
/// so the production lowerer has no spec-tool dependency, and pinned to it by
/// `spec_metadata_section_names_match_spec_tool`.
pub const RAW_SPEC_SECTION_PREFIX: &str = "thruputspecraw";

/// Raw contract-spec section prefix used through raw spec format 4.
///
/// Stripped like [`RAW_SPEC_SECTION_PREFIX`] so an ELF linked against an older
/// SDK cannot carry spec bytes into the image. The SDK stopped recognizing it
/// in 0.58.0; the lowerer still strips it.
pub const LEGACY_RAW_SPEC_SECTION_PREFIX: &str = "groundhogspecraw";

/// Name prefix shared by every version of the finalized contract-spec
/// section.
///
/// Matched as a prefix for the reason the raw one is. The finalized name
/// gained a version with `groundhogspecv1`, and an artifact carrying an
/// older version must still be stripped rather than reaching the
/// "unexpected alloc section" path: these bytes are discarded either way, so
/// a format bump is no reason to fail a lowering. Mirrors
/// `thruput_spec_tool::FINAL_SECTION_NAME`, which this is a prefix of, and
/// is pinned to it by `spec_metadata_section_names_match_spec_tool`.
pub const FINAL_SPEC_SECTION_PREFIX: &str = "thruputspecv";

/// Finalized contract-spec section prefix used before the `thruput` rename
/// (`groundhogspecv0` through `groundhogspecv2`), stripped for the same reason.
pub const LEGACY_FINAL_SPEC_SECTION_PREFIX: &str = "groundhogspecv";

/// Name of the finalized contract-spec section this SDK generation writes.
pub const FINAL_SPEC_SECTION: &str = "thruputspecv2";

/// The product of lowering: the serialized artifact plus the pieces that went
/// into it (handy for metadata and tests).
///
/// Non-exhaustive: produced only by [`lower`] and [`lower_with`], and fields are added as the
/// lowerer learns to report more (`warnings` was the first such addition).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Lowered {
    /// The complete spec-less v6 artifact bytes (`.rv32a`). The SDK build
    /// pipeline adds its canonical spec outside the guest memory image.
    pub artifact: Vec<u8>,
    /// The contract code (`.text`) section.
    pub code: Vec<u8>,
    /// The stored initial-memory image: the minimal nonzero span of the data
    /// image, staged at `initial_memory_start`. The runtime zero-fills the rest
    /// of `memory_bytes`.
    pub initial_memory: Vec<u8>,
    /// Guest data address where `initial_memory` is staged (0 when empty).
    pub initial_memory_start: usize,
    /// Total guest data address space, in bytes.
    pub memory_bytes: usize,
    /// Non-fatal findings about the input, for the caller to surface. Today:
    /// duplicate ids in the interface table, and a code section past the
    /// protocol-default code limit.
    pub warnings: Vec<String>,
}

/// What a lowering is told about the guest beyond what the ELF says.
///
/// The lowerer does not know which chain an artifact is for, so it enforces
/// no chain bound: code size is whatever the ELF carries (a warning past the
/// protocol default), and the data address space is the contract's own
/// choice, stamped as given. Whether a chain admits the result is checked
/// where the contract is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowerOptions {
    /// Guest data address space stamped into the artifact, in bytes; every
    /// data section must end within it. Defaults to the protocol's 64 KiB. A
    /// contract only uses a larger value once its linker script provides a
    /// matching `__memory_size`.
    pub memory_bytes: usize,
}

impl Default for LowerOptions {
    fn default() -> Self {
        Self {
            memory_bytes: DEFAULT_MEMORY_BYTES,
        }
    }
}

/// Parse, validate, and lower an rv32im contract ELF into a v5 artifact with
/// the protocol-default data address space.
pub fn lower(elf_bytes: &[u8]) -> Result<Lowered> {
    lower_with(elf_bytes, &LowerOptions::default())
}

/// [`lower`] with an explicit data address space.
pub fn lower_with(elf_bytes: &[u8], options: &LowerOptions) -> Result<Lowered> {
    if options.memory_bytes == 0 {
        return Err(invalid("memory_bytes must be nonzero"));
    }
    if u32::try_from(options.memory_bytes).is_err() {
        return Err(invalid(format!(
            "memory_bytes {} exceeds the artifact's 32-bit field",
            options.memory_bytes
        )));
    }
    let sections = elf::parse_elf(elf_bytes)?;
    let (code, initial_memory, initial_memory_start) =
        validate_sections(elf_bytes, &sections, *options)?;
    let mut warnings = check_interface_table(elf_bytes, &sections)?;
    // Not a refusal: the lowerer does not know the target chain, and a chain
    // configured above the protocol default admits this code. The warning
    // is what an offline build can say; the deploying chain has the last word.
    if code.len() > DEFAULT_MAX_CODE_BYTES {
        warnings.push(format!(
            ".text is {} bytes, {} over the {DEFAULT_MAX_CODE_BYTES}-byte code limit a chain \
             enforces unless its configuration raises max_rv32_code_bytes; only a chain \
             admitting at least {} bytes of code accepts this artifact",
            code.len(),
            code.len() - DEFAULT_MAX_CODE_BYTES,
            code.len()
        ));
    }
    let artifact = build_artifact(
        &code,
        &initial_memory,
        initial_memory_start,
        options.memory_bytes,
    )?;
    verify_artifact(&artifact)?;
    Ok(Lowered {
        artifact,
        code,
        initial_memory,
        initial_memory_start,
        memory_bytes: options.memory_bytes,
        warnings,
    })
}

fn spans_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start < b_end && b_start < a_end
}

fn is_spec_metadata_section(name: &str) -> bool {
    [
        RAW_SPEC_SECTION_PREFIX,
        LEGACY_RAW_SPEC_SECTION_PREFIX,
        FINAL_SPEC_SECTION_PREFIX,
        LEGACY_FINAL_SPEC_SECTION_PREFIX,
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

/// Validate the interface table's shape and report duplicate ids.
///
/// Interface ids are 8-byte truncated hashes of `(interface name, MAJOR)`,
/// and a contract's ERC-165 answer is "is this id in my table", so two
/// interfaces that share an id are indistinguishable to callers. The SDK
/// cannot see this at compile time (the colliding interfaces may come from
/// different crates), but the linked table shows it: a duplicate record is
/// either that collision or two contract types in one image implementing
/// the same interface. Both are worth a warning, neither a failure.
fn check_interface_table(data: &[u8], sections: &[Section]) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    for section in sections
        .iter()
        .filter(|s| s.name == INTERFACE_TABLE_SECTION && s.is_alloc() && !s.is_nobits())
    {
        let size = section.size as usize;
        if !size.is_multiple_of(INTERFACE_ID_BYTES) {
            return Err(invalid(format!(
                "{INTERFACE_TABLE_SECTION} size {size} is not a multiple of {INTERFACE_ID_BYTES}"
            )));
        }
        let off = section.offset as usize;
        if off + size > data.len() {
            return Err(invalid(format!(
                "section {INTERFACE_TABLE_SECTION} extends past end of file"
            )));
        }
        // A set, not a scan of what came before: the table is bounded only
        // by the ELF, and it is checked whether or not it reaches the image.
        let mut seen: HashSet<[u8; INTERFACE_ID_BYTES]> =
            HashSet::with_capacity(size / INTERFACE_ID_BYTES);
        for record in data[off..off + size].chunks_exact(INTERFACE_ID_BYTES) {
            let record: [u8; INTERFACE_ID_BYTES] = record
                .try_into()
                .expect("bug: chunks_exact yields INTERFACE_ID_BYTES-long chunks");
            if !seen.insert(record) {
                let hex = record.iter().fold(String::new(), |mut acc, b| {
                    use std::fmt::Write as _;
                    let _ = write!(acc, "{b:02x}");
                    acc
                });
                let message = format!(
                    "interface table has duplicate id 0x{hex}: two interfaces share an id \
                     (or two contract types in this image implement the same interface); \
                     ERC-165 callers cannot tell them apart"
                );
                if !warnings.contains(&message) {
                    warnings.push(message);
                }
            }
        }
    }
    Ok(warnings)
}

/// Find the single `.text` section, erroring if absent or duplicated.
fn unique_text(sections: &[Section]) -> Result<&Section> {
    let mut text = sections.iter().filter(|s| s.name == ".text");
    match (text.next(), text.next()) {
        (Some(section), None) => Ok(section),
        _ => Err(invalid("expected exactly one .text section")),
    }
}

/// Validate the section layout and return `(code, initial_memory, start)`,
/// where `initial_memory` is the minimal nonzero span of the data image and
/// `start` is the data address it is staged at (the runtime zero-fills the
/// rest of the address space).
#[allow(clippy::too_many_lines)]
fn validate_sections(
    data: &[u8],
    sections: &[Section],
    options: LowerOptions,
) -> Result<(Vec<u8>, Vec<u8>, usize)> {
    let text = unique_text(sections)?;
    if !text.is_exec() || !text.is_alloc() {
        return Err(invalid(".text must be alloc+exec"));
    }
    if text.sh_type != elf::SHT_PROGBITS {
        return Err(invalid(".text must be file-backed SHT_PROGBITS"));
    }
    if text.addr != 0 {
        return Err(invalid(format!(
            ".text must start at vaddr 0, got {}",
            text.addr
        )));
    }
    if text.size == 0 {
        return Err(invalid(".text is empty"));
    }
    if !text.size.is_multiple_of(4) {
        return Err(invalid(".text size must be four-byte aligned"));
    }
    let text_off = text.offset as usize;
    let text_size = text.size as usize;
    if text_off + text_size > data.len() {
        return Err(invalid(".text extends past end of file"));
    }
    // Section alignment alone permits paired compressed encodings; check every boundary.
    // Full opcode validation remains the runtime's responsibility.
    for (index, instruction) in data[text_off..text_off + text_size]
        .chunks_exact(4)
        .enumerate()
    {
        if instruction[0] & 0b11 != 0b11 {
            return Err(invalid(format!(
                ".text contains a compressed instruction encoding at PC {:#x}; \
                 build for riscv32im without the C extension",
                index * 4
            )));
        }
    }

    let interface_table_has_reader = sections.iter().any(|section| {
        section.name == INTERFACE_TABLE_ANCHOR_SECTION
            && section.is_alloc()
            && !section.is_nobits()
            && section.size > 0
    });

    // (guest address, file offset, size) of every section with file bytes to
    // stage, and the address range they span together.
    let mut placements: Vec<(usize, usize, usize)> = Vec::new();
    let mut lowest = usize::MAX;
    let mut extent = 0usize;
    let mut data_ranges: Vec<(String, u32, u32)> = Vec::new();
    for section in sections {
        if (section.sh_type == elf::SHT_REL || section.sh_type == elf::SHT_RELA)
            && section.size != 0
        {
            return Err(invalid(format!(
                "relocations are not allowed: {}",
                section.name
            )));
        }
        if section.is_exec() && section.name != ".text" {
            return Err(invalid(format!(
                "unexpected executable section: {}",
                section.name
            )));
        }
        if is_spec_metadata_section(&section.name) {
            continue;
        }
        if !section.is_alloc() {
            continue;
        }
        if !ALLOC_ALLOWLIST.contains(&section.name.as_str()) {
            return Err(invalid(format!(
                "unexpected alloc section: {}",
                section.name
            )));
        }
        if section.name == ".text" {
            continue;
        }
        // The interface table is in every ELF the SDK links, but only a
        // contract that reads it (its anchor survived the link) needs it in
        // memory. Dropped like the spec bytes otherwise.
        if section.name == INTERFACE_TABLE_SECTION && !interface_table_has_reader {
            continue;
        }
        // Code (PC space) and data are separate VM memories under the Harvard
        // split, so a data vaddr inside .text's numeric PC range is legal. Only
        // the low null guard is off-limits: data must not reach into [0, 8).
        if section.addr < NULL_GUARD_BYTES {
            return Err(invalid(format!(
                "data section {} occupies the reserved null guard [0, {NULL_GUARD_BYTES}) at addr {}",
                section.name, section.addr
            )));
        }
        let sec_start = section.addr;
        let sec_end = section
            .addr
            .checked_add(section.size)
            .ok_or_else(|| invalid(format!("section {} address range overflow", section.name)))?;
        for (other_name, other_start, other_end) in &data_ranges {
            if spans_overlap(sec_start, sec_end, *other_start, *other_end) {
                return Err(invalid(format!(
                    "section {} overlaps {other_name}",
                    section.name
                )));
            }
        }
        data_ranges.push((section.name.clone(), sec_start, sec_end));
        if sec_end as usize > options.memory_bytes {
            return Err(LowerError::DataTooLarge {
                section: section.name.clone(),
                end: sec_end as usize,
                max: options.memory_bytes,
            });
        }
        if section.is_nobits() {
            continue;
        }
        let off = section.offset as usize;
        let size = section.size as usize;
        if off + size > data.len() {
            return Err(invalid(format!(
                "section {} extends past end of file",
                section.name
            )));
        }
        if size == 0 {
            continue;
        }
        let addr = section.addr as usize;
        lowest = lowest.min(addr);
        extent = extent.max(addr + size);
        placements.push((addr, off, size));
    }

    // Stage only the span the ELF places data in, not the address space
    // around it: `memory_bytes` is the caller's and can be far larger than any
    // image, and a section may sit high inside it. The per-section check
    // above already bounds `extent` by `memory_bytes`.
    let base = if placements.is_empty() { 0 } else { lowest };
    let mut memory = vec![0u8; extent - base];
    for (addr, off, size) in placements {
        memory[addr - base..addr - base + size].copy_from_slice(&data[off..off + size]);
    }

    let code = data[text_off..text_off + text_size].to_vec();
    // Store the minimal nonzero span; the runtime zero-fills everything
    // outside `[start, start + len)`, so edge zeros carry no information.
    let (initial_memory, start) = match (
        memory.iter().position(|&b| b != 0),
        memory.iter().rposition(|&b| b != 0),
    ) {
        (Some(low), Some(high)) => (memory[low..=high].to_vec(), base + low),
        _ => (Vec::new(), 0),
    };
    Ok((code, initial_memory, start))
}
