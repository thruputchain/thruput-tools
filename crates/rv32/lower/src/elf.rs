//! A minimal, dependency-free ELF32 little-endian RISC-V reader.
//!
//! Just enough to drive the lowerer: the section table, with names resolved
//! via the section-header string table. v4 dispatch lives in the guest, so the
//! symbols are optional and used only for memory reporting. The entry contract
//! is `e_entry == 0` plus the `.text` layout rules.

use crate::error::{Result, invalid};

// Section header types.
/// ELF section containing program-defined, file-backed data.
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_RELA: u32 = 4;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_REL: u32 = 9;
// Section header flags.
pub const SHF_ALLOC: u32 = 0x2;
pub const SHF_EXECINSTR: u32 = 0x4;
// `e_machine` for RISC-V.
pub const EM_RISCV: u16 = 243;

const EHDR_BYTES: usize = 52;
const SHDR_BYTES: usize = 40;

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

pub(crate) fn read_u16(data: &[u8], off: usize) -> Result<u16> {
    let bytes = data
        .get(off..off + 2)
        .ok_or_else(|| invalid(format!("read past end of file at offset {off}")))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn read_u32(data: &[u8], off: usize) -> Result<u32> {
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
