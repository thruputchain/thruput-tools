//! Hermetic tests for the rv32 lowerer.
//!
//! Most tests build a minimal synthetic ELF32 in-process (no riscv toolchain
//! required) and exercise the happy path plus every validation/rejection rule.
//! A final opt-in test (`RV32_LOWER_PARITY_ELF`) lowers a real contract ELF and
//! optionally byte-compares it against a known-good golden artifact — an
//! opt-in / out-of-default-CI regression check.

// The synthetic-ELF builder favors owned specs that tests clone-and-mutate.
#![allow(clippy::needless_pass_by_value)]

use thruput_rv32_artifact::{FORMAT_FINGERPRINT, verify_artifact};
use thruput_rv32_lower::{
    ALLOC_ALLOWLIST, INTERFACE_ID_BYTES, INTERFACE_TABLE_ANCHOR_SECTION, INTERFACE_TABLE_SECTION,
    MetadataInput, SYSCALL_API_SECTION, SyscallApiVersion, lower, render_metadata,
};

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
///
/// Adds the syscall API version record every SDK-built guest carries, unless
/// the spec supplies its own [`SYSCALL_API_SECTION`].
fn assemble(mut spec: ElfSpec) -> Vec<u8> {
    if !spec.extras.iter().any(|e| e.name == SYSCALL_API_SECTION) {
        spec.extras.push(syscall_api_record(1, 1));
    }
    assemble_exactly(spec)
}

/// The non-alloc record section `thruput-sdk` emits: major then minor.
fn syscall_api_record(major: u32, minor: u32) -> ExtraSection {
    let mut bytes = major.to_le_bytes().to_vec();
    bytes.extend_from_slice(&minor.to_le_bytes());
    ExtraSection::progbits(SYSCALL_API_SECTION, 0, 0, bytes)
}

