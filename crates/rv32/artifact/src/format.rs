//! The Groundhog RV32 **v5 artifact format**: constants, writer, and
//! self-verifier.
//!
//! This is the single Rust source of truth for the on-disk format the hogd
//! RV32 VM loads. It mirrors `hogd/hogexec/runtime/rv32/artifact.h`; the
//! values below are the load-bearing contract and must stay in lockstep with
//! that header.
//!
//! v4 moved method dispatch into the guest: the runtime always enters at
//! `_start` (PC 0) and seeds the invoked method id in registers, so the
//! artifact carries no method → PC dispatch table, and only the initialized
//! prefix of data memory is stored.
//!
//! v5 adds `initial_memory_start`: the guest data address (a VA, unrelated to
//! file layout) where the runtime stages the stored image. The runtime
//! zero-fills `[0, start)` and `[start + initial_memory_size, memory_bytes)`.
//! This lets the stored image be the minimal nonzero span of the data image
//! instead of the full `[0, high_water)` prefix, so a stack-first guest
//! layout (stack below the data sections) carries no stack-sized leading-zero
//! shadow. v5 also bumps the entry ABI to 3: `sp` is seeded to 0 and the
//! guest owns stack placement (v2 seeded `sp` to the top of data memory).
//!
//! Header layout (little-endian, no padding, 128 bytes total):
//!
//! ```text
//!   off  size  field
//!     0     8  magic "GHRV32A1"
//!     8     4  format_version          (5)
//!    12     4  header_bytes            (128)
//!    16     8  vm_profile_digest
//!    24     4  flags                   (0)
//!    28     4  entry_abi_version       (3)
//!    32     4  syscall_abi_version     (1)
//!    36     4  gas_table_version       (3)
//!    40     8  gas_schedule_digest
//!    48     4  memory_bytes            (65536)
//!    52     4  code_size
//!    56     4  initial_memory_size
//!    60     4  initial_memory_start
//!    64    32  sha256(code)
//!    96    32  sha256(initial_memory)
//! ```
//!
//! Body: `header || code || initial_memory`. Section positions are implicit:
//! code starts at `header_bytes`, `initial_memory` directly after code, and
//! the file ends exactly at `header_bytes + code_size + initial_memory_size`
//! — there are no stored file offsets (v5 dropped v4's redundant
//! `code_offset` / `initial_memory_offset` fields).

use sha2::{Digest, Sha256};

use crate::error::{Result, invalid, verify};

/// Code (`.text`) bytes a chain admits in an attached artifact unless its
/// configuration states otherwise (`kConfigParamMaxRv32CodeBytes` in the
/// protocol). The format itself does not cap `code_size` beyond its 32-bit
/// field; the lowerer refuses code past the limit it is given, which defaults
/// to this.
pub const DEFAULT_MAX_CODE_BYTES: usize = 64 * 1024;
/// Guest data address space the lowerer stamps into `memory_bytes` unless
/// told otherwise, and the size a chain admits unless its configuration
/// states otherwise (`kConfigParamMaxRv32MemoryBytes`). The stored
/// initial-memory image is the span staged at `initial_memory_start`; the
/// runtime zero-fills the rest. A contract only benefits from a larger value
/// once its linker script provides a matching `__memory_size`.
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024;
/// RV32 instruction width in bytes.
pub const INSTRUCTION_BYTES: usize = 4;
/// Artifact magic: `GHRV32A1`.
pub const MAGIC: [u8; 8] = *b"GHRV32A1";
/// Artifact format version.
pub const FORMAT_VERSION: u32 = 5;
/// Fixed header size in bytes.
pub const HEADER_BYTES: usize = 128;
/// VM-profile digest the runtime validates the artifact against.
pub const VM_PROFILE_DIGEST: u64 = 0x55c5_3a68_5f0e_b33d;
/// Entry ABI version (3 = enter at PC 0 with the method id seeded in a0/a1
/// and `sp` seeded to 0 — the guest owns stack placement and must set `sp`
/// before touching the stack; v2 seeded `sp` to the top of data memory).
pub const ENTRY_ABI_VERSION: u32 = 3;
/// Syscall ABI version (1 = register-native: args in a0..a7, no frame).
pub const SYSCALL_ABI_VERSION: u32 = 1;
/// Gas-schedule table version.
/// v3: hogd admitted canonical UNIMP (0xc0001073) as a deterministic
/// invalid-instruction trap.
/// v4: hogd prices `NNInt` storage access and the bytestring syscalls.
pub const GAS_TABLE_VERSION: u32 = 4;
/// Gas-schedule consensus digest (folds in instruction + host-syscall costs).
pub const GAS_SCHEDULE_DIGEST: u64 = 0xbd9a_5e2f_5bf2_16fd;
/// Digest size of each section hash stamped into the artifact header.
pub const DIGEST_BYTES: usize = 32;

