//! Hermetic tests for the rv32 lowerer.
//!
//! Most tests build a minimal synthetic ELF32 in-process (no riscv toolchain
//! required) and exercise the happy path plus every validation/rejection rule.
//! A final opt-in test (`RV32_LOWER_PARITY_ELF`) lowers a real contract ELF and
//! optionally byte-compares it against a known-good golden artifact — an
//! opt-in / out-of-default-CI regression check.

// The synthetic-ELF builder favors owned specs that tests clone-and-mutate.
#![allow(clippy::needless_pass_by_value)]

use groundhog_rv32_artifact::{FORMAT_FINGERPRINT, verify_artifact};
use groundhog_rv32_lower::{MetadataInput, lower, render_metadata};

// ---------------------------------------------------------------------------
// Minimal ELF32 little-endian RISC-V builder.
// ---------------------------------------------------------------------------

// Section/symbol constants (subset, mirrors elf.rs). A `.symtab` is still
// assembled so the reader's generic section parse is exercised, but the lowerer
// no longer interprets symbols, so only the structural shape matters here.
const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHT_REL: u32 = 9;
const SHF_ALLOC: u32 = 0x2;
const SHF_EXECINSTR: u32 = 0x4;
const STT_FUNC: u8 = 2;
const STB_GLOBAL: u8 = 1;
const EM_RISCV: u16 = 243;

fn as_u32(value: usize) -> u32 {
    u32::try_from(value).expect("test value fits in u32")
}

#[derive(Clone)]
struct ExtraSection {
    name: &'static str,
    sh_type: u32,
    flags: u32,
    addr: u32,
    file_bytes: Vec<u8>,
    /// Section header `sh_size`. Defaults to `file_bytes.len()`; set explicitly
    /// for NOBITS (`.bss`), which carries a size but no file content.
    size_override: Option<u32>,
}

impl ExtraSection {
    fn progbits(name: &'static str, flags: u32, addr: u32, file_bytes: Vec<u8>) -> Self {
        Self {
            name,
            sh_type: SHT_PROGBITS,
            flags,
            addr,
            file_bytes,
            size_override: None,
        }
    }

    fn nobits(name: &'static str, addr: u32, size: u32) -> Self {
        Self {
            name,
            sh_type: SHT_NOBITS,
            flags: SHF_ALLOC,
            addr,
            file_bytes: Vec::new(),
            size_override: Some(size),
        }
    }
}

#[derive(Clone)]
struct SymSpec {
    name: &'static str,
    value: u32,
    size: u32,
    sym_type: u8,
    bind: u8,
    /// Section index in the assembled table: 1 = `.text`.
    shndx: u16,
}

#[derive(Clone)]
struct ElfSpec {
    text: Vec<u8>,
    text_addr: u32,
    extras: Vec<ExtraSection>,
    syms: Vec<SymSpec>,
    /// When true, omit the `.symtab`/`.strtab` sections entirely (a stripped
    /// release build). v4 lowering must accept these.
    strip: bool,
}

fn add_name(blob: &mut Vec<u8>, name: &str) -> u32 {
    let offset = as_u32(blob.len());
    blob.extend_from_slice(name.as_bytes());
    blob.push(0);
    offset
}

#[allow(clippy::too_many_arguments)]
fn push_shdr(
    out: &mut Vec<u8>,
    name: u32,
    sh_type: u32,
    flags: u32,
    addr: u32,
    offset: u32,
    size: u32,
    link: u32,
    entsize: u32,
) {
    for field in [
        name, sh_type, flags, addr, offset, size, link, 0, 1, entsize,
    ] {
        out.extend_from_slice(&field.to_le_bytes());
    }
}