/// [`assemble`] without the implied syscall API version record.
#[allow(clippy::too_many_lines)]
fn assemble_exactly(spec: ElfSpec) -> Vec<u8> {
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

fn nops(words: usize) -> Vec<u8> {
    0x0000_0013u32.to_le_bytes().repeat(words) // ADDI x0, x0, 0 (NOP)
}

/// A baseline valid contract: 32-byte `.text`, `_start` at 0, two method
/// entry points (method ids 0xAA @ pc 4 and 0x10 @ pc 16).
fn happy_spec() -> ElfSpec {
    ElfSpec {
        text: nops(8),
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
    // header (120) + code (32) + empty initial memory + empty extension list (4).
    assert_eq!(lowered.artifact.len(), 120 + 32 + 4);
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
    // header + code + the 4-byte span + empty extension list: no leading-zero shadow.
    assert_eq!(lowered.artifact.len(), 120 + 32 + 4 + 4);
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
fn rejects_compressed_instructions_at_every_code_boundary() {
    // The total remains four-byte aligned, even with two 16-bit encodings.
    // Neither stripped symbols nor a missing RVC flag may hide the encoding.
    for compressed in [[0x00, 0x00], [0x01, 0x00], [0x02, 0x90]] {
        for pc in [0, 4, 28] {
            for flags in [0u32, 1] {
                let mut spec = happy_spec();
                spec.strip = true;
                spec.text[pc..pc + 4].copy_from_slice(&compressed.repeat(2));
                let mut bytes = assemble(spec);
                bytes[36..40].copy_from_slice(&flags.to_le_bytes());
                let err = lower(&bytes).expect_err("compressed code must not lower");
                assert!(err.to_string().contains("compressed instruction"), "{err}");
                assert!(err.to_string().contains(&format!("PC {pc:#x}")), "{err}");
            }
        }
    }
}

#[test]
fn preserves_rv32im_instructions_and_canonical_unimp() {
    let words: [u32; 6] = [
        0x0000_0013, // NOP
        0x0000_8067, // RET
        0x02b5_0533, // MUL a0, a0, a1
        0x0000_0073, // ECALL
        0x0010_0073, // EBREAK
        0xc000_1073, // canonical UNIMP, an intentional runtime trap
    ];
    let code: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
    let mut spec = happy_spec();
    spec.text.clone_from(&code);
    assert_eq!(lower(&assemble(spec)).expect("RV32IM lowers").code, code);
}

#[test]
fn rejects_text_without_file_contents() {
    for section_type in [SHT_NOBITS, 0] {
        let mut bytes = assemble(happy_spec());
        let shoff = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
        // Keep valid instructions at sh_offset: reading those incidental
        // bytes must not make a non-PROGBITS .text acceptable.
        let text_type_offset = shoff + 40 + 4;
        bytes[text_type_offset..text_type_offset + 4].copy_from_slice(&section_type.to_le_bytes());
        let err = lower(&bytes).expect_err(".text must have file contents");
        assert!(err.to_string().contains("SHT_PROGBITS"), "{err}");
    }
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

// ---------------------------------------------------------------------------
// Interface table: in every ELF, in the image only when its anchor is.
// ---------------------------------------------------------------------------

/// Eight distinct 8-byte records, none starting with a zero byte so the
/// stored span begins exactly at the table.
fn distinct_records() -> Vec<u8> {
    (1u8..=64).collect()
}

fn interface_table_spec(with_anchor: Option<Vec<u8>>) -> ElfSpec {
    interface_table_spec_with(distinct_records(), with_anchor)
}

fn interface_table_spec_with(table: Vec<u8>, with_anchor: Option<Vec<u8>>) -> ElfSpec {
    let mut spec = happy_spec();
    let anchor_addr = 0x2000 + as_u32(table.len());
    spec.extras.push(ExtraSection::progbits(
        INTERFACE_TABLE_SECTION,
        SHF_ALLOC,
        0x2000,
        table,
    ));
    if let Some(anchor) = with_anchor {
        spec.extras.push(ExtraSection::progbits(
            INTERFACE_TABLE_ANCHOR_SECTION,
            SHF_ALLOC,
            anchor_addr,
            anchor,
        ));
    }
    spec
}

#[test]
fn drops_the_interface_table_without_its_anchor() {
    let lowered = lower(&assemble(interface_table_spec(None))).expect("lowers");
    assert!(
        lowered.initial_memory.is_empty(),
        "table bytes reached the image: {:?}",
        lowered.initial_memory
    );
}

#[test]
fn drops_the_interface_table_when_the_anchor_is_empty() {
    let lowered = lower(&assemble(interface_table_spec(Some(Vec::new())))).expect("lowers");
    assert!(lowered.initial_memory.is_empty());
}

#[test]
fn keeps_the_interface_table_with_its_anchor() {
    let lowered = lower(&assemble(interface_table_spec(Some(vec![1, 0, 0, 0])))).expect("lowers");
    assert_eq!(lowered.initial_memory_start, 0x2000);
    assert_eq!(&lowered.initial_memory[..64], &distinct_records()[..]);
    assert_eq!(&lowered.initial_memory[64..], &[1][..]);
    assert!(lowered.warnings.is_empty(), "{:?}", lowered.warnings);
}

#[test]
fn warns_once_per_duplicate_interface_id() {
    let mut table = distinct_records();
    table.extend_from_slice(&distinct_records()[8..16]); // record 1 again
    table.extend_from_slice(&distinct_records()[8..16]); // and again
    let lowered = lower(&assemble(interface_table_spec_with(
        table,
        Some(vec![1, 0, 0, 0]),
    )))
    .expect("lowers");
    // The warning is advisory: all ten records reach the image as linked.
    assert_eq!(lowered.initial_memory.len(), 10 * INTERFACE_ID_BYTES + 1);
    assert_eq!(lowered.warnings.len(), 1, "{:?}", lowered.warnings);
    assert!(
        lowered.warnings[0].contains("duplicate id 0x090a0b0c0d0e0f10"),
        "{:?}",
        lowered.warnings
    );
}

#[test]
fn checks_the_interface_table_even_when_dropping_it() {
    let mut table = distinct_records();
    table.extend_from_slice(&distinct_records()[..8]);
    let lowered = lower(&assemble(interface_table_spec_with(table, None))).expect("lowers");
    assert!(lowered.initial_memory.is_empty());
    assert_eq!(lowered.warnings.len(), 1, "{:?}", lowered.warnings);
}

#[test]
fn rejects_a_ragged_interface_table() {
    expect_err(
        interface_table_spec_with(vec![0xAB; 12], None),
        "is not a multiple of 8",
    );
}

#[test]
fn interface_table_sections_are_allowlisted() {
    assert!(ALLOC_ALLOWLIST.contains(&INTERFACE_TABLE_SECTION));
    assert!(ALLOC_ALLOWLIST.contains(&INTERFACE_TABLE_ANCHOR_SECTION));
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
        thruput_spec_tool::RAW_SECTION_NAME,
        "thruputspecrawv99",
        thruput_spec_tool::FINAL_SECTION_NAME,
        // Written by SDKs from before the `thruput` rename.
        "groundhogspecrawv0",
        "groundhogspecrawv4",
        "groundhogspecv0",
        "groundhogspecv2",
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
        thruput_rv32_lower::lower::RAW_SPEC_SECTION_PREFIX,
        thruput_spec_tool::RAW_SECTION_PREFIX,
    );
    assert_eq!(
        thruput_rv32_lower::lower::FINAL_SPEC_SECTION,
        thruput_spec_tool::FINAL_SECTION_NAME,
    );
    // The prefix has to actually cover the current name, or a bump would
    // silently stop stripping the section it is meant to catch.
    assert!(
        thruput_spec_tool::FINAL_SECTION_NAME
            .starts_with(thruput_rv32_lower::lower::FINAL_SPEC_SECTION_PREFIX),
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
    assert_eq!(lowered.artifact.len(), 120 + 32 + 4 + 4);
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
fn code_at_the_protocol_default_lowers_without_a_warning() {
    let mut spec = happy_spec();
    spec.text = nops(256 * 1024 / 4);
    let lowered = lower(&assemble(spec)).expect("at-default .text lowers");
    assert_eq!(lowered.code.len(), 256 * 1024);
    assert!(lowered.warnings.is_empty(), "{:?}", lowered.warnings);
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[test]
fn code_over_the_protocol_default_lowers_with_a_warning() {
    // The lowerer does not know the target chain, so code past the protocol
    // default is a warning, not a refusal: a chain configured above the
    // default admits it, and one that is not rejects it at deploy.
    let mut spec = happy_spec();
    spec.text = nops(256 * 1024 / 4 + 1);
    let lowered = lower(&assemble(spec)).expect("over-default .text lowers");
    assert_eq!(lowered.code.len(), 262_148);
    assert_eq!(
        u32::from_le_bytes(lowered.artifact[44..48].try_into().unwrap()),
        262_148,
        "code_size header field"
    );
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
    // The warning reports the actual size, the default, and the overage.
    assert_eq!(lowered.warnings.len(), 1, "{:?}", lowered.warnings);
    let warning = &lowered.warnings[0];
    assert!(
        warning.contains(".text is 262148 bytes, 4 over"),
        "{warning:?}"
    );
    assert!(warning.contains("262144-byte"), "{warning:?}");
    assert!(warning.contains("max_rv32_code_bytes"), "{warning:?}");
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
        thruput_rv32_lower::LowerError::DataTooLarge {
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
    // Section positions are implicit: the extension list runs from the end of
    // initial memory exactly to the end of the file.
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    bytes.push(0); // trailing byte
    let err = verify_artifact(&bytes).expect_err("should reject");
    assert!(err.to_string().contains("trailing bytes"), "got: {err}");
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
fn build_artifact_rejects_empty_code_only() {
    use thruput_rv32_artifact::{DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, build_artifact};
    let memory: Vec<u8> = Vec::new();
    assert!(build_artifact(&[], &memory, 0, DEFAULT_MEMORY_BYTES).is_err()); // empty code
    // The code limit is the lowering's, not the format's: the writer emits
    // code past the protocol default for a chain that admits it.
    let oversized = vec![0u8; DEFAULT_MAX_CODE_BYTES + 4];
    assert!(build_artifact(&oversized, &memory, 0, DEFAULT_MEMORY_BYTES).is_ok());
}

#[test]
fn build_artifact_accepts_span_up_to_memory_bytes() {
    use thruput_rv32_artifact::{DEFAULT_MEMORY_BYTES, build_artifact};
    let code = vec![0u8; 4];
    let m = DEFAULT_MEMORY_BYTES;
    // A short span and a full-size span are both valid; only past-the-end is not.
    assert!(build_artifact(&code, &[], 0, m).is_ok());
    assert!(build_artifact(&code, &vec![0u8; m - 4], 4, m).is_ok());
    assert!(build_artifact(&code, &vec![0u8; m], 0, m).is_ok());
    assert!(build_artifact(&code, &vec![0u8; m + 4], 0, m).is_err());
    // ... unless the caller declares a larger address space.
    assert!(build_artifact(&code, &vec![0u8; m + 4], 0, m + 4).is_ok());
}

#[test]
fn verify_artifact_rejects_code_size_disagreeing_with_the_body() {
    let lowered = lower(&assemble(happy_spec())).expect("lowers");
    let mut bytes = lowered.artifact.clone();
    // Overwrite code_size (u32 @ 44) with a value the body does not carry. No
    // fixed cap applies any more; the layout check is what refuses it.
    bytes[44..48].copy_from_slice(&0x0002_0000u32.to_le_bytes());
    let err = verify_artifact(&bytes).expect_err("should reject");
    assert!(err.to_string().contains("non-canonical"), "got: {err}");
}

#[test]
fn lower_with_stamps_the_requested_memory() {
    use thruput_rv32_artifact::DEFAULT_MEMORY_BYTES;
    use thruput_rv32_lower::{LowerOptions, lower_with};
    let elf = assemble(happy_spec());
    let default = lower(&elf).expect("lowers");

    // A raised memory size is stamped into the header and reported back.
    let raised = LowerOptions {
        memory_bytes: 2 * DEFAULT_MEMORY_BYTES,
    };
    let lowered = lower_with(&elf, &raised).expect("lowers under a raised memory size");
    assert_eq!(lowered.memory_bytes, 2 * DEFAULT_MEMORY_BYTES);
    assert_eq!(
        u32::from_le_bytes(lowered.artifact[40..44].try_into().unwrap()) as usize,
        2 * DEFAULT_MEMORY_BYTES
    );
    assert_eq!(lowered.code, default.code);
    assert_eq!(lowered.initial_memory, default.initial_memory);
    assert!(lowered.warnings.is_empty(), "{:?}", lowered.warnings);

    let zero = LowerOptions { memory_bytes: 0 };
    assert!(lower_with(&elf, &zero).is_err());
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
    let rendered = thruput_rv32_lower::summary_json("/tmp/x.rv32a", &lowered);
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

#[test]
fn a_large_memory_size_stages_only_the_placed_data() {
    use thruput_rv32_lower::{LowerOptions, lower_with};
    let elf = assemble(happy_spec());
    let default = lower(&elf).expect("lowers");

    // Declaring 3 GiB of address space must not allocate 3 GiB to stage a
    // few bytes of data; the image is the same span as under the default.
    let huge = LowerOptions {
        memory_bytes: 3 << 30,
    };
    let lowered = lower_with(&elf, &huge).expect("lowers under a large memory size");
    assert_eq!(lowered.memory_bytes, 3 << 30);
    assert_eq!(lowered.initial_memory, default.initial_memory);
    assert_eq!(lowered.initial_memory_start, default.initial_memory_start);
}

#[test]
fn a_high_data_section_stages_only_its_own_span() {
    use thruput_rv32_lower::{LowerOptions, lower_with};
    // A few bytes placed near the top of a 3 GiB address space must stage
    // neither the gigabytes below them nor the space above; the image is the
    // nonzero span at its own address.
    let addr = (3u32 << 30) - 0x1000;
    let mut spec = happy_spec();
    spec.extras.push(ExtraSection::progbits(
        ".rodata",
        SHF_ALLOC,
        addr,
        vec![0, 7, 0, 9, 0],
    ));
    let lowered = lower_with(
        &assemble(spec),
        &LowerOptions {
            memory_bytes: 3 << 30,
        },
    )
    .expect("lowers with a high data section");
    assert_eq!(lowered.initial_memory, [7, 0, 9]);
    assert_eq!(lowered.initial_memory_start, addr as usize + 1);
    assert_eq!(lowered.artifact.len(), 120 + 32 + 3 + 4);
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[cfg(target_pointer_width = "64")]
#[test]
fn rejects_a_memory_size_past_the_header_field() {
    use thruput_rv32_lower::{LowerError, LowerOptions, lower_with};
    let elf = assemble(happy_spec());
    let past = LowerOptions {
        memory_bytes: u32::MAX as usize + 1,
    };
    let err = lower_with(&elf, &past).expect_err("cannot be stamped");
    assert!(
        matches!(err, LowerError::Invalid(ref message) if message.contains("32-bit")),
        "{err}"
    );
}

fn run_cli(
    elf: &std::path::Path,
    out: &std::path::Path,
    metadata: Option<&std::path::Path>,
    json: bool,
) -> std::process::Output {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_rv32-lower"));
    command.arg("--elf").arg(elf).arg("--out").arg(out);
    if let Some(path) = metadata {
        command.arg("--metadata").arg(path);
    }
    if json {
        command.arg("--json");
    }
    command.output().expect("run rv32-lower")
}

fn assert_cli_error(output: &std::process::Output, json: bool, needle: &str) {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    let message = if json {
        assert!(output.stderr.is_empty(), "{output:?}");
        let doc: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON error");
        assert_eq!(doc["status"], "error");
        doc["message"].as_str().expect("error message").to_string()
    } else {
        assert!(output.stdout.is_empty(), "{output:?}");
        String::from_utf8(output.stderr.clone()).expect("plain error")
    };
    assert!(message.contains(needle), "{message}");
}

#[test]
fn cli_rejects_bad_code_before_touching_outputs() {
    for json in [false, true] {
        for existing in [false, true] {
            for nobits in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let elf = dir.path().join("contract.elf");
                let out = dir.path().join("contract.rv32a");
                let metadata = dir.path().join("contract.json");
                let mut bytes = assemble(happy_spec());
                let shoff = u32::from_le_bytes(bytes[32..36].try_into().unwrap()) as usize;
                let needle = if nobits {
                    let text_type_offset = shoff + 40 + 4;
                    bytes[text_type_offset..text_type_offset + 4]
                        .copy_from_slice(&SHT_NOBITS.to_le_bytes());
                    "SHT_PROGBITS"
                } else {
                    let text_offset_field = shoff + 40 + 16;
                    let offset = u32::from_le_bytes(
                        bytes[text_offset_field..text_offset_field + 4]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    bytes[offset..offset + 4].copy_from_slice(&[1, 0, 1, 0]);
                    "compressed instruction"
                };
                std::fs::write(&elf, &bytes).unwrap();
                if existing {
                    std::fs::write(&out, b"previous artifact").unwrap();
                    std::fs::write(&metadata, b"previous metadata").unwrap();
                }
                assert_cli_error(&run_cli(&elf, &out, Some(&metadata), json), json, needle);
                assert_eq!(std::fs::read(&elf).unwrap(), bytes);
                if existing {
                    assert_eq!(std::fs::read(&out).unwrap(), b"previous artifact");
                    assert_eq!(std::fs::read(&metadata).unwrap(), b"previous metadata");
                } else {
                    assert!(!out.exists());
                    assert!(!metadata.exists());
                }
            }
        }
    }
}

#[test]
fn cli_rejects_identical_and_normalized_output_paths() {
    for json in [false, true] {
        for existing in [false, true] {
            for normalized in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let elf = dir.path().join("contract.elf");
                let out = dir.path().join("contract.rv32a");
                std::fs::write(&elf, assemble(happy_spec())).unwrap();
                if existing {
                    std::fs::write(&out, b"previous artifact").unwrap();
                }
                let metadata = if normalized {
                    std::fs::create_dir(dir.path().join("sub")).unwrap();
                    dir.path().join("sub/../contract.rv32a")
                } else {
                    out.clone()
                };
                assert_cli_error(
                    &run_cli(&elf, &out, Some(&metadata), json),
                    json,
                    "--metadata and --out refer to the same file",
                );
                // Opening a new destination can create it, but it must never
                // contain either of the two conflicting payloads.
                assert_eq!(
                    std::fs::read(&out).unwrap(),
                    if existing {
                        b"previous artifact".as_slice()
                    } else {
                        &[]
                    },
                );
            }
        }
    }
}

#[test]
fn cli_rejects_outputs_aliasing_the_input() {
    for metadata_alias in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("contract.elf");
        let out = dir.path().join("contract.rv32a");
        let bytes = assemble(happy_spec());
        std::fs::write(&elf, &bytes).unwrap();
        std::fs::write(&out, b"previous artifact").unwrap();
        let result = if metadata_alias {
            run_cli(&elf, &out, Some(&elf), true)
        } else {
            run_cli(&elf, &elf, None, true)
        };
        assert_cli_error(&result, true, "--elf refer to the same file");
        assert_eq!(std::fs::read(&elf).unwrap(), bytes);
        assert_eq!(std::fs::read(&out).unwrap(), b"previous artifact");
    }
}

#[cfg(unix)]
#[test]
fn cli_rejects_symlink_and_hard_link_aliases() {
    for symlink in [false, true] {
        // Each of the three path pairs can alias the same underlying file.
        for pair in [0, 1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let elf = dir.path().join("contract.elf");
            let out = dir.path().join("contract.rv32a");
            let metadata = dir.path().join("contract.json");
            let bytes = assemble(happy_spec());
            std::fs::write(&elf, &bytes).unwrap();
            let (source, alias) = match pair {
                0 => (&elf, &out),
                1 => (&elf, &metadata),
                _ => (&out, &metadata),
            };
            if pair != 0 {
                std::fs::write(&out, b"previous artifact").unwrap();
            }
            if symlink {
                std::os::unix::fs::symlink(source, alias).unwrap();
            } else {
                std::fs::hard_link(source, alias).unwrap();
            }
            assert_cli_error(
                &run_cli(&elf, &out, Some(&metadata), true),
                true,
                "refer to the same file",
            );
            assert_eq!(std::fs::read(&elf).unwrap(), bytes);
            if pair != 0 {
                assert_eq!(std::fs::read(&out).unwrap(), b"previous artifact");
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn cli_rejects_dangling_symlink_output_aliases() {
    for reverse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("contract.elf");
        let out = dir.path().join("contract.rv32a");
        let alias = dir.path().join("alias");
        std::fs::write(&elf, assemble(happy_spec())).unwrap();
        std::os::unix::fs::symlink(&out, &alias).unwrap();
        let (out, metadata) = if reverse {
            (&alias, &out)
        } else {
            (&out, &alias)
        };
        assert_cli_error(
            &run_cli(&elf, out, Some(metadata), true),
            true,
            "--metadata and --out refer to the same file",
        );
        assert!(std::fs::read(out).unwrap().is_empty());
    }
}

#[test]
fn cli_emits_matching_artifact_metadata_and_summary() {
    for existing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("contract.elf");
        let out = dir.path().join("contract.rv32a");
        let metadata = dir.path().join("contract.json");
        let bytes = assemble(happy_spec());
        std::fs::write(&elf, &bytes).unwrap();
        if existing {
            // Rebuilding shorter outputs must truncate stale tails.
            std::fs::write(&out, vec![0xff; 4096]).unwrap();
            std::fs::write(&metadata, vec![0xff; 4096]).unwrap();
        }
        let output = run_cli(&elf, &out, Some(&metadata), true);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let artifact = std::fs::read(&out).unwrap();
        verify_artifact(&artifact).unwrap();
        assert_eq!(artifact, lower(&bytes).unwrap().artifact);
        assert_eq!(std::fs::read(&elf).unwrap(), bytes);
        let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metadata).unwrap()).unwrap();
        let hash = thruput_rv32_artifact::sha256_hex(&artifact);
        assert_eq!(summary["artifact_sha256"], hash);
        assert_eq!(document["artifact_sha256"], hash);
        assert_eq!(
            document["elf_sha256"],
            thruput_rv32_artifact::sha256_hex(&bytes)
        );
        assert_eq!(summary["artifact_bytes"], artifact.len());
        assert_eq!(document["artifact_bytes"], artifact.len());
    }
}

#[test]
fn cli_preserves_existing_artifact_when_metadata_cannot_be_opened() {
    let dir = tempfile::tempdir().unwrap();
    let elf = dir.path().join("contract.elf");
    let out = dir.path().join("contract.rv32a");
    let metadata = dir.path().join("missing-parent/contract.json");
    std::fs::write(&elf, assemble(happy_spec())).unwrap();
    std::fs::write(&out, b"previous artifact").unwrap();
    assert_cli_error(&run_cli(&elf, &out, Some(&metadata), true), true, "");
    assert_eq!(std::fs::read(&out).unwrap(), b"previous artifact");
}

// ---------------------------------------------------------------------------
// Syscall API version record.
// ---------------------------------------------------------------------------

#[test]
fn stamps_the_syscall_api_version_the_elf_records() {
    use thruput_rv32_artifact::declared_syscall_api;

    let mut spec = happy_spec();
    spec.extras.push(syscall_api_record(1, 7));
    let lowered = lower(&assemble(spec)).expect("lowers");
    let version = SyscallApiVersion { major: 1, minor: 7 };
    assert_eq!(lowered.syscall_api, version);
    assert_eq!(declared_syscall_api(&lowered.artifact), Some(version));
    // Major then minor, directly before memory_bytes.
    assert_eq!(lowered.artifact[12..16], 120u32.to_le_bytes());
    assert_eq!(lowered.artifact[32..36], 1u32.to_le_bytes());
    assert_eq!(lowered.artifact[36..40], 7u32.to_le_bytes());
    verify_artifact(&lowered.artifact).expect("artifact self-verifies");
}

#[test]
fn rejects_an_elf_without_a_syscall_api_record() {
    let err = lower(&assemble_exactly(happy_spec())).expect_err("no record");
    let message = err.to_string();
    assert!(message.contains(SYSCALL_API_SECTION), "{message}");
    assert!(message.contains("thruput-sdk 0.61.1"), "{message}");
}

#[test]
fn rejects_a_malformed_syscall_api_record() {
    // Two SDKs linked in: two records in one section.
    let mut doubled = happy_spec();
    let mut record = syscall_api_record(1, 1);
    record
        .file_bytes
        .extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0]);
    doubled.extras.push(record);
    let message = lower(&assemble(doubled))
        .expect_err("two records")
        .to_string();
    assert!(message.contains("holds 16 bytes"), "{message}");

    let mut empty = happy_spec();
    let mut record = syscall_api_record(1, 1);
    record.file_bytes.clear();
    empty.extras.push(record);
    assert!(lower(&assemble(empty)).is_err());

    let mut twice = happy_spec();
    twice.extras.push(syscall_api_record(1, 1));
    twice.extras.push(syscall_api_record(1, 1));
    let message = lower(&assemble(twice))
        .expect_err("two sections")
        .to_string();
    assert!(message.contains("exactly one"), "{message}");
}

#[test]
fn rejects_a_syscall_api_major_this_format_does_not_describe() {
    let mut spec = happy_spec();
    spec.extras.push(syscall_api_record(2, 0));
    let message = lower(&assemble(spec)).expect_err("major 2").to_string();
    assert!(message.contains("syscall API 2.0"), "{message}");
}
