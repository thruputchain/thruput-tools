#[cfg(unix)]
#[allow(dead_code)]
pub fn write_versioned_devnode(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let binary = dir.join("hog-devnode");
    std::fs::write(&binary, "#!/bin/sh\nprintf 'hog-devnode 0.1.12\\n'\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let contracts = dir.join("contracts");
    std::fs::create_dir(&contracts).unwrap();
    let artifact = groundhog_rv32_artifact::build_artifact(
        &[0; 4],
        &[],
        0,
        groundhog_rv32_artifact::DEFAULT_MEMORY_BYTES,
    )
    .unwrap();
    std::fs::write(contracts.join("dev_wallet.rv32a"), artifact).unwrap();
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

/// Minimal valid ELF32-LE RISC-V executable accepted by `groundhog-rv32-lower`.
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
    fn spec_string(value: &str) -> groundhog_spec_tool::ContractSpecString {
        value.try_into().expect("bug: test spec string should fit")
    }

    let fragments = [
        groundhog_spec_tool::SpecFragment::SpecInterface {
            interface_fragment: groundhog_spec_tool::SpecInterfaceFragment {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
                throws: vec![].try_into().unwrap(),
                emits: vec![].try_into().unwrap(),
            },
        },
        groundhog_spec_tool::SpecFragment::InterfaceAnchor {
            interface_anchor: groundhog_contract_spec::SpecInterfaceAnchor {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
            },
        },
        groundhog_spec_tool::SpecFragment::SpecMethod {
            method_fragment: groundhog_spec_tool::SpecMethodFragment {
                interface_name: spec_string("Counter"),
                interface_version: spec_string("0.1.0"),
                method: groundhog_spec_tool::ContractSpecMethod {
                    name: spec_string("get"),
                    args: vec![].try_into().unwrap(),
                    return_type: groundhog_spec_tool::ContractType::U64,
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

    let mut shstr = vec![0u8];
    let text_name = add_name(&mut shstr, ".text");
    let extra_name = extra.map(|(name, _)| add_name(&mut shstr, name));
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
    let extra_off = extra.map(|(_, data)| {
        let offset = as_u32(file.len());
        file.extend_from_slice(data);
        offset
    });
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
    if let Some(name) = extra_name {
        let (_, data) = extra.expect("bug: extra name implies extra data");
        // The SDK emits the raw spec fragment as a `static`, which lands in an
        // ALLOC section. Mark the fixture ALLOC so the lowering step in the
        // build path exercises the spec-metadata skip rather than the
        // non-alloc fast path.
        push_shdr(
            &mut file,
            name,
            SHT_PROGBITS,
            SHF_ALLOC,
            0,
            extra_off.expect("bug: extra data has an offset"),
            as_u32(data.len()),
            0,
            0,
        );
    }
    let symtab_index: u32 = if extra.is_some() { 3 } else { 2 };
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
