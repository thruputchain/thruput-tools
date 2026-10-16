//! Optional capacity reporting for the SDK's stack-first ELF layout.

use crate::elf::{self, Section};
use crate::lower::ALLOC_ALLOWLIST;

/// Linked memory reservations, not runtime high-water measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryLayout {
    pub stack_bytes: u32,
    /// Static data plus alignment between the stack and heap.
    pub static_data_bytes: u32,
    pub heap_bytes: u32,
}

fn section_bytes<'a>(data: &'a [u8], section: &Section) -> Option<&'a [u8]> {
    let start = section.offset as usize;
    data.get(start..start.checked_add(section.size as usize)?)
}

/// Missing, ambiguous or inconsistent symbols leave reporting unavailable;
/// symbols never affect artifact admission or its bytes.
pub(crate) fn memory_layout(
    data: &[u8],
    sections: &[Section],
    memory_bytes: usize,
) -> Option<MemoryLayout> {
    let names: [&[u8]; 4] = [
        b"__stack_bottom\0",
        b"__stack_top\0",
        b"__heap_base\0",
        b"__heap_end\0",
    ];
    let mut values = [None; 4];
    for table in sections.iter().filter(|s| s.sh_type == 2) {
        if table.entsize != 16 || table.size % 16 != 0 {
            return None;
        }
        let strings = sections.get(table.link as usize)?;
        if strings.sh_type != 3 {
            return None;
        }
        let strings = section_bytes(data, strings)?;
        for symbol in section_bytes(data, table)?.chunks_exact(16) {
            let section = elf::read_u16(symbol, 14).ok()?;
            if section == 0 {
                continue;
            }
            let name = strings.get(elf::read_u32(symbol, 0).ok()? as usize..)?;
            if let Some(index) = names.iter().position(|expected| name.starts_with(expected)) {
                if section != 0xfff1 && usize::from(section) >= sections.len() {
                    return None;
                }
                if values[index]
                    .replace(elf::read_u32(symbol, 4).ok()?)
                    .is_some()
                {
                    return None;
                }
            }
        }
    }
    let [Some(bottom), Some(top), Some(heap_base), Some(heap_end)] = values else {
        return None;
    };
    if bottom != 0 || top < 16 || top % 16 != 0 || heap_end as usize != memory_bytes {
        return None;
    }
    let static_data_bytes = heap_base.checked_sub(top)?;
    let heap_bytes = heap_end.checked_sub(heap_base)?;
    for section in sections.iter().filter(|s| {
        s.is_alloc()
            && s.name != ".text"
            && ALLOC_ALLOWLIST.contains(&s.name.as_str())
            && s.size != 0
    }) {
        if section.addr < top || section.addr.checked_add(section.size)? > heap_base {
            return None;
        }
    }
    Some(MemoryLayout {
        stack_bytes: top,
        static_data_bytes,
        heap_bytes,
    })
}
