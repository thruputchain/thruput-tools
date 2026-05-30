//! ELF validation, dispatch discovery, and the top-level [`lower`] entry point.
//!
//! Validation rules follow `docs/rv32/rv32-vm-experimental-design.md` (and the
//! `scripts/rv32_lower.py` reference this crate replaces).

use std::collections::BTreeMap;

use crate::artifact::{self, DispatchEntry};
use crate::elf::{self, Section, Symbol};
use crate::error::{Result, invalid};

/// Allocatable sections an rv32 contract image may contain. Anything else with
/// the ALLOC flag is rejected.
pub const ALLOC_ALLOWLIST: [&str; 7] = [
    ".text", ".rodata", ".srodata", ".data", ".sdata", ".bss", ".sbss",
];

/// The product of lowering: the serialized artifact plus the pieces that went
/// into it (handy for metadata and tests).
#[derive(Debug, Clone)]
pub struct Lowered {
    /// The complete v3 artifact bytes (`.rv32a`).
    pub artifact: Vec<u8>,
    /// The contract code (`.text`) section.
    pub code: Vec<u8>,
    /// The materialized 64 KiB initial-memory image.
    pub initial_memory: Vec<u8>,
    /// Discovered dispatch entries, sorted by method id.
    pub entries: Vec<DispatchEntry>,
}

/// Parse, validate, and lower an rv32im contract ELF into a v3 artifact.
pub fn lower(elf_bytes: &[u8]) -> Result<Lowered> {
    let sections = elf::parse_elf(elf_bytes)?;
    let symbols = elf::parse_symbols(elf_bytes, &sections)?;
    let (code, initial_memory) = validate_sections(elf_bytes, &sections, &symbols)?;
    let entries = discover_dispatch_entries(&sections, &symbols, code.len())?;
    let artifact = artifact::build_artifact(&code, &initial_memory, &entries)?;
    artifact::verify_artifact(&artifact)?;
    Ok(Lowered {
        artifact,
        code,
        initial_memory,
        entries,
    })
}

fn spans_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> bool {
    a_start < b_end && b_start < a_end
}

/// Find the single `.text` section, erroring if absent or duplicated.
fn unique_text(sections: &[Section]) -> Result<&Section> {
    let mut text = sections.iter().filter(|s| s.name == ".text");
    match (text.next(), text.next()) {
        (Some(section), None) => Ok(section),
        _ => Err(invalid("expected exactly one .text section")),
    }
}