/// Fixed v5 header offsets.
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
pub const INITIAL_MEMORY_START_OFFSET: usize = 60;
pub const CODE_DIGEST_OFFSET: usize = 64;
pub const INITIAL_MEMORY_DIGEST_OFFSET: usize = CODE_DIGEST_OFFSET + DIGEST_BYTES;

const _: () = assert!(INITIAL_MEMORY_DIGEST_OFFSET + DIGEST_BYTES == HEADER_BYTES);

// Project-specific seeded FNV-1a fold. NOTE: FNV_OFFSET_BASIS is intentionally
// NOT the canonical 64-bit FNV-1a basis (0xcbf29ce484222325); it is a fixed
// project seed shared byte-for-byte with hogd's `kFnvOffsetBasis`. It also
// seeds the consensus gas-schedule digest, so changing it would alter every
// emitted artifact's gas digest — a format break, not a cleanup. The fold
// only needs a *consistent* seed across the two implementations, which the
// cross-repo fingerprint check enforces. Do not "fix" to the canonical value.
const FNV_OFFSET_BASIS: u64 = 1_469_598_103_934_665_603;
const FNV_PRIME: u64 = 1_099_511_628_211;
const FORMAT_FINGERPRINT_DOMAIN: &[u8] = b"groundhog.rv32.artifact.format.v5";
const SECTION_DIGEST_ALGORITHM: &[u8] = b"sha256";
const CANONICAL_SECTION_ORDER: &[u8] = b"header|code|initial_memory";
const FLAGS_REQUIRED_VALUE: u32 = 0;

/// Pinned v5 artifact-format contract fingerprint. hogd pins the same value
/// as `kArtifactFormatFingerprint` in `rv32/artifact.h`.
pub const FORMAT_FINGERPRINT: u64 = 0x44f3_a5cd_6a28_5ecf;
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