/// Assemble a complete ELF file. Section table layout:
/// `[0]=NULL, [1]=.text, [2..]=extras, then .symtab, .strtab, .shstrtab`.
#[allow(clippy::too_many_lines)]
fn assemble(spec: ElfSpec) -> Vec<u8> {
    let extra_count = spec.extras.len();
    let symtab_idx = as_u32(2 + extra_count);
    let strtab_idx = symtab_idx + 1;
    let symbol_sections = if spec.strip { 0 } else { 2 };
    let section_count = 2 + extra_count + symbol_sections + 1;

    // Section-header string table.
    let mut shstr = vec![0u8];
    let text_name = add_name(&mut shstr, ".text");
    let extra_names: Vec<u32> = spec
        .extras
        .iter()
        .map(|e| add_name(&mut shstr, e.name))
        .collect();
    let symtab_name = add_name(&mut shstr, ".symtab");
    let strtab_name = add_name(&mut shstr, ".strtab");
    let shstrtab_name = add_name(&mut shstr, ".shstrtab");

    // Symbol string table + symbol-name offsets.
    let mut strtab = vec![0u8];
    let sym_name_offsets: Vec<u32> = spec
        .syms
        .iter()
        .map(|s| {
            if s.name.is_empty() {
                0
            } else {
                add_name(&mut strtab, s.name)
            }
        })
        .collect();

    // Symbol table: leading null symbol, then the spec symbols.
    let mut symtab = vec![0u8; 16];
    for (sym, &name_off) in spec.syms.iter().zip(&sym_name_offsets) {
        symtab.extend_from_slice(&name_off.to_le_bytes());
        symtab.extend_from_slice(&sym.value.to_le_bytes());
        symtab.extend_from_slice(&sym.size.to_le_bytes());
        symtab.push((sym.bind << 4) | sym.sym_type);
        symtab.push(0); // st_other
        symtab.extend_from_slice(&sym.shndx.to_le_bytes());
    }

    // File body: ehdr placeholder, then section contents, then the shdr table.
    let mut file = vec![0u8; 52];
    let text_off = as_u32(file.len());
    file.extend_from_slice(&spec.text);

    let mut extra_offsets = Vec::with_capacity(extra_count);
    for extra in &spec.extras {
        if extra.file_bytes.is_empty() {
            extra_offsets.push(0);
        } else {
            extra_offsets.push(as_u32(file.len()));
            file.extend_from_slice(&extra.file_bytes);
        }
    }

    let strtab_off = as_u32(file.len());
    file.extend_from_slice(&strtab);
    let symtab_off = as_u32(file.len());
    file.extend_from_slice(&symtab);
    let shstr_off = as_u32(file.len());
    file.extend_from_slice(&shstr);
    while !file.len().is_multiple_of(4) {
        file.push(0);
    }
    let e_shoff = as_u32(file.len());

    // Section headers.
    push_shdr(&mut file, 0, 0, 0, 0, 0, 0, 0, 0); // NULL
    push_shdr(
        &mut file,
        text_name,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_EXECINSTR,
        spec.text_addr,
        text_off,
        as_u32(spec.text.len()),
        0,
        0,
    );
    for (extra, (&name, &offset)) in spec
        .extras
        .iter()
        .zip(extra_names.iter().zip(&extra_offsets))
    {
        push_shdr(
            &mut file,
            name,
            extra.sh_type,
            extra.flags,
            extra.addr,
            offset,
            extra
                .size_override
                .unwrap_or_else(|| as_u32(extra.file_bytes.len())),
            0,
            0,
        );
    }
    if !spec.strip {
        push_shdr(
            &mut file,
            symtab_name,
            SHT_SYMTAB,
            0,
            0,
            symtab_off,
            as_u32(symtab.len()),
            strtab_idx,
            16,
        );
        push_shdr(
            &mut file,
            strtab_name,
            SHT_STRTAB,
            0,
            0,
            strtab_off,
            as_u32(strtab.len()),
            0,
            0,
        );
    }
    push_shdr(
        &mut file,
        shstrtab_name,
        SHT_STRTAB,
        0,
        0,
        shstr_off,
        as_u32(shstr.len()),
        0,
        0,
    );

    // ELF header.
    let mut ehdr = Vec::with_capacity(52);
    ehdr.extend_from_slice(b"\x7fELF");
    ehdr.push(1); // ELFCLASS32
    ehdr.push(1); // ELFDATA2LSB
    ehdr.push(1); // EV_CURRENT
    ehdr.extend_from_slice(&[0u8; 9]); // OSABI + padding
    ehdr.extend_from_slice(&2u16.to_le_bytes()); // e_type = ET_EXEC
    ehdr.extend_from_slice(&EM_RISCV.to_le_bytes());
    ehdr.extend_from_slice(&1u32.to_le_bytes()); // e_version
    ehdr.extend_from_slice(&0u32.to_le_bytes()); // e_entry
    ehdr.extend_from_slice(&0u32.to_le_bytes()); // e_phoff
    ehdr.extend_from_slice(&e_shoff.to_le_bytes());
    ehdr.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    ehdr.extend_from_slice(&52u16.to_le_bytes()); // e_ehsize
    ehdr.extend_from_slice(&0u16.to_le_bytes()); // e_phentsize
    ehdr.extend_from_slice(&0u16.to_le_bytes()); // e_phnum
    ehdr.extend_from_slice(&40u16.to_le_bytes()); // e_shentsize
    ehdr.extend_from_slice(&as_u16(section_count).to_le_bytes()); // e_shnum
    ehdr.extend_from_slice(&as_u16(section_count - 1).to_le_bytes()); // e_shstrndx
    file[..52].copy_from_slice(&ehdr);

    file
}

