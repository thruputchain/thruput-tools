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
/// Gas-schedule table version.
pub const GAS_TABLE_VERSION: u32 = 2;
/// Gas-schedule consensus digest (folds in instruction + host-syscall costs).
pub const GAS_SCHEDULE_DIGEST: u64 = 0xebe5_3dce_ea58_1d0c;

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
    header.extend_from_slice(&0u32.to_le_bytes()); // flags
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
    if artifact[..8] != MAGIC {
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

    if read_u32(8) != FORMAT_VERSION {
        return Err(verify(format!("format_version {}", read_u32(8))));
    }
    if read_u32(12) as usize != HEADER_BYTES {
        return Err(verify(format!("header_bytes {}", read_u32(12))));
    }
    if read_u64(16) != VM_PROFILE_DIGEST {
        return Err(verify("vm_profile mismatch"));
    }
    if read_u32(28) != ENTRY_ABI_VERSION {
        return Err(verify("entry_abi mismatch"));
    }
    if read_u32(32) != SYSCALL_ABI_VERSION {
        return Err(verify("syscall_abi mismatch"));
    }
    if read_u32(36) != GAS_TABLE_VERSION || read_u64(40) != GAS_SCHEDULE_DIGEST {
        return Err(verify("gas schedule mismatch"));
    }
    if read_u32(48) as usize != MAX_MEMORY_BYTES {
        return Err(verify("memory_bytes mismatch"));
    }
    if read_u32(24) != 0 {
        return Err(verify("flags must be zero"));
    }

    let code_size = read_u32(52) as usize;
    let init_mem_size = read_u32(56) as usize;
    let code_off = read_u32(60) as usize;
    let mem_off = read_u32(64) as usize;
    let disp_off = read_u32(68) as usize;
    let disp_size = read_u32(72) as usize;

    // Canonical layout: the body is exactly header || code || dispatch ||
    // memory with no gaps or trailing bytes, the memory image is fully
    // materialized, and there is at least one dispatch entry. This mirrors the
    // acceptance set the consensus C++ loader enforces, so the writer's
    // self-check rejects anything the loader would.
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

    let code_dig = &artifact[76..108];
    let mem_dig = &artifact[108..140];
    let disp_dig = &artifact[140..172];

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
        let method_id = u64::from_le_bytes(entry[0..8].try_into().expect("bug: 8-byte slice"));
        let pc = u32::from_le_bytes(entry[8..12].try_into().expect("bug: 4-byte slice"));
        let reserved = u32::from_le_bytes(entry[12..16].try_into().expect("bug: 4-byte slice"));
        if reserved != 0 {
            return Err(verify(format!(
                "dispatch entry {index} reserved={reserved}"
            )));
        }
        if i128::from(method_id) <= prev {
            return Err(verify("dispatch entries not strictly sorted"));
        }
        if pc as usize >= code_size || !pc.is_multiple_of(4) {
            return Err(verify(format!(
                "dispatch entry {method_id:016X} pc={pc} invalid"
            )));
        }
        prev = i128::from(method_id);
    }
    Ok(())
}
