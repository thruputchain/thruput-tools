//! A minimal, dependency-free ELF32 little-endian RISC-V reader.
//!
//! Just enough to drive the lowerer: the section table (with names resolved
//! via the section-header string table) and the symbol table (with names
//! resolved via the linked string table). Mirrors the parser in
//! `scripts/rv32_lower.py`.

use crate::error::{Result, invalid};

// Section header types.
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_REL: u32 = 9;
// Section header flags.
pub const SHF_ALLOC: u32 = 0x2;
pub const SHF_EXECINSTR: u32 = 0x4;
// Symbol types / bindings.
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
// `e_machine` for RISC-V.
pub const EM_RISCV: u16 = 243;

const EHDR_BYTES: usize = 52;
const SHDR_BYTES: usize = 40;
const SYM_BYTES: usize = 16;

/// A parsed section header.
#[derive(Debug, Clone)]
pub struct Section {
    pub index: usize,
    pub name: String,
    pub sh_type: u32,
    pub flags: u32,
    pub addr: u32,
    pub offset: u32,
    pub size: u32,
    pub link: u32,
    pub entsize: u32,
}

impl Section {
    #[must_use]
    pub fn is_alloc(&self) -> bool {
        self.flags & SHF_ALLOC != 0
    }
    #[must_use]
    pub fn is_exec(&self) -> bool {
        self.flags & SHF_EXECINSTR != 0
    }
    #[must_use]
    pub fn is_nobits(&self) -> bool {
        self.sh_type == SHT_NOBITS
    }
}

/// A parsed symbol-table entry.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub size: u32,
    pub sym_type: u8,
    pub bind: u8,
    pub section_index: u16,
}

fn read_u16(data: &[u8], off: usize) -> Result<u16> {
    let bytes = data
        .get(off..off + 2)
        .ok_or_else(|| invalid(format!("read past end of file at offset {off}")))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], off: usize) -> Result<u32> {
    let bytes = data
        .get(off..off + 4)
        .ok_or_else(|| invalid(format!("read past end of file at offset {off}")))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_c_string(blob: &[u8], offset: usize) -> Result<String> {
    let tail = blob
        .get(offset..)
        .ok_or_else(|| invalid(format!("string offset {offset} out of range")))?;
    let end = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid("unterminated string in string table"))?;
    String::from_utf8(tail[..end].to_vec()).map_err(|_| invalid("non-utf8 string in string table"))
}

/// Raw section-header fields, before names are resolved via the shstrtab.
struct Raw {
    name_off: u32,
    sh_type: u32,
    flags: u32,
    addr: u32,
    offset: u32,
    size: u32,
    link: u32,
    entsize: u32,
}