fn as_u16(value: usize) -> u16 {
    u16::try_from(value).expect("test value fits in u16")
}

/// A baseline valid contract: 32-byte `.text`, `_start` at 0, two method
/// entry points (method ids 0xAA @ pc 4 and 0x10 @ pc 16).
fn happy_spec() -> ElfSpec {
    ElfSpec {
        text: vec![0u8; 32],
        text_addr: 0,
        extras: Vec::new(),
        syms: vec![
            SymSpec {
                name: "_start",
                value: 0,
                size: 0,
                sym_type: STT_FUNC,
                bind: STB_GLOBAL,
                shndx: 1,
            },
            SymSpec {
                name: "00000000000000AA",
                value: 4,
                size: 0,
                sym_type: STT_FUNC,
                bind: STB_GLOBAL,
                shndx: 1,
            },
            SymSpec {
                name: "0000000000000010",
                value: 16,
                size: 0,
                sym_type: STT_FUNC,
                bind: STB_GLOBAL,
                shndx: 1,
            },
        ],
        strip: false,
    }
}

fn expect_err(spec: ElfSpec, needle: &str) {
    let elf = assemble(spec);
    let err = lower(&elf).expect_err("expected lowering to fail");
    let message = err.to_string();
    assert!(
        message.contains(needle),
        "error {message:?} did not contain {needle:?}"
    );
}

// ---------------------------------------------------------------------------
// Happy path.
// ---------------------------------------------------------------------------

#[test]
fn lowers_a_minimal_contract() {
    let elf = assemble(happy_spec());
    let lowered = lower(&elf).expect("happy path lowers");

    assert_eq!(lowered.code.len(), 32);
    assert_eq!(lowered.memory_bytes, 64 * 1024);
    // No data sections, so the stored initial-memory span is empty.
    assert_eq!(lowered.initial_memory.len(), 0);
    assert_eq!(lowered.initial_memory_start, 0);
    // header (128) + code (32) + empty initial memory.
    assert_eq!(lowered.artifact.len(), 128 + 32);
    assert_eq!(&lowered.artifact[..8], b"GHRV32A1");
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[test]
fn lowers_a_stripped_elf() {
    // A stripped release build has no .symtab; v4 lowering must accept it
    // because the entry contract is e_entry == 0 plus the .text layout.
    let mut spec = happy_spec();
    spec.strip = true;
    spec.syms.clear();
    let lowered = lower(&assemble(spec)).expect("stripped elf lowers");
    assert_eq!(lowered.code.len(), 32);
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[test]
fn copies_data_sections_into_the_memory_image() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".rodata",
        SHF_ALLOC,
        0x100,
        vec![0xDE, 0xAD, 0xBE, 0xEF],
    ));
    let lowered = lower(&assemble(spec)).expect("lowers with rodata");
    // The stored image is the minimal nonzero span, staged at its start VA — not
    // a prefix from address 0.
    assert_eq!(lowered.initial_memory_start, 0x100);
    assert_eq!(lowered.initial_memory, [0xDE, 0xAD, 0xBE, 0xEF]);
    // header + code + the 4-byte span: no leading-zero shadow.
    assert_eq!(lowered.artifact.len(), 128 + 32 + 4);
}