/// Validate the section/symbol layout and return `(code, initial_memory)`.
// Kept as a single function for a 1:1 correspondence with the reference
// validator in `scripts/rv32_lower.py`.
#[allow(clippy::too_many_lines)]
fn validate_sections(
    data: &[u8],
    sections: &[Section],
    symbols: &[Symbol],
) -> Result<(Vec<u8>, Vec<u8>)> {
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
    if text.size as usize > artifact::MAX_CODE_BYTES {
        return Err(invalid(format!(
            ".text exceeds {} bytes",
            artifact::MAX_CODE_BYTES
        )));
    }
    let text_off = text.offset as usize;
    let text_size = text.size as usize;
    if text_off + text_size > data.len() {
        return Err(invalid(".text extends past end of file"));
    }

    let start = symbols
        .iter()
        .find(|s| s.name == "_start")
        .ok_or_else(|| invalid("_start symbol is missing"))?;
    if start.value != 0 {
        return Err(invalid(format!(
            "_start must be at vaddr 0, got {}",
            start.value
        )));
    }
    if start.sym_type != elf::STT_FUNC {
        return Err(invalid("_start must be a function symbol"));
    }

    // .text starts at 0 (checked above), so text_end == text.size and cannot
    // overflow a u32 (size <= 64 KiB).
    let text_end = text.addr + text.size;
    for symbol in symbols {
        let Some(symbol_section) = sections.get(symbol.section_index as usize) else {
            continue;
        };
        if symbol_section.index == text.index
            && (symbol.sym_type == elf::STT_OBJECT || symbol.name.starts_with("$d"))
        {
            return Err(invalid(format!(
                "data symbol {:?} is not allowed in .text",
                symbol.name
            )));
        }
        if symbol.sym_type == elf::STT_OBJECT
            && text.addr <= symbol.value
            && symbol.value < text_end
        {
            return Err(invalid(format!(
                "object symbol {:?} resolves into .text",
                symbol.name
            )));
        }
    }

    let mut memory = vec![0u8; artifact::MAX_MEMORY_BYTES];
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
        if section.addr < text_end {
            return Err(invalid(format!(
                "data section {} overlaps .text range ending at {text_end}",
                section.name
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
        if sec_end as usize > artifact::MAX_MEMORY_BYTES {
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
    }

    let code = data[text_off..text_off + text_size].to_vec();
    Ok((code, memory))
}

/// True for symbol names matching the SDK macro's entry-point shape: exactly
/// 16 uppercase hex characters (the `{:016X}` `ContractEntrypointID`).
fn is_entrypoint_symbol(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// Scan the symbol table for `#[contract]`-macro entry points and resolve them
/// to `(method_id, pc)`, sorted by method id.
fn discover_dispatch_entries(
    sections: &[Section],
    symbols: &[Symbol],
    code_size: usize,
) -> Result<Vec<DispatchEntry>> {
    let text_index = unique_text(sections)?.index;

    let mut entries: BTreeMap<u64, u32> = BTreeMap::new();
    for symbol in symbols {
        if !is_entrypoint_symbol(&symbol.name) {
            continue;
        }
        if symbol.sym_type != elf::STT_FUNC {
            return Err(invalid(format!(
                "entry-point-shaped symbol {:?} is not a function",
                symbol.name
            )));
        }
        if symbol.bind != elf::STB_GLOBAL && symbol.bind != elf::STB_WEAK {
            // A local symbol matching the hex shape is a name collision, not an
            // intended export. Skip silently (matches the reference lowerer).
            continue;
        }
        if symbol.section_index as usize != text_index {
            // Reject SHN_ABS, undefined, and symbols in any other section: a
            // dispatch entry must point into executable code.
            return Err(invalid(format!(
                "entry point {:?} does not resolve to .text (section_index={})",
                symbol.name, symbol.section_index
            )));
        }
        if !symbol.value.is_multiple_of(4) {
            return Err(invalid(format!(
                "entry point {:?} is not four-byte aligned (value={})",
                symbol.name, symbol.value
            )));
        }
        if symbol.value as usize >= code_size {
            return Err(invalid(format!(
                "entry point {:?} pc={} falls outside .text (code_size={code_size})",
                symbol.name, symbol.value
            )));
        }
        if symbol.size != 0 && symbol.value as usize + symbol.size as usize > code_size {
            return Err(invalid(format!(
                "entry point {:?} extends past .text (pc={}, size={}, code_size={code_size})",
                symbol.name, symbol.value, symbol.size
            )));
        }
        // is_entrypoint_symbol guarantees 16 hex digits, so this cannot fail.
        let method_id = u64::from_str_radix(&symbol.name, 16)
            .map_err(|_| invalid(format!("entry point {:?} is not valid hex", symbol.name)))?;
        if let Some(&existing) = entries.get(&method_id)
            && existing != symbol.value
        {
            return Err(invalid(format!(
                "duplicate dispatch entry for {:?} ({existing} vs {})",
                symbol.name, symbol.value
            )));
        }
        entries.insert(method_id, symbol.value);
    }

    if entries.is_empty() {
        return Err(invalid(
            "no #[contract] entry-point symbols found in ELF symbol table",
        ));
    }
    Ok(entries
        .into_iter()
        .map(|(method_id, pc)| DispatchEntry { method_id, pc })
        .collect())
}
