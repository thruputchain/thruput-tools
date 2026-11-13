#[cfg(unix)]
#[allow(dead_code)]
pub fn write_versioned_devnode(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let binary = dir.join("hog-devnode");
    std::fs::write(&binary, "#!/bin/sh\nprintf 'hog-devnode 0.1.12\\n'\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let contracts = dir.join("contracts");
    std::fs::create_dir(&contracts).unwrap();
    let artifact = thruput_rv32_artifact::build_artifact(
        &[0; 4],
        &[],
        0,
        thruput_rv32_artifact::DEFAULT_MEMORY_BYTES,
    )
    .unwrap();
    std::fs::write(contracts.join("genesis_root.rv32a"), artifact).unwrap();
    binary
}

fn as_u32(value: usize) -> u32 {
    u32::try_from(value).expect("bug: test value fits in u32")
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

/// Minimal valid ELF32-LE RISC-V executable accepted by `thruput-rv32-lower`.
#[must_use]
#[allow(dead_code)]
pub fn minimal_rv32_elf() -> Vec<u8> {
    minimal_rv32_elf_with_optional_section(None)
}

#[must_use]
#[allow(dead_code)]
pub fn minimal_rv32_elf_with_section(section_name: &str, section_data: &[u8]) -> Vec<u8> {
    minimal_rv32_elf_with_optional_section(Some((section_name, section_data)))
}

/// Minimal raw contract spec with one anchored interface and one method.
#[must_use]
#[allow(dead_code)]
pub fn minimal_contract_spec_raw() -> Vec<u8> {
    contract_spec_raw_for_interface("Counter")
}

/// [`minimal_contract_spec_raw`] with its interface named `interface_name`.
#[must_use]
#[allow(dead_code)]
pub fn contract_spec_raw_for_interface(interface_name: &str) -> Vec<u8> {
    fn spec_string(value: &str) -> thruput_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test spec string should fit")
    }

    /// The crate that declares this interface. A bare `#[contract]` block
    /// records its crate rather than a trait, so the module path and the
    /// declared name are both empty -- see `crate_id_tokens` in the SDK.
    fn declarer() -> thruput_contract_spec::ContractSpecId {
        thruput_contract_spec::ContractSpecId {
            crate_name: spec_string("counter"),
            major: spec_string("0.1"),
            module_path: spec_string(""),
            declared: spec_string(""),
        }
    }

    let fragments = [
        thruput_spec_tool::SpecFragment::SpecInterface {
            interface_fragment: thruput_spec_tool::SpecInterfaceFragment {
                interface_name: spec_string(interface_name),
                interface_version: spec_string("0.1.0"),
                declared_by: declarer(),
                throws: vec![].try_into().unwrap(),
                emits: vec![].try_into().unwrap(),
            },
        },
        thruput_spec_tool::SpecFragment::InterfaceAnchor {
            interface_anchor: thruput_contract_spec::SpecInterfaceAnchor {
                interface_name: spec_string(interface_name),
                interface_version: spec_string("0.1.0"),
                declared_by: declarer(),
            },
        },
        thruput_spec_tool::SpecFragment::SpecMethod {
            method_fragment: thruput_spec_tool::SpecMethodFragment {
                interface_name: spec_string(interface_name),
                interface_version: spec_string("0.1.0"),
                declared_by: declarer(),
                method: thruput_spec_tool::ContractSpecMethod {
                    name: spec_string("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: thruput_spec_tool::ContractType::U64,
                    effect: None,
                },
            },
        },
    ];

    let mut bytes = Vec::new();
    for fragment in &fragments {
        bytes.extend(xdrs::txs::encode(fragment).expect("bug: test spec fragment should encode"));
    }
    bytes
}

#[allow(clippy::too_many_lines)]
fn minimal_rv32_elf_with_optional_section(extra: Option<(&str, &[u8])>) -> Vec<u8> {
    const SHT_PROGBITS: u32 = 1;
    const SHT_SYMTAB: u32 = 2;
    const SHT_STRTAB: u32 = 3;
    const SHF_ALLOC: u32 = 0x2;
    const SHF_EXECINSTR: u32 = 0x4;
    const STT_FUNC: u8 = 2;
    const STB_GLOBAL: u8 = 1;
    const EM_RISCV: u16 = 243;

    let text = [0x13, 0x00, 0x00, 0x00]; // nop

    // (name, flags, contents) of each section after `.text`. The SDK emits the
    // raw spec fragment as a `static`, which lands in an ALLOC section; mark
    // the fixture ALLOC so the lowering step in the build path exercises the
    // spec-metadata skip rather than the non-alloc fast path. Every SDK-built
    // guest also records the syscall API version it was built against (here
    // 1.1), in a section its linker script keeps out of guest memory.
    let syscall_api: [u8; 8] = [1, 0, 0, 0, 1, 0, 0, 0];
    let mut sections: Vec<(&str, u32, &[u8])> = Vec::new();
    if let Some((name, data)) = extra {
        sections.push((name, SHF_ALLOC, data));
    }
    sections.push((thruput_rv32_lower::SYSCALL_API_SECTION, 0, &syscall_api));

    let mut shstr = vec![0u8];
    let text_name = add_name(&mut shstr, ".text");
    let section_names: Vec<u32> = sections
        .iter()
        .map(|(name, _, _)| add_name(&mut shstr, name))
        .collect();
    let symtab_name = add_name(&mut shstr, ".symtab");
    let strtab_name = add_name(&mut shstr, ".strtab");
    let shstrtab_name = add_name(&mut shstr, ".shstrtab");

    let mut strtab = vec![0u8];
    let start_name = add_name(&mut strtab, "_start");
    let entry_name = add_name(&mut strtab, "0000000000000001");

    let mut symtab = vec![0u8; 16];
    for name in [start_name, entry_name] {
        symtab.extend_from_slice(&name.to_le_bytes());
        symtab.extend_from_slice(&0u32.to_le_bytes()); // st_value
        symtab.extend_from_slice(&4u32.to_le_bytes()); // st_size
        symtab.push((STB_GLOBAL << 4) | STT_FUNC);
        symtab.push(0); // st_other
        symtab.extend_from_slice(&1u16.to_le_bytes()); // .text
    }

    let mut file = vec![0u8; 52];
    let text_off = as_u32(file.len());
    file.extend_from_slice(&text);
    let section_offsets: Vec<u32> = sections
        .iter()
        .map(|(_, _, data)| {
            let offset = as_u32(file.len());
            file.extend_from_slice(data);
            offset
        })
        .collect();
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

    push_shdr(&mut file, 0, 0, 0, 0, 0, 0, 0, 0);
    push_shdr(
        &mut file,
        text_name,
        SHT_PROGBITS,
        SHF_ALLOC | SHF_EXECINSTR,
        0,
        text_off,
        as_u32(text.len()),
        0,
        0,
    );
    for ((_, flags, data), (&name, &offset)) in sections
        .iter()
        .zip(section_names.iter().zip(&section_offsets))
    {
        push_shdr(
            &mut file,
            name,
            SHT_PROGBITS,
            *flags,
            0,
            offset,
            as_u32(data.len()),
            0,
            0,
        );
    }
    let symtab_index = 2 + as_u32(sections.len());
    let strtab_index = symtab_index + 1;
    let shstrtab_index = strtab_index + 1;
    push_shdr(
        &mut file,
        symtab_name,
        SHT_SYMTAB,
        0,
        0,
        symtab_off,
        as_u32(symtab.len()),
        strtab_index,
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

    file[0..4].copy_from_slice(b"\x7fELF");
    file[4] = 1; // ELF32
    file[5] = 1; // little-endian
    file[6] = 1; // version
    file[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    file[18..20].copy_from_slice(&EM_RISCV.to_le_bytes());
    file[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
    file[24..28].copy_from_slice(&0u32.to_le_bytes()); // e_entry
    file[32..36].copy_from_slice(&e_shoff.to_le_bytes());
    file[40..42].copy_from_slice(&52u16.to_le_bytes());
    file[46..48].copy_from_slice(&40u16.to_le_bytes());
    file[48..50].copy_from_slice(
        &u16::try_from(shstrtab_index + 1)
            .expect("bug: test section count fits in u16")
            .to_le_bytes(),
    );
    file[50..52].copy_from_slice(
        &u16::try_from(shstrtab_index)
            .expect("bug: test section index fits in u16")
            .to_le_bytes(),
    );
    file
}