#[test]
fn metadata_has_sorted_keys_and_expected_shape() {
    let elf = assemble(happy_spec());
    let lowered = lower(&elf).expect("lowers");
    let document = render_metadata(&MetadataInput {
        elf: &elf,
        artifact: &lowered.artifact,
        code: &lowered.code,
        initial_memory: &lowered.initial_memory,
        initial_memory_start: lowered.initial_memory_start,
        memory_bytes: lowered.memory_bytes,
        rustc_version: None,
        target_triple: Some("riscv32im-unknown-none-elf".to_string()),
        build_flags: None,
    });

    assert!(document.ends_with('\n'), "trailing newline");
    assert!(document.contains("\"code_bytes\": 32"));
    assert!(document.contains(&format!(
        "\"artifact_format_fingerprint\": \"{FORMAT_FINGERPRINT:#018x}\""
    )));
    assert!(document.contains("\"rustc_version\": null"));
    assert!(document.contains("\"target_triple\": \"riscv32im-unknown-none-elf\""));
    // Keys are sorted at the top level.
    let artifact_bytes = document.find("artifact_bytes").expect("key present");
    let code_bytes = document.find("code_bytes").expect("key present");
    assert!(artifact_bytes < code_bytes, "keys not sorted");

    let parsed: serde_json::Value = serde_json::from_str(&document).expect("valid json");
    assert_eq!(parsed["code_bytes"], 32);
    assert_eq!(parsed["memory_bytes"], 64 * 1024);
    assert_eq!(parsed["initial_memory_bytes"], 0);
    assert_eq!(parsed["initial_memory_start"], 0);
    assert!(parsed.get("dispatch_entries").is_none());
    assert_eq!(
        parsed["artifact_format_fingerprint"],
        format!("{FORMAT_FINGERPRINT:#018x}")
    );
}

// ---------------------------------------------------------------------------
// Rejections.
// ---------------------------------------------------------------------------

#[test]
fn rejects_non_elf_input() {
    let err = lower(b"not an elf at all, much longer than four bytes padding..............")
        .expect_err("not an elf");
    assert!(err.to_string().contains("not an ELF"));
}

#[test]
fn rejects_text_not_at_vaddr_zero() {
    let mut spec = happy_spec();
    spec.text_addr = 4;
    expect_err(spec, ".text must start at vaddr 0");
}

#[test]
fn rejects_empty_text() {
    let mut spec = happy_spec();
    spec.text = Vec::new();
    expect_err(spec, ".text is empty");
}

#[test]
fn rejects_unaligned_text_size() {
    let mut spec = happy_spec();
    spec.text = vec![0u8; 30];
    expect_err(spec, ".text size must be four-byte aligned");
}

#[test]
fn rejects_relocations() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection {
        name: ".rela.text",
        sh_type: SHT_RELA,
        flags: 0,
        addr: 0,
        file_bytes: vec![0u8; 12],
        size_override: None,
    });
    expect_err(spec, "relocations are not allowed");
}

#[test]
fn rejects_unexpected_alloc_section() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".weird",
        SHF_ALLOC,
        0x1000,
        vec![0u8; 4],
    ));
    expect_err(spec, "unexpected alloc section");
}

#[test]
fn ignores_alloc_contract_spec_metadata_sections() {
    // Every raw version, not just the one this build's spec-tool reads: the
    // lowerer drops these bytes rather than decoding them, so a section
    // written by an older or newer SDK must still be stripped instead of
    // failing the lowering with "unexpected alloc section".
    for name in [
        "groundhogspecrawv0",
        groundhog_spec_tool::RAW_SECTION_NAME,
        "groundhogspecrawv99",
        "groundhogspecv0",
    ] {
        let mut spec = happy_spec();
        spec.extras.push(ExtraSection::progbits(
            name,
            SHF_ALLOC,
            0x1000,
            vec![1, 2, 3, 4],
        ));
        let lowered = lower(&assemble(spec)).expect("spec metadata section is ignored");

        // The spec section is not copied into VM memory, so it does not raise
        // the high-water mark: with no real data sections the prefix is empty.
        assert_eq!(lowered.initial_memory.len(), 0);
    }
}

