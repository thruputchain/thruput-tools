//! The Groundhog RV32 **v3 artifact format**: constants, writer, and
//! self-verifier.
//!
//! This is the single Rust source of truth for the on-disk format the hogd
//! RV32 VM loads. It mirrors `hogd/hogexec/runtime/rv32/artifact.h`; the
//! values below are the load-bearing contract and must stay in lockstep with
//! that header. (The author-toolchain plan slates this module to split out to
//! its own `groundhog-rv32-artifact` crate next to `groundhog-xdr`, with hogd
//! cross-checking the digests at build time. It lives here for now.)
//!
//! Header layout (little-endian, no padding, 172 bytes total):
//!
//! ```text
//!   off  size  field
//!     0     8  magic "GHRV32A1"
//!     8     4  format_version          (3)
//!    12     4  header_bytes            (172)
//!    16     8  vm_profile_digest
//!    24     4  flags                   (0)
//!    28     4  entry_abi_version       (1)
//!    32     4  syscall_abi_version     (1)
//!    36     4  gas_table_version       (2)
//!    40     8  gas_schedule_digest
//!    48     4  memory_bytes            (65536)
//!    52     4  code_size
//!    56     4  initial_memory_size
//!    60     4  code_offset
//!    64     4  initial_memory_offset
//!    68     4  dispatch_offset
//!    72     4  dispatch_size
//!    76    32  sha256(code)
//!   108    32  sha256(initial_memory)
//!   140    32  sha256(dispatch_table)
//! ```
//!
//! Body: `header || code || dispatch_table || initial_memory`.

use sha2::{Digest, Sha256};

use crate::error::{Result, invalid, verify};

/// Maximum size of the contract code (`.text`) section.
pub const MAX_CODE_BYTES: usize = 64 * 1024;
/// Size of the (always fully-materialized) initial-memory image.
pub const MAX_MEMORY_BYTES: usize = 64 * 1024;
/// RV32 instruction width in bytes.
pub const INSTRUCTION_BYTES: usize = 4;
/// Artifact magic: `GHRV32A1`.
pub const MAGIC: [u8; 8] = *b"GHRV32A1";
/// Artifact format version.
pub const FORMAT_VERSION: u32 = 3;
/// Fixed header size in bytes.
pub const HEADER_BYTES: usize = 172;
/// VM-profile digest the runtime validates the artifact against.
pub const VM_PROFILE_DIGEST: u64 = 0x55c5_3a68_5f0e_b33d;
/// Entry ABI version.
pub const ENTRY_ABI_VERSION: u32 = 1;
/// Syscall ABI version (1 = register-native: args in a0..a7, no frame).
pub const SYSCALL_ABI_VERSION: u32 = 1;
/// Size of one packed dispatch-table entry.
pub const DISPATCH_ENTRY_BYTES: usize = 16;
/// Offset of the method id inside one packed dispatch-table entry.
pub const DISPATCH_ENTRY_METHOD_OFFSET: usize = 0;
/// Offset of the code pc inside one packed dispatch-table entry.
pub const DISPATCH_ENTRY_PC_OFFSET: usize = 8;
/// Offset of the reserved field inside one packed dispatch-table entry.
pub const DISPATCH_ENTRY_RESERVED_OFFSET: usize = 12;
/// Maximum number of dispatch entries accepted by the runtime.
pub const MAX_DISPATCH_ENTRIES: usize = MAX_CODE_BYTES / INSTRUCTION_BYTES;
/// Maximum dispatch-table size accepted by the runtime.
pub const MAX_DISPATCH_BYTES: usize = MAX_DISPATCH_ENTRIES * DISPATCH_ENTRY_BYTES;
/// Gas-schedule table version.
pub const GAS_TABLE_VERSION: u32 = 2;
/// Gas-schedule consensus digest (folds in instruction + host-syscall costs).
pub const GAS_SCHEDULE_DIGEST: u64 = 0xebe5_3dce_ea58_1d0c;
/// Digest size of each section hash stamped into the artifact header.
pub const DIGEST_BYTES: usize = 32;