const fn fnv_digest_u64(mut digest: u64, value: u64) -> u64 {
    // Fold the little-endian bytes directly; to_le_bytes() avoids a
    // `value as u8` truncation cast (clippy::cast_possible_truncation under
    // -D warnings) while folding the exact same bytes as the C++ side.
    let bytes = value.to_le_bytes();
    let mut i = 0;
    while i < bytes.len() {
        digest = fnv_digest_byte(digest, bytes[i]);
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

/// Compute the v5 artifact-format contract fingerprint.
#[must_use]
pub const fn compute_format_fingerprint() -> u64 {
    compute_format_fingerprint_with_inputs(
        FORMAT_FINGERPRINT_DOMAIN,
        MAGIC,
        FORMAT_VERSION,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

/// Test hook for proving that a folded scalar perturbation moves the digest.
#[doc(hidden)]
#[must_use]
// Sensitivity-test hook only (the perturbation test feeds it
// FORMAT_VERSION + 1 to prove the digest reacts), NOT version dispatch: the
// folded domain string is hard-wired to the current format version. At a
// format bump, bump the domain string and FORMAT_VERSION together.
pub const fn compute_format_fingerprint_with_format_version(format_version: u32) -> u64 {
    compute_format_fingerprint_with_inputs(
        FORMAT_FINGERPRINT_DOMAIN,
        MAGIC,
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
        magic,
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
        MAGIC,
        FORMAT_VERSION,
        SECTION_DIGEST_ALGORITHM,
        CANONICAL_SECTION_ORDER,
    )
}

// Intentionally a long, flat enumeration: it folds every runtime-validated v5
// format fact in a fixed order. Splitting it would obscure the field list that
// is the whole point of the fingerprint.
#[allow(clippy::too_many_lines)]
const fn compute_format_fingerprint_with_inputs(
    domain: &[u8],
    magic: [u8; 8],
    format_version: u32,
    section_digest_algorithm: &[u8],
    canonical_section_order: &[u8],
) -> u64 {
    let mut digest = FNV_OFFSET_BASIS;
    digest = fnv_digest_name(digest, domain);

    digest = format_fingerprint_bytes(digest, b"magic", &magic);
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
        b"header.initial_memory_start.offset",
        b"header.initial_memory_start.size",
        INITIAL_MEMORY_START_OFFSET,
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

    digest = format_fingerprint_bytes(
        digest,
        b"section_digest_algorithm",
        section_digest_algorithm,
    );
    digest = format_fingerprint_u64(digest, b"digest_bytes", DIGEST_BYTES as u64);
    digest = format_fingerprint_u64(digest, b"instruction_bytes", INSTRUCTION_BYTES as u64);
    // The code and memory bounds are a chain's configuration, not part of the
    // format: the header fields are 32-bit either way, and what a chain admits
    // is checked where a contract is attached.

    digest = format_fingerprint_u64(digest, b"memory_bytes_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"memory_bytes_le_max", 1);
    digest = format_fingerprint_u64(digest, b"code_size_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"initial_memory_start_plus_size_le_memory_bytes", 1);
    digest = format_fingerprint_u64(digest, b"initial_memory_start_zero_when_empty", 1);
    digest = format_fingerprint_bytes(digest, b"canonical_section_order", canonical_section_order);
    digest = format_fingerprint_u64(digest, b"no_section_gaps_or_trailing_bytes", 1);
    digest
}

/// Raw SHA-256 digest of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Lowercase-hex SHA-256 of `bytes` (matches Python `hashlib.hexdigest()`).
pub fn sha256_hex(bytes: &[u8]) -> String {
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

/// Assemble a complete v5 artifact from the code section and the stored span
/// of the data-memory image.
///
/// `initial_memory` is staged at data address `initial_memory_start` — the
/// runtime zero-fills `[0, base)` and `[base + initial_memory.len(),
/// memory_bytes)`. The span must fit the data address space
/// (`base + len <= MAX_MEMORY_BYTES`), and an empty image must use base 0.
/// `memory_bytes` is stamped as `MAX_MEMORY_BYTES` (the full data address
/// space).
/// `memory_bytes` is the guest data address space the artifact declares; the
/// stored image must fit inside it. The code and memory bounds a chain admits
/// are its configuration, checked at deploy rather than here, so a caller
/// building for a chain with raised limits passes what that chain allows.
pub fn build_artifact(
    code: &[u8],
    initial_memory: &[u8],
    initial_memory_start: usize,
    memory_bytes: usize,
) -> Result<Vec<u8>> {
    // The loader rejects code_size == 0; refuse to emit such an artifact even
    // when called directly (not via `lower`).
    if code.is_empty() {
        return Err(invalid("code size 0 out of range"));
    }
    if memory_bytes == 0 {
        return Err(invalid("memory_bytes must be nonzero"));
    }
    // The stored span must fit the data address space.
    let span_end = initial_memory_start
        .checked_add(initial_memory.len())
        .ok_or_else(|| invalid("initial memory span overflows"))?;
    if span_end > memory_bytes {
        return Err(invalid(format!(
            "initial memory span end {span_end} exceeds memory_bytes {memory_bytes}"
        )));
    }
    // Keep the format canonical: an empty image has exactly one encoding.
    if initial_memory.is_empty() && initial_memory_start != 0 {
        return Err(invalid(
            "initial_memory_start must be zero for an empty image",
        ));
    }

    let to_u32 = |value: usize, what: &str| -> Result<u32> {
        u32::try_from(value).map_err(|_| invalid(format!("{what} does not fit in u32")))
    };
    let code_size = to_u32(code.len(), "code size")?;
    let memory_size = to_u32(initial_memory.len(), "initial memory size")?;
    let memory_base = to_u32(initial_memory_start, "initial memory base")?;

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
    header.extend_from_slice(&to_u32(memory_bytes, "memory bytes")?.to_le_bytes());
    header.extend_from_slice(&code_size.to_le_bytes());
    header.extend_from_slice(&memory_size.to_le_bytes());
    header.extend_from_slice(&memory_base.to_le_bytes());
    header.extend_from_slice(&sha256(code));
    header.extend_from_slice(&sha256(initial_memory));
    if header.len() != HEADER_BYTES {
        return Err(verify("internal artifact header size mismatch"));
    }

    let mut artifact = header;
    artifact.extend_from_slice(code);
    artifact.extend_from_slice(initial_memory);
    Ok(artifact)
}

/// Re-parse a freshly-written artifact and confirm magic, the format/ABI/gas
/// stamps, the canonical section layout, and the two section digests. Cheap
/// insurance against a writer regression slipping past until the C++ parser
/// flags it.
pub fn verify_artifact(artifact: &[u8]) -> Result<()> {
    if artifact.len() < HEADER_BYTES {
        return Err(verify("artifact shorter than header"));
    }
    if artifact[..MAGIC.len()] != MAGIC {
        return Err(verify("bad magic"));
    }

    // All header reads land in [0, HEADER_BYTES) <= artifact.len(); the
    // fixed-width slices are exactly 4/8 bytes, so the conversions cannot fail.
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
    if read_u32(FLAGS_OFFSET) != FLAGS_REQUIRED_VALUE {
        return Err(verify("flags must be zero"));
    }

    let memory_bytes = read_u32(MEMORY_BYTES_OFFSET) as usize;
    let code_size = read_u32(CODE_SIZE_OFFSET) as usize;
    let init_mem_size = read_u32(INITIAL_MEMORY_SIZE_OFFSET) as usize;
    let init_mem_start = read_u32(INITIAL_MEMORY_START_OFFSET) as usize;

    // Canonical layout: the body is exactly header || code || initial_memory
    // with no gaps or trailing bytes, and the stored memory prefix fits the
    // declared data address space. This mirrors the structural acceptance set
    // the consensus C++ loader enforces, so the writer's self-check rejects
    // anything the loader would on shape alone. How large `code_size` and
    // `memory_bytes` may be is the deploying chain's configuration, checked
    // there rather than here.
    if memory_bytes == 0 {
        return Err(verify("memory_bytes out of range"));
    }
    if code_size == 0 {
        return Err(verify("code size out of range"));
    }
    // The staged span must fit the data address space; checked math so a
    // crafted header cannot wrap the sum on 32-bit hosts. Subsumes the v4
    // `initial_memory_size <= memory_bytes` check.
    let init_mem_end = init_mem_start
        .checked_add(init_mem_size)
        .ok_or_else(|| verify("initial memory span overflows"))?;
    if init_mem_end > memory_bytes {
        return Err(verify(
            "initial_memory_start plus initial_memory_size exceeds memory_bytes",
        ));
    }
    if init_mem_size == 0 && init_mem_start != 0 {
        return Err(verify(
            "initial_memory_start must be zero for an empty image",
        ));
    }
    // Section positions are implicit: code at HEADER_BYTES, initial_memory
    // directly after, and the file ends exactly there — no gaps, no trailing
    // bytes. Checked math for the same 32-bit wrap reason as above.
    let expected_len = HEADER_BYTES
        .checked_add(code_size)
        .and_then(|len| len.checked_add(init_mem_size))
        .ok_or_else(|| verify("non-canonical section layout"))?;
    if expected_len != artifact.len() {
        return Err(verify("non-canonical section layout"));
    }

    let code_dig = &artifact[CODE_DIGEST_OFFSET..CODE_DIGEST_OFFSET + DIGEST_BYTES];
    let mem_dig =
        &artifact[INITIAL_MEMORY_DIGEST_OFFSET..INITIAL_MEMORY_DIGEST_OFFSET + DIGEST_BYTES];

    let code = &artifact[HEADER_BYTES..HEADER_BYTES + code_size];
    let memory = &artifact[HEADER_BYTES + code_size..];

    if sha256(code).as_slice() != code_dig {
        return Err(verify("code digest mismatch"));
    }
    if sha256(memory).as_slice() != mem_dig {
        return Err(verify("initial_memory digest mismatch"));
    }

    Ok(())
}