#[test]
fn spec_metadata_section_names_match_spec_tool() {
    // The lowerer mirrors these names locally so the production binary keeps
    // no spec-tool dependency; pin them here (dev-dependency only) so a
    // rename in the SDK cannot silently leave spec bytes in a lowered image.
    assert_eq!(
        groundhog_rv32_lower::lower::RAW_SPEC_SECTION_PREFIX,
        groundhog_spec_tool::RAW_SECTION_PREFIX,
    );
    assert_eq!(
        groundhog_rv32_lower::lower::FINAL_SPEC_SECTION,
        groundhog_spec_tool::FINAL_SECTION_NAME,
    );
}

#[test]
fn rejects_executable_contract_spec_metadata_section() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        "groundhogspecrawv0",
        SHF_ALLOC | SHF_EXECINSTR,
        0x1000,
        vec![0u8; 4],
    ));
    expect_err(spec, "unexpected executable section");
}

#[test]
fn accepts_data_section_inside_the_code_pc_range() {
    // Harvard split: code (PC space) and data are separate VM memories, so a
    // data vaddr that numerically falls inside .text's PC range is legal. Here
    // .text spans [0, 0x20) and .rodata sits at 0x8, inside that range.
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".rodata",
        SHF_ALLOC,
        0x8,
        vec![0xAA, 0xBB, 0xCC, 0xDD],
    ));
    let lowered = lower(&assemble(spec)).expect("lowers with low-vaddr rodata");
    // The stored image is the minimal nonzero span: the null guard below it is
    // never stored, just implied by the start VA.
    assert_eq!(lowered.initial_memory_start, 0x8);
    assert_eq!(lowered.initial_memory, [0xAA, 0xBB, 0xCC, 0xDD]);
}

#[test]
fn stack_first_layout_stores_no_leading_zero_shadow() {
    // A stack-first guest layout parks the stack below the data sections, so
    // the first data section starts past the stack region (here 0x1008). The
    // stored image must be just the data span, not 4 KiB of stack zeros.
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".rodata",
        SHF_ALLOC,
        0x1008,
        vec![0x11, 0x22, 0x33, 0x44],
    ));
    let lowered = lower(&assemble(spec)).expect("lowers with stack-first layout");
    assert_eq!(lowered.initial_memory_start, 0x1008);
    assert_eq!(lowered.initial_memory, [0x11, 0x22, 0x33, 0x44]);
    assert_eq!(lowered.artifact.len(), 128 + 32 + 4);
}

#[test]
fn zero_bytes_are_trimmed_from_both_ends_of_the_span() {
    // Zero bytes at the edges of section content carry no information — the
    // runtime zero-fills everything outside the stored span anyway.
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".data",
        SHF_ALLOC,
        0x100,
        vec![0, 0, 0x5A, 0x5B, 0, 0],
    ));
    let lowered = lower(&assemble(spec)).expect("lowers");
    assert_eq!(lowered.initial_memory_start, 0x102);
    assert_eq!(lowered.initial_memory, [0x5A, 0x5B]);
}

#[test]
fn all_zero_data_section_stores_an_empty_image() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".data",
        SHF_ALLOC,
        0x100,
        vec![0u8; 16],
    ));
    let lowered = lower(&assemble(spec)).expect("lowers");
    assert_eq!(lowered.initial_memory_start, 0);
    assert_eq!(lowered.initial_memory.len(), 0);
}

#[test]
fn rejects_data_section_in_the_null_guard() {
    // Address 0 is reserved so a Rust &'static is never a readable null
    // pointer. Any data section reaching into [0, 8) is rejected — both
    // vaddr 0 and a vaddr inside the guard.
    for addr in [0, 4] {
        let mut spec = happy_spec();
        spec.extras.push(ExtraSection::progbits(
            ".rodata",
            SHF_ALLOC,
            addr,
            vec![0u8; 4],
        ));
        expect_err(spec, "reserved null guard");
    }
}