/// Fixed v3 header offsets.
pub const FORMAT_VERSION_OFFSET: usize = 8;
pub const HEADER_SIZE_OFFSET: usize = 12;
pub const VM_PROFILE_OFFSET: usize = 16;
pub const FLAGS_OFFSET: usize = 24;
pub const ENTRY_ABI_OFFSET: usize = 28;
pub const SYSCALL_ABI_OFFSET: usize = 32;
pub const GAS_VERSION_OFFSET: usize = 36;
pub const GAS_DIGEST_OFFSET: usize = 40;
pub const MEMORY_BYTES_OFFSET: usize = 48;
pub const CODE_SIZE_OFFSET: usize = 52;
pub const INITIAL_MEMORY_SIZE_OFFSET: usize = 56;
pub const CODE_OFFSET_OFFSET: usize = 60;
pub const INITIAL_MEMORY_OFFSET_OFFSET: usize = 64;
pub const DISPATCH_OFFSET_OFFSET: usize = 68;
pub const DISPATCH_SIZE_OFFSET: usize = 72;
pub const CODE_DIGEST_OFFSET: usize = 76;
pub const INITIAL_MEMORY_DIGEST_OFFSET: usize = CODE_DIGEST_OFFSET + DIGEST_BYTES;
pub const DISPATCH_DIGEST_OFFSET: usize = INITIAL_MEMORY_DIGEST_OFFSET + DIGEST_BYTES;

const _: () = assert!(DISPATCH_DIGEST_OFFSET + DIGEST_BYTES == HEADER_BYTES);

const FNV_OFFSET_BASIS: u64 = 1_469_598_103_934_665_603;
const FNV_PRIME: u64 = 1_099_511_628_211;
const FORMAT_FINGERPRINT_DOMAIN: &[u8] = b"groundhog.rv32.artifact.format.v3";
const SECTION_DIGEST_ALGORITHM: &[u8] = b"sha256";
const CANONICAL_SECTION_ORDER: &[u8] = b"header|code|dispatch|initial_memory";
const FLAGS_REQUIRED_VALUE: u32 = 0;

/// Pinned v3 artifact-format contract fingerprint.
pub const FORMAT_FINGERPRINT: u64 = 0x3892_7fa2_342c_674a;
const _: () = assert!(compute_format_fingerprint() == FORMAT_FINGERPRINT);

const fn fnv_digest_byte(digest: u64, value: u8) -> u64 {
    (digest ^ value as u64).wrapping_mul(FNV_PRIME)
}

const fn fnv_digest_bytes(mut digest: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        digest = fnv_digest_byte(digest, bytes[i]);
        i += 1;
    }
    digest
}

const fn fnv_digest_u64(mut digest: u64, mut value: u64) -> u64 {
    let mut i = 0;
    while i < 8 {
        digest = fnv_digest_byte(digest, value as u8);
        value >>= 8;
        i += 1;
    }
    digest
}

const fn fnv_digest_name(digest: u64, name: &[u8]) -> u64 {
    fnv_digest_byte(fnv_digest_bytes(digest, name), 0)
}

const fn format_fingerprint_u64(digest: u64, name: &[u8], value: u64) -> u64 {
    fnv_digest_u64(fnv_digest_name(digest, name), value)
}

const fn format_fingerprint_bytes(digest: u64, name: &[u8], bytes: &[u8]) -> u64 {
    let digest = fnv_digest_name(digest, name);
    let digest = fnv_digest_u64(digest, bytes.len() as u64);
    fnv_digest_bytes(digest, bytes)
}