/// Parse the ELF header and section table. Enforces the structural shape the
/// lowerer requires: little-endian ELF32, RISC-V, `ET_EXEC`, entry point 0.
pub fn parse_elf(data: &[u8]) -> Result<Vec<Section>> {
    if data.len() < EHDR_BYTES || data[..4] != *b"\x7fELF" {
        return Err(invalid("input is not an ELF file"));
    }
    if data[4] != 1 || data[5] != 1 {
        return Err(invalid("expected little-endian ELF32"));
    }
    if data[6] != 1 {
        return Err(invalid("unsupported ELF version"));
    }

    let e_type = read_u16(data, 16)?;
    let e_machine = read_u16(data, 18)?;
    let e_entry = read_u32(data, 24)?;
    let e_shoff = read_u32(data, 32)? as usize;
    let e_shentsize = read_u16(data, 46)?;
    let e_shnum = read_u16(data, 48)? as usize;
    let e_shstrndx = read_u16(data, 50)? as usize;

    if e_machine != EM_RISCV {
        return Err(invalid(format!(
            "expected RISC-V ELF, got e_machine={e_machine}"
        )));
    }
    if e_type != 2 {
        return Err(invalid(format!(
            "expected executable ELF, got e_type={e_type}"
        )));
    }
    if e_entry != 0 {
        return Err(invalid(format!("expected entry point 0, got {e_entry}")));
    }
    if e_shentsize as usize != SHDR_BYTES {
        return Err(invalid(format!(
            "unexpected section header size {e_shentsize}"
        )));
    }

    let table_bytes = SHDR_BYTES
        .checked_mul(e_shnum)
        .ok_or_else(|| invalid("section table size overflow"))?;
    let table_end = e_shoff
        .checked_add(table_bytes)
        .ok_or_else(|| invalid("section table offset overflow"))?;
    if table_end > data.len() {
        return Err(invalid("section headers extend past end of file"));
    }
    if e_shstrndx >= e_shnum {
        return Err(invalid("invalid section string table index"));
    }

    // First pass: raw (name-offset, fields). Names need the shstrtab, which is
    // itself a section, so resolve names in a second pass.
    let mut raw = Vec::with_capacity(e_shnum);
    for i in 0..e_shnum {
        let off = e_shoff + i * SHDR_BYTES;
        raw.push(Raw {
            name_off: read_u32(data, off)?,
            sh_type: read_u32(data, off + 4)?,
            flags: read_u32(data, off + 8)?,
            addr: read_u32(data, off + 12)?,
            offset: read_u32(data, off + 16)?,
            size: read_u32(data, off + 20)?,
            link: read_u32(data, off + 24)?,
            entsize: read_u32(data, off + 36)?,
        });
    }

    let shstr = &raw[e_shstrndx];
    let shstr_off = shstr.offset as usize;
    let shstr_end = shstr_off
        .checked_add(shstr.size as usize)
        .ok_or_else(|| invalid("section string table overflow"))?;
    // Deliberately stricter than the Python reference (which slices and so
    // silently clamps a past-EOF table); we reject it outright.
    let shstr_blob = data
        .get(shstr_off..shstr_end)
        .ok_or_else(|| invalid("section string table out of range"))?;

    let mut sections = Vec::with_capacity(e_shnum);
    for (index, r) in raw.iter().enumerate() {
        let name = if r.name_off != 0 {
            read_c_string(shstr_blob, r.name_off as usize)?
        } else {
            String::new()
        };
        sections.push(Section {
            index,
            name,
            sh_type: r.sh_type,
            flags: r.flags,
            addr: r.addr,
            offset: r.offset,
            size: r.size,
            link: r.link,
            entsize: r.entsize,
        });
    }
    Ok(sections)
}

/// Parse every symbol table in the ELF, resolving names via each table's
/// linked string table.
pub fn parse_symbols(data: &[u8], sections: &[Section]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();
    for section in sections {
        if section.sh_type != SHT_SYMTAB {
            continue;
        }
        let strings = sections
            .get(section.link as usize)
            .ok_or_else(|| invalid("symbol table has invalid string table link"))?;
        if strings.sh_type != SHT_STRTAB {
            return Err(invalid("symbol table link must point to a string table"));
        }
        if section.entsize as usize != SYM_BYTES {
            return Err(invalid("unexpected ELF32 symbol size"));
        }
        // Reject a symbol table whose size is not a whole number of entries.
        // A partial trailing entry is ill-formed, and tolerating it would
        // either silently drop a symbol (here) or fabricate one from adjacent
        // file bytes (the Python reference's `unpack_from` over a stride
        // range) -- diverging the two lowerers' symbol sets on malformed input.
        if !(section.size as usize).is_multiple_of(SYM_BYTES) {
            return Err(invalid(
                "symbol table size is not a multiple of the entry size",
            ));
        }

        let str_off = strings.offset as usize;
        let str_end = str_off
            .checked_add(strings.size as usize)
            .ok_or_else(|| invalid("string table overflow"))?;
        // Deliberately stricter than the Python reference, which slices and so
        // silently clamps a past-EOF string table; we reject it outright.
        let string_blob = data
            .get(str_off..str_end)
            .ok_or_else(|| invalid("string table out of range"))?;

        let start = section.offset as usize;
        let end = start
            .checked_add(section.size as usize)
            .ok_or_else(|| invalid("symbol table overflow"))?;
        if end > data.len() {
            return Err(invalid("symbol table extends past end of file"));
        }

        let mut off = start;
        while off + SYM_BYTES <= end {
            let st_name = read_u32(data, off)?;
            let st_value = read_u32(data, off + 4)?;
            let st_size = read_u32(data, off + 8)?;
            let st_info = data[off + 12];
            let st_shndx = read_u16(data, off + 14)?;
            let name = if st_name != 0 {
                read_c_string(string_blob, st_name as usize)?
            } else {
                String::new()
            };
            symbols.push(Symbol {
                name,
                value: st_value,
                size: st_size,
                sym_type: st_info & 0x0f,
                bind: (st_info >> 4) & 0x0f,
                section_index: st_shndx,
            });
            off += SYM_BYTES;
        }
    }
    Ok(symbols)
}