// ---------------------------------------------------------------------------
// Parity-rule & edge coverage (added after an adversarial review fleet flagged
// these branches as exercised only by the byte-golden, not independently).
// ---------------------------------------------------------------------------

#[test]
fn accepts_code_exactly_at_max() {
    let mut spec = happy_spec();
    spec.text = vec![0u8; 64 * 1024]; // exactly at the cap
    let lowered = lower(&assemble(spec)).expect("at-limit .text lowers");
    assert_eq!(lowered.code.len(), 64 * 1024);
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[test]
fn rejects_code_larger_than_max() {
    let mut spec = happy_spec();
    spec.text = vec![0u8; 64 * 1024 + 4]; // one instruction over the cap
    let elf = assemble(spec);
    let err = lower(&elf).expect_err("expected lowering to fail");
    assert!(matches!(
        err,
        groundhog_rv32_lower::LowerError::CodeTooLarge {
            size: 65540,
            max: 65536,
        }
    ));
    // The message reports the actual size, the cap, and the overage.
    let message = err.to_string();
    assert!(message.contains(".text is 65540 bytes"), "{message:?}");
    assert!(message.contains("65536 bytes"), "{message:?}");
    assert!(message.contains("(4 bytes over)"), "{message:?}");
}

#[test]
fn rejects_multiple_text_sections() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".text",
        SHF_ALLOC | SHF_EXECINSTR,
        0x100,
        vec![0u8; 4],
    ));
    expect_err(spec, "expected exactly one .text section");
}

#[test]
fn rejects_data_sections_overlapping_each_other() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".rodata",
        SHF_ALLOC,
        0x100,
        vec![0u8; 0x100],
    ));
    spec.extras.push(ExtraSection::progbits(
        ".data",
        SHF_ALLOC,
        0x180,
        vec![0u8; 0x100],
    ));
    expect_err(spec, "overlaps");
}

#[test]
fn accepts_data_section_ending_exactly_at_memory_limit() {
    let mut spec = happy_spec();
    // Ends at exactly 0x10000, the top of the 64 KiB data address space.
    spec.extras.push(ExtraSection::progbits(
        ".data",
        SHF_ALLOC,
        0xFF00,
        vec![0xA5; 0x100],
    ));
    let lowered = lower(&assemble(spec)).expect("at-limit data section lowers");
    assert_eq!(lowered.initial_memory_start, 0xFF00);
    assert_eq!(lowered.initial_memory.len(), 0x100);
}

#[test]
fn rejects_section_exceeding_memory_limit() {
    let mut spec = happy_spec();
    // Ends at 0x10100 (65792), past the 64 KiB image.
    spec.extras.push(ExtraSection::progbits(
        ".data",
        SHF_ALLOC,
        0xFF00,
        vec![0u8; 0x200],
    ));
    let elf = assemble(spec);
    let err = lower(&elf).expect_err("expected lowering to fail");
    assert!(matches!(
        &err,
        groundhog_rv32_lower::LowerError::DataTooLarge {
            section,
            end: 65792,
            max: 65536,
        } if section == ".data"
    ));
    // The message names the section and reports end address and overage.
    let message = err.to_string();
    assert!(
        message.contains("section .data ends at 65792"),
        "{message:?}"
    );
    assert!(message.contains("65536-byte"), "{message:?}");
    assert!(message.contains("by 256 bytes"), "{message:?}");
}

#[test]
fn rejects_bss_exceeding_memory_limit() {
    // NOBITS sections occupy address space even without file content, so an
    // oversized .bss (the common overflow under the stack-first layout, where
    // data sits above the stack reserve) is rejected by name too.
    let mut spec = happy_spec();
    // Ends at 0x11218 (70168), 4632 bytes past the 64 KiB image.
    spec.extras
        .push(ExtraSection::nobits(".bss", 0x1000, 0x10218));
    let elf = assemble(spec);
    let err = lower(&elf).expect_err("expected lowering to fail");
    let message = err.to_string();
    assert!(
        message.contains("section .bss ends at 70168"),
        "{message:?}"
    );
    assert!(message.contains("by 4632 bytes"), "{message:?}");
}