const fn fold_layout_field(
    digest: u64,
    offset_name: &[u8],
    size_name: &[u8],
    offset: usize,
    size: usize,
) -> u64 {
    let digest = format_fingerprint_u64(digest, offset_name, offset as u64);
    format_fingerprint_u64(digest, size_name, size as u64)
}

/// Compute the v3 artifact-format contract fingerprint.
#[must_use]
pub const fn compute_format_fingerprint() -> u64 {
    compute_format_fingerprint_with_inputs(
        FORMAT_FINGERPRINT_DOMAIN,
        &MAGIC,
        FORMAT_VERSION,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

/// Test hook for proving that a folded scalar perturbation moves the digest.
#[doc(hidden)]
#[must_use]
pub const fn compute_format_fingerprint_with_format_version(format_version: u32) -> u64 {
    compute_format_fingerprint_with_inputs(
        FORMAT_FINGERPRINT_DOMAIN,
        &MAGIC,
        format_version,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

/// Test hook for proving that a folded byte field perturbation moves the digest.
#[doc(hidden)]
#[must_use]
pub const fn compute_format_fingerprint_with_magic(magic: [u8; 8]) -> u64 {
    compute_format_fingerprint_with_inputs(
        FORMAT_FINGERPRINT_DOMAIN,
        &magic,
        FORMAT_VERSION,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

/// Test hook for proving that a folded name/domain perturbation moves the digest.
#[doc(hidden)]
#[must_use]
pub const fn compute_format_fingerprint_with_domain(domain: &[u8]) -> u64 {
    compute_format_fingerprint_with_inputs(
        domain,
        &MAGIC,
        FORMAT_VERSION,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

const fn compute_format_fingerprint_with_inputs(
    domain: &[u8],
    magic: &[u8; 8],
    format_version: u32,
    section_digest_algorithm: &[u8],
    canonical_section_order: &[u8],
) -> u64 {
    let mut digest = FNV_OFFSET_BASIS;
    digest = fnv_digest_name(digest, domain);

    digest = format_fingerprint_bytes(digest, b"magic", magic);
    digest = format_fingerprint_u64(digest, b"format_version", format_version as u64);
    digest = format_fingerprint_u64(digest, b"header_bytes", HEADER_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"vm_profile_digest", VM_PROFILE_DIGEST);
    digest = format_fingerprint_u64(digest, b"flags_required_value", FLAGS_REQUIRED_VALUE as u64);
    digest = format_fingerprint_u64(digest, b"entry_abi_version", ENTRY_ABI_VERSION as u64);
    digest = format_fingerprint_u64(digest, b"syscall_abi_version", SYSCALL_ABI_VERSION as u64);
    digest = format_fingerprint_u64(digest, b"gas_table_version", GAS_TABLE_VERSION as u64);
    digest = format_fingerprint_u64(digest, b"gas_schedule_digest", GAS_SCHEDULE_DIGEST);

    digest = fold_layout_field(
        digest,
        b"header.magic.offset",
        b"header.magic.size",
        0,
        magic.len(),
    );
    digest = fold_layout_field(
        digest,
        b"header.format_version.offset",
        b"header.format_version.size",
        FORMAT_VERSION_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.header_size.offset",
        b"header.header_size.size",
        HEADER_SIZE_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.vm_profile.offset",
        b"header.vm_profile.size",
        VM_PROFILE_OFFSET,
        8,
    );
    digest = fold_layout_field(
        digest,
        b"header.flags.offset",
        b"header.flags.size",
        FLAGS_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.entry_abi.offset",
        b"header.entry_abi.size",
        ENTRY_ABI_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.syscall_abi.offset",
        b"header.syscall_abi.size",
        SYSCALL_ABI_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.gas_version.offset",
        b"header.gas_version.size",
        GAS_VERSION_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.gas_digest.offset",
        b"header.gas_digest.size",
        GAS_DIGEST_OFFSET,
        8,
    );
    digest = fold_layout_field(
        digest,
        b"header.memory_bytes.offset",
        b"header.memory_bytes.size",
        MEMORY_BYTES_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.code_size.offset",
        b"header.code_size.size",
        CODE_SIZE_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.initial_memory_size.offset",
        b"header.initial_memory_size.size",
        INITIAL_MEMORY_SIZE_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.code_offset.offset",
        b"header.code_offset.size",
        CODE_OFFSET_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.initial_memory_offset.offset",
        b"header.initial_memory_offset.size",
        INITIAL_MEMORY_OFFSET_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.dispatch_offset.offset",
        b"header.dispatch_offset.size",
        DISPATCH_OFFSET_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.dispatch_size.offset",
        b"header.dispatch_size.size",
        DISPATCH_SIZE_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.code_digest.offset",
        b"header.code_digest.size",
        CODE_DIGEST_OFFSET,
        DIGEST_BYTES,
    );
    digest = fold_layout_field(
        digest,
        b"header.initial_memory_digest.offset",
        b"header.initial_memory_digest.size",
        INITIAL_MEMORY_DIGEST_OFFSET,
        DIGEST_BYTES,
    );
    digest = fold_layout_field(
        digest,
        b"header.dispatch_digest.offset",
        b"header.dispatch_digest.size",
        DISPATCH_DIGEST_OFFSET,
        DIGEST_BYTES,
    );

    digest = format_fingerprint_bytes(
        digest,
        b"section_digest_algorithm",
        section_digest_algorithm,
    );
    digest = format_fingerprint_u64(digest, b"digest_bytes", DIGEST_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"instruction_bytes", INSTRUCTION_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"max_code_bytes", MAX_CODE_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"max_memory_bytes", MAX_MEMORY_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"max_dispatch_entries", MAX_DISPATCH_ENTRIES as u64);
    digest = format_fingerprint_u64(digest, b"max_dispatch_bytes", MAX_DISPATCH_BYTES as u64);

    digest = format_fingerprint_u64(digest, b"dispatch_entry_bytes", DISPATCH_ENTRY_BYTES as u64);
    digest = fold_layout_field(
        digest,
        b"dispatch.method_id.offset",
        b"dispatch.method_id.size",
        DISPATCH_ENTRY_METHOD_OFFSET,
        8,
    );
    digest = fold_layout_field(
        digest,
        b"dispatch.pc.offset",
        b"dispatch.pc.size",
        DISPATCH_ENTRY_PC_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"dispatch.reserved.offset",
        b"dispatch.reserved.size",
        DISPATCH_ENTRY_RESERVED_OFFSET,
        4,
    );
    digest = format_fingerprint_u64(digest, b"dispatch_reserved_allowed_mask", 0);
    digest = format_fingerprint_u64(digest, b"dispatch_table_nonempty", 1);
    digest = format_fingerprint_u64(digest, b"dispatch_size_multiple_of_entry", 1);
    digest = format_fingerprint_u64(digest, b"dispatch_method_ids_strictly_increasing", 1);
    digest = format_fingerprint_u64(digest, b"dispatch_pc_must_be_valid_program_pc", 1);

    digest = format_fingerprint_u64(digest, b"memory_bytes_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"code_size_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"initial_memory_size_equals_memory_bytes", 1);
    digest = format_fingerprint_bytes(digest, b"canonical_section_order", canonical_section_order);
    digest = format_fingerprint_u64(digest, b"sections_must_be_in_bounds", 1);
    digest = format_fingerprint_u64(digest, b"sections_must_not_overlap", 1);
    digest = format_fingerprint_u64(digest, b"no_section_gaps_or_trailing_bytes", 1);
    digest
}

/// A resolved dispatch entry: a contract method id and the code offset (pc)
/// its entry symbol resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchEntry {
    /// The `ContractEntrypointID` (big-endian u64 the SDK macro encodes as
    /// 16 uppercase hex chars in the export name).
    pub method_id: u64,
    /// The entry program counter (byte offset into the code section).
    pub pc: u32,
}

/// Raw SHA-256 digest of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Lowercase-hex SHA-256 of `bytes` (matches Python `hashlib.hexdigest()`).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        // Two lowercase hex nibbles per byte.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Pack dispatch entries into the v3 dispatch-table layout, sorted by method
/// id (the caller is responsible for the sort; this re-checks the pc bound):
///
/// ```text
///   struct DispatchEntry { u64 method_id; u32 pc; u32 _reserved; }
/// ```
pub fn build_dispatch_table(entries: &[DispatchEntry], code_size: usize) -> Result<Vec<u8>> {
    let mut table = Vec::with_capacity(entries.len() * DISPATCH_ENTRY_BYTES);
    for entry in entries {
        if entry.pc as usize >= code_size {
            return Err(invalid(format!(
                "dispatch entry {:016x} pc={} outside .text",
                entry.method_id, entry.pc
            )));
        }
        table.extend_from_slice(&entry.method_id.to_le_bytes());
        table.extend_from_slice(&entry.pc.to_le_bytes());
        table.extend_from_slice(&0u32.to_le_bytes());
    }
    Ok(table)
}

/// Assemble a complete v3 artifact from the code section, the materialized
/// 64 KiB initial-memory image, and the (already method-id-sorted) dispatch
/// entries.
pub fn build_artifact(code: &[u8], memory: &[u8], entries: &[DispatchEntry]) -> Result<Vec<u8>> {
    // The loader rejects code_size == 0 || > MAX_CODE_BYTES; refuse to emit
    // such an artifact even when called directly (not via `lower`).
    if code.is_empty() || code.len() > MAX_CODE_BYTES {
        return Err(invalid(format!(
            "code size {} out of range (1..={MAX_CODE_BYTES})",
            code.len()
        )));
    }
    let dispatch_table = build_dispatch_table(entries, code.len())?;

    let code_offset = HEADER_BYTES;
    let dispatch_offset = code_offset + code.len();
    let memory_offset = dispatch_offset + dispatch_table.len();

    let to_u32 = |value: usize, what: &str| -> Result<u32> {
        u32::try_from(value).map_err(|_| invalid(format!("{what} does not fit in u32")))
    };
    let code_size = to_u32(code.len(), "code size")?;
    let memory_size = to_u32(memory.len(), "initial memory size")?;
    let code_off = to_u32(code_offset, "code offset")?;
    let memory_off = to_u32(memory_offset, "initial memory offset")?;
    let dispatch_off = to_u32(dispatch_offset, "dispatch offset")?;
    let dispatch_size = to_u32(dispatch_table.len(), "dispatch size")?;

    let mut header = Vec::with_capacity(HEADER_BYTES);
    header.extend_from_slice(&MAGIC);
    header.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    header.extend_from_slice(&to_u32(HEADER_BYTES, "header bytes")?.to_le_bytes());
    header.extend_from_slice(&VM_PROFILE_DIGEST.to_le_bytes());
    header.extend_from_slice(&FLAGS_REQUIRED_VALUE.to_le_bytes());
    header.extend_from_slice(&ENTRY_ABI_VERSION.to_le_bytes());
    header.extend_from_slice(&SYSCALL_ABI_VERSION.to_le_bytes());
    header.extend_from_slice(&GAS_TABLE_VERSION.to_le_bytes());
    header.extend_from_slice(&GAS_SCHEDULE_DIGEST.to_le_bytes());
    header.extend_from_slice(&to_u32(MAX_MEMORY_BYTES, "memory bytes")?.to_le_bytes());
    header.extend_from_slice(&code_size.to_le_bytes());
    header.extend_from_slice(&memory_size.to_le_bytes());
    header.extend_from_slice(&code_off.to_le_bytes());
    header.extend_from_slice(&memory_off.to_le_bytes());
    header.extend_from_slice(&dispatch_off.to_le_bytes());
    header.extend_from_slice(&dispatch_size.to_le_bytes());
    header.extend_from_slice(&sha256(code));
    header.extend_from_slice(&sha256(memory));
    header.extend_from_slice(&sha256(&dispatch_table));
    if header.len() != HEADER_BYTES {
        return Err(verify("internal artifact header size mismatch"));
    }

    let mut artifact = header;
    artifact.extend_from_slice(code);
    artifact.extend_from_slice(&dispatch_table);
    artifact.extend_from_slice(memory);
    Ok(artifact)
}

/// Re-parse a freshly-written artifact and confirm magic, the format/ABI/gas
/// stamps, the three section digests, and that the dispatch table is strictly
/// sorted with zero reserved bytes. Cheap insurance against a writer
/// regression slipping past until the C++ parser flags it.
pub fn verify_artifact(artifact: &[u8]) -> Result<()> {
    if artifact.len() < HEADER_BYTES {
        return Err(verify("artifact shorter than header"));
    }
    if artifact[..MAGIC.len()] != MAGIC {
        return Err(verify("bad magic"));
    }

    // All header reads land in [0, 172) <= artifact.len(); the fixed-width
    // slices are exactly 4/8 bytes, so the conversions cannot fail.
    let read_u32 = |off: usize| -> u32 {
        u32::from_le_bytes(
            artifact[off..off + 4]
                .try_into()
                .expect("bug: 4-byte slice"),
        )
    };
    let read_u64 = |off: usize| -> u64 {
        u64::from_le_bytes(
            artifact[off..off + 8]
                .try_into()
                .expect("bug: 8-byte slice"),
        )
    };

    if read_u32(FORMAT_VERSION_OFFSET) != FORMAT_VERSION {
        return Err(verify(format!(
            "format_version {}",
            read_u32(FORMAT_VERSION_OFFSET)
        )));
    }
    if read_u32(HEADER_SIZE_OFFSET) as usize != HEADER_BYTES {
        return Err(verify(format!(
            "header_bytes {}",
            read_u32(HEADER_SIZE_OFFSET)
        )));
    }
    if read_u64(VM_PROFILE_OFFSET) != VM_PROFILE_DIGEST {
        return Err(verify("vm_profile mismatch"));
    }
    if read_u32(ENTRY_ABI_OFFSET) != ENTRY_ABI_VERSION {
        return Err(verify("entry_abi mismatch"));
    }
    if read_u32(SYSCALL_ABI_OFFSET) != SYSCALL_ABI_VERSION {
        return Err(verify("syscall_abi mismatch"));
    }
    if read_u32(GAS_VERSION_OFFSET) != GAS_TABLE_VERSION
        || read_u64(GAS_DIGEST_OFFSET) != GAS_SCHEDULE_DIGEST
    {
        return Err(verify("gas schedule mismatch"));
    }
    if read_u32(MEMORY_BYTES_OFFSET) as usize != MAX_MEMORY_BYTES {
        return Err(verify("memory_bytes mismatch"));
    }
    if read_u32(FLAGS_OFFSET) != FLAGS_REQUIRED_VALUE {
        return Err(verify("flags must be zero"));
    }

    let code_size = read_u32(CODE_SIZE_OFFSET) as usize;
    let init_mem_size = read_u32(INITIAL_MEMORY_SIZE_OFFSET) as usize;
    let code_off = read_u32(CODE_OFFSET_OFFSET) as usize;
    let mem_off = read_u32(INITIAL_MEMORY_OFFSET_OFFSET) as usize;
    let disp_off = read_u32(DISPATCH_OFFSET_OFFSET) as usize;
    let disp_size = read_u32(DISPATCH_SIZE_OFFSET) as usize;

    // Canonical layout: the body is exactly header || code || dispatch ||
    // memory with no gaps or trailing bytes, code fits the declared limit, the
    // memory image is fully materialized, and there is at least one dispatch
    // entry. This mirrors the acceptance set the consensus C++ loader enforces,
    // so the writer's self-check rejects anything the loader would.
    if code_size == 0 || code_size > MAX_CODE_BYTES {
        return Err(verify("code size out of range"));
    }
    if init_mem_size != MAX_MEMORY_BYTES {
        return Err(verify("initial_memory_size mismatch"));
    }
    if disp_size == 0 {
        return Err(verify("dispatch table is empty"));
    }
    if code_off != HEADER_BYTES
        || disp_off != code_off + code_size
        || mem_off != disp_off + disp_size
        || mem_off + init_mem_size != artifact.len()
    {
        return Err(verify("non-canonical section layout"));
    }

    let code_dig = &artifact[CODE_DIGEST_OFFSET..CODE_DIGEST_OFFSET + DIGEST_BYTES];
    let mem_dig =
        &artifact[INITIAL_MEMORY_DIGEST_OFFSET..INITIAL_MEMORY_DIGEST_OFFSET + DIGEST_BYTES];
    let disp_dig = &artifact[DISPATCH_DIGEST_OFFSET..DISPATCH_DIGEST_OFFSET + DIGEST_BYTES];

    let section = |off: usize, size: usize, what: &str| -> Result<&[u8]> {
        artifact
            .get(off..off + size)
            .ok_or_else(|| verify(format!("{what} section out of range")))
    };
    let code = section(code_off, code_size, "code")?;
    let dispatch = section(disp_off, disp_size, "dispatch")?;
    let memory = section(mem_off, init_mem_size, "initial_memory")?;

    if sha256(code).as_slice() != code_dig {
        return Err(verify("code digest mismatch"));
    }
    if sha256(memory).as_slice() != mem_dig {
        return Err(verify("initial_memory digest mismatch"));
    }
    if sha256(dispatch).as_slice() != disp_dig {
        return Err(verify("dispatch digest mismatch"));
    }

    verify_dispatch_table(dispatch, code_size)
}

/// Confirm the dispatch table is a whole number of entries, strictly sorted by
/// method id, with zero reserved bytes and in-range, aligned pcs.
fn verify_dispatch_table(dispatch: &[u8], code_size: usize) -> Result<()> {
    if !dispatch.len().is_multiple_of(DISPATCH_ENTRY_BYTES) {
        return Err(verify("dispatch size not a multiple of entry size"));
    }
    let mut prev: i128 = -1;
    for (index, entry) in dispatch.chunks_exact(DISPATCH_ENTRY_BYTES).enumerate() {
        let method_id = u64::from_le_bytes(
            entry[DISPATCH_ENTRY_METHOD_OFFSET..DISPATCH_ENTRY_METHOD_OFFSET + 8]
                .try_into()
                .expect("bug: 8-byte slice"),
        );
        let pc = u32::from_le_bytes(
            entry[DISPATCH_ENTRY_PC_OFFSET..DISPATCH_ENTRY_PC_OFFSET + 4]
                .try_into()
                .expect("bug: 4-byte slice"),
        );
        let reserved = u32::from_le_bytes(
            entry[DISPATCH_ENTRY_RESERVED_OFFSET..DISPATCH_ENTRY_RESERVED_OFFSET + 4]
                .try_into()
                .expect("bug: 4-byte slice"),
        );
        if reserved != 0 {
            return Err(verify(format!(
                "dispatch entry {index} reserved={reserved}"
            )));
        }
        if i128::from(method_id) <= prev {
            return Err(verify("dispatch entries not strictly sorted"));
        }
        if pc as usize >= code_size || !(pc as usize).is_multiple_of(INSTRUCTION_BYTES) {
            return Err(verify(format!(
                "dispatch entry {method_id:016X} pc={pc} invalid"
            )));
        }
        prev = i128::from(method_id);
    }
    Ok(())
}
