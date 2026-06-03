//! ELF validation and the top-level [`lower`] entry point.
//!
//! Validation rules follow `docs/rv32/rv32-vm-experimental-design.md`. v4
//! dispatch lives in the guest (`_start` reads the method id seeded in
//! registers), so the lowerer no longer scans the symbol table: the entry
//! contract is `e_entry == 0` (enforced in [`elf::parse_elf`]) plus the
//! `.text` layout rules below. Stripped release builds are valid inputs.

use crate::elf::{self, Section};
use crate::error::{Result, invalid};
use groundhog_rv32_artifact::{MAX_CODE_BYTES, MAX_MEMORY_BYTES, build_artifact, verify_artifact};

/// Allocatable sections an rv32 contract image may contain. Anything else with
/// the ALLOC flag is rejected.
pub const ALLOC_ALLOWLIST: [&str; 7] = [
    ".text", ".rodata", ".srodata", ".data", ".sdata", ".bss", ".sbss",
];

/// Low data addresses reserved as a null guard. The VM stages the data image
/// at data address 0, so a static landing at address 0 would make a Rust
/// `&'static` to it a readable null pointer. The SDK linker keeps data above
/// this guard; the lowerer rejects any data section that reaches into it.
pub const NULL_GUARD_BYTES: u32 = 8;

/// Contract spec metadata sections are compile-time sidecars, not VM memory.
/// These names mirror `groundhog_spec_tool::{RAW_SECTION_NAME, FINAL_SECTION_NAME}`;
/// keep them local so the production lowerer has no spec-tool dependency.
pub const SPEC_METADATA_SECTIONS: [&str; 2] = ["groundhogspecrawv0", "groundhogspecv0"];

/// The product of lowering: the serialized artifact plus the pieces that went
/// into it (handy for metadata and tests).
#[derive(Debug, Clone)]
pub struct Lowered {
    /// The complete v4 artifact bytes (`.rv32a`).
    pub artifact: Vec<u8>,
    /// The contract code (`.text`) section.
    pub code: Vec<u8>,
    /// The stored initial-memory prefix `[0, high_water)`. The runtime
    /// zero-fills the rest of `memory_bytes`.
    pub initial_memory: Vec<u8>,
    /// Total guest data address space (the `sp` top), in bytes.
    pub memory_bytes: usize,
}

/// Parse, validate, and lower an rv32im contract ELF into a v4 artifact.
pub fn lower(elf_bytes: &[u8]) -> Result<Lowered> {
    let sections = elf::parse_elf(elf_bytes)?;
    let (code, initial_memory) = validate_sections(elf_bytes, &sections)?;
    let artifact = build_artifact(&code, &initial_memory)?;
    verify_artifact(&artifact)?;
    Ok(Lowered {
        artifact,
        code,
        initial_memory,
        memory_bytes: MAX_MEMORY_BYTES,
    })
}

fn spans_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start < b_end && b_start < a_end
}

fn is_spec_metadata_section(name: &str) -> bool {
    SPEC_METADATA_SECTIONS.contains(&name)
}

/// Find the single `.text` section, erroring if absent or duplicated.
fn unique_text(sections: &[Section]) -> Result<&Section> {
    let mut text = sections.iter().filter(|s| s.name == ".text");
    match (text.next(), text.next()) {
        (Some(section), None) => Ok(section),
        _ => Err(invalid("expected exactly one .text section")),
    }
}

/// Validate the section layout and return `(code, initial_memory)`, where
/// `initial_memory` is the high-water prefix of the data image (the runtime
/// zero-fills the rest of the address space).
#[allow(clippy::too_many_lines)]
fn validate_sections(data: &[u8], sections: &[Section]) -> Result<(Vec<u8>, Vec<u8>)> {
    let text = unique_text(sections)?;
    if !text.is_exec() || !text.is_alloc() {
        return Err(invalid(".text must be alloc+exec"));
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
    if text.size as usize > MAX_CODE_BYTES {
        return Err(invalid(format!(".text exceeds {MAX_CODE_BYTES} bytes")));
    }
    let text_off = text.offset as usize;
    let text_size = text.size as usize;
    if text_off + text_size > data.len() {
        return Err(invalid(".text extends past end of file"));
    }

    let mut memory = vec![0u8; MAX_MEMORY_BYTES];
    // End address of the highest-addressed PROGBITS data section written into
    // the image. The stored prefix is `memory[0..high_water]`; everything above
    // is zero-filled by the runtime (bss, heap, stack).
    let mut high_water: usize = 0;
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
        if sec_end as usize > MAX_MEMORY_BYTES {
            return Err(invalid(format!(
                "section {} exceeds memory limit",
                section.name
            )));
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
        let addr = section.addr as usize;
        memory[addr..addr + size].copy_from_slice(&data[off..off + size]);
        high_water = high_water.max(addr + size);
    }

    let code = data[text_off..text_off + text_size].to_vec();
    // Store only the initialized prefix; the runtime zero-fills the tail.
    memory.truncate(high_water);
    Ok((code, memory))
}