#[test]
fn handles_nobits_bss_section() {
    // NOBITS carries a size but no file content; it is not stored (the runtime
    // zero-fills it), so it does not raise the high-water mark. With only a
    // .bss the stored prefix is empty.
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::nobits(".bss", 0x200, 0x100));
    let lowered = lower(&assemble(spec)).expect("lowers with .bss");
    assert_eq!(lowered.initial_memory.len(), 0);
}

#[test]
fn rejects_sht_rel_relocations() {
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection {
        name: ".rel.text",
        sh_type: SHT_REL,
        flags: 0,
        addr: 0,
        file_bytes: vec![0u8; 8],
        size_override: None,
    });
    expect_err(spec, "relocations are not allowed");
}

#[test]
fn malformed_inputs_error_without_panicking() {
    let mut bytes_huge_shnum = assemble(happy_spec());
    bytes_huge_shnum[48] = 0xFF; // e_shnum -> absurd
    bytes_huge_shnum[49] = 0xFF;
    let cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"\x7fELF".to_vec(),
        vec![0u8; 60], // long enough, but bad magic / machine
        bytes_huge_shnum,
    ];
    for bytes in cases {
        assert!(
            lower(&bytes).is_err(),
            "expected Err for {}-byte input",
            bytes.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Artifact self-check (verify_artifact) failure paths.
// ---------------------------------------------------------------------------

#[test]
fn verify_artifact_rejects_corrupted_code_digest() {
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    bytes[64] ^= 0xFF; // first byte of the code digest (v5 offset 64)
    assert!(verify_artifact(&bytes).is_err());
}

#[test]
fn verify_artifact_rejects_non_canonical_layout() {
    // Section positions are implicit, so the only layout degree of freedom is
    // total length: header + code_size + initial_memory_size must equal the
    // file size exactly.
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    bytes.push(0); // trailing byte
    let err = verify_artifact(&bytes).expect_err("should reject");
    assert!(err.to_string().contains("non-canonical"));
}

#[test]
fn verify_artifact_rejects_nonzero_flags() {
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    bytes[24] = 1; // flags @ 24
    let err = verify_artifact(&bytes).expect_err("should reject");
    assert!(err.to_string().contains("flags"));
}

#[test]
fn build_artifact_rejects_code_outside_limits() {
    use groundhog_rv32_artifact::{MAX_CODE_BYTES, build_artifact};
    let memory: Vec<u8> = Vec::new();
    assert!(build_artifact(&[], &memory, 0).is_err()); // empty code
    let oversized = vec![0u8; MAX_CODE_BYTES + 4];
    assert!(build_artifact(&oversized, &memory, 0).is_err()); // > 64 KiB
}

#[test]
fn build_artifact_accepts_span_up_to_max_memory() {
    use groundhog_rv32_artifact::{MAX_MEMORY_BYTES, build_artifact};
    let code = vec![0u8; 4];
    // A short span and a full-size span are both valid; only past-the-end is not.
    assert!(build_artifact(&code, &[], 0).is_ok());
    assert!(build_artifact(&code, &vec![0u8; MAX_MEMORY_BYTES - 4], 4).is_ok());
    assert!(build_artifact(&code, &vec![0u8; MAX_MEMORY_BYTES], 0).is_ok());
    assert!(build_artifact(&code, &vec![0u8; MAX_MEMORY_BYTES + 4], 0).is_err());
}

#[test]
fn verify_artifact_rejects_oversized_code_size() {
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    // Overwrite code_size (u32 @ 52) with a value past MAX_CODE_BYTES.
    bytes[52..56].copy_from_slice(&0x0002_0000u32.to_le_bytes());
    let err = verify_artifact(&bytes).expect_err("should reject");
    assert!(err.to_string().contains("code size"));
}

#[test]
fn metadata_renders_null_build_info() {
    let elf = assemble(happy_spec());
    let lowered = lower(&elf).expect("lowers");
    let doc = render_metadata(&MetadataInput {
        elf: &elf,
        artifact: &lowered.artifact,
        code: &lowered.code,
        initial_memory: &lowered.initial_memory,
        initial_memory_start: lowered.initial_memory_start,
        memory_bytes: lowered.memory_bytes,
        rustc_version: None,
        target_triple: None,
        build_flags: None,
    });
    assert!(doc.contains("\"rustc_version\": null"));
    assert!(doc.contains("\"target_triple\": null"));
    assert!(doc.contains("\"build_flags\": null"));

    let parsed: serde_json::Value = serde_json::from_str(&doc).expect("valid json");
    assert!(parsed.get("dispatch_entries").is_none());
    assert_eq!(parsed["memory_bytes"], 64 * 1024);
}

// ---------------------------------------------------------------------------
// --json structured output (lib summary + end-to-end CLI).
// ---------------------------------------------------------------------------

#[test]
fn summary_json_has_the_expected_shape() {
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let rendered = groundhog_rv32_lower::summary_json("/tmp/x.rv32a", &lowered);
    let doc: serde_json::Value = serde_json::from_str(&rendered).expect("valid json");
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["out"], "/tmp/x.rv32a");
    assert_eq!(doc["code_bytes"], 32);
    assert_eq!(doc["memory_bytes"], 64 * 1024);
    assert_eq!(
        doc["artifact_format_fingerprint"],
        format!("{FORMAT_FINGERPRINT:#018x}")
    );
    assert!(doc["artifact_sha256"].as_str().is_some());
    assert_eq!(doc["initial_memory_bytes"], 0);
    assert_eq!(doc["initial_memory_start"], 0);
    assert!(doc.get("dispatch_entries").is_none());
}

#[test]
fn cli_json_flag_emits_structured_stdout() {
    use std::process::Command;
    let dir = std::env::temp_dir();
    let tag = std::process::id();
    let elf_path = dir.join(format!("rv32_lower_json_{tag}.elf"));
    let out_path = dir.join(format!("rv32_lower_json_{tag}.rv32a"));
    std::fs::write(&elf_path, assemble(happy_spec())).expect("write elf");
    let bin = env!("CARGO_BIN_EXE_rv32-lower");

    let ok = Command::new(bin)
        .args([
            "--elf",
            elf_path.to_str().unwrap(),
            "--out",
            out_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run rv32-lower");
    assert!(ok.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&ok.stdout).expect("stdout is json");
    assert_eq!(doc["status"], "ok");
    assert_eq!(doc["code_bytes"], 32);
    assert!(doc.get("dispatch_entries").is_none());

    // Error path: a nonexistent ELF still yields a JSON envelope on stdout.
    let err = Command::new(bin)
        .args([
            "--elf",
            "/nonexistent-rv32-lower-input",
            "--out",
            out_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("run rv32-lower");
    assert!(!err.status.success());
    let err_doc: serde_json::Value = serde_json::from_slice(&err.stdout).expect("stdout is json");
    assert_eq!(err_doc["status"], "error");
    assert!(err_doc["message"].as_str().is_some());

    let _ = std::fs::remove_file(&elf_path);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn cli_print_format_fingerprint_does_not_require_artifact_args() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_rv32-lower");

    let ok = Command::new(bin)
        .arg("--print-format-fingerprint")
        .output()
        .expect("run rv32-lower");
    assert!(ok.status.success());
    assert_eq!(
        String::from_utf8(ok.stdout).expect("stdout is utf8"),
        format!("{FORMAT_FINGERPRINT:#018x}\n")
    );
}

// ---------------------------------------------------------------------------
// Opt-in real-ELF golden check (skipped unless RV32_LOWER_PARITY_ELF is set).
// ---------------------------------------------------------------------------

#[test]
fn matches_golden_when_provided() {
    let Ok(elf_path) = std::env::var("RV32_LOWER_PARITY_ELF") else {
        eprintln!("skipping: RV32_LOWER_PARITY_ELF not set");
        return;
    };
    let elf = std::fs::read(&elf_path).expect("read parity ELF");
    let lowered = lower(&elf).expect("parity ELF lowers");
    if let Ok(golden_path) = std::env::var("RV32_LOWER_PARITY_GOLDEN") {
        let golden = std::fs::read(&golden_path).expect("read golden artifact");
        assert_eq!(
            lowered.artifact, golden,
            "artifact bytes diverge from the golden artifact"
        );
    }
}
