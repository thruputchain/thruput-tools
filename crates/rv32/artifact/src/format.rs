//! The Thruput RV32 **v6 artifact format**: constants, writer, and
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
//! v6 appends an extension list after initial memory: data the runtime never
//! loads into guest memory, such as the contract spec. Adding an extension
//! kind does not change the format. See [`crate::extension`] for the XDR
//! definition and the rules the chain enforces. The enclosing
//! `SHA256(XDR(Contract))` identity covers the extensions along with the rest
//! of the artifact.
//!
//! v6 also replaces v5's gas-schedule stamp with the syscall API version the
//! guest was built against ([`SyscallApiVersion`]). A chain admits the
//! artifact when it implements the same major version and at least that
//! minor; what a syscall costs is the chain's business and no longer part of
//! the artifact. The minor takes the old gas-table-version slot and the
//! 8-byte digest is gone, so a v6 header is 120 bytes and every later field
//! sits 8 bytes earlier than in v5.
//!
//! Readers accept v5 too, since v5 contracts stay deployed after v6 ships: a
//! v5 artifact reads as a v6 artifact with no extensions. Producers emit v6
//! only.
//!
//! Header layout (little-endian, no padding, 120 bytes total):
//!
//! ```text
//!   off  size  field
//!     0     8  magic "GHRV32A1"
//!     8     4  format_version          (6)
//!    12     4  header_bytes            (120)
//!    16     8  vm_profile_digest
//!    24     4  flags                   (0)
//!    28     4  entry_abi_version       (3)
//!    32     4  syscall_api_major       (1)
//!    36     4  syscall_api_minor
//!    40     4  memory_bytes            (65536)
//!    44     4  code_size
//!    48     4  initial_memory_size
//!    52     4  initial_memory_start
//!    56    32  sha256(code)
//!    88    32  sha256(initial_memory)
//! ```
//!
//! A v5 header is 128 bytes: `gas_table_version` at 36, an 8-byte
//! `gas_schedule_digest` at 40, and the remaining fields 8 bytes later. So is
//! a v6 header written before v6 declared a syscall API version; its
//! `header_bytes` of 128 tells the two v6 layouts apart. Readers open all
//! three.
//!
//! Body: `header || code || initial_memory || extensions`. Section positions
//! are implicit: code starts at `header_bytes`, `initial_memory` directly
//! after code, and `extensions` (the XDR-encoded `Rv32ArtifactExtensions`)
//! runs from there exactly to the end of the file. There are no stored file
//! offsets (v5 dropped v4's redundant `code_offset` / `initial_memory_offset`
//! fields). A v5 body has no `extensions` and ends after `initial_memory`.

use sha2::{Digest, Sha256};

use crate::error::{Result, invalid, verify};
use crate::extension::{
    ArtifactExtension, ArtifactSpec, EXTENSION_KIND_CONTRACT_SPEC, decode_extensions,
    decode_spec_extension, encode_extensions, encode_spec_extension,
};

/// Code (`.text`) bytes a chain admits in an attached artifact unless its
/// configuration states otherwise: the protocol's `kDefaultMaxRv32CodeBytes`
/// (`hog_exec_constants.x`), the value in force for an unstated
/// `kConfigParamMaxRv32CodeBytes`. The format itself does not cap `code_size`
/// beyond its 32-bit field, and neither does the lowerer: it warns past this
/// value and leaves admission to the chain a contract is deployed to.
pub const DEFAULT_MAX_CODE_BYTES: usize = 256 * 1024;
/// Guest data address space the lowerer stamps into `memory_bytes` unless
/// told otherwise, and the size a chain admits unless its configuration
/// states otherwise: the protocol's `kDefaultMaxRv32MemoryBytes`, the value
/// in force for an unstated `kConfigParamMaxRv32MemoryBytes`. The stored
/// initial-memory image is the span staged at `initial_memory_start`; the
/// runtime zero-fills the rest. A contract only benefits from a larger value
/// once its linker script provides a matching `__memory_size`.
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024;
/// RV32 instruction width in bytes.
pub const INSTRUCTION_BYTES: usize = 4;
/// Artifact magic: `GHRV32A1`.
pub const MAGIC: [u8; 8] = *b"GHRV32A1";
/// Artifact format version producers emit.
pub const FORMAT_VERSION: u32 = 6;
/// The last format version without an extension list. Readers still accept
/// it because contracts deployed in it remain on chain.
pub const LEGACY_FORMAT_VERSION_V5: u32 = 5;
/// Format versions readers accept.
pub const READABLE_FORMAT_VERSIONS: [u32; 2] = [LEGACY_FORMAT_VERSION_V5, FORMAT_VERSION];
/// Header size in bytes of the artifacts producers emit.
pub const HEADER_BYTES: usize = 120;
/// Header size in bytes of a header that declares a gas schedule: every v5
/// artifact, and a v6 artifact written before v6 declared a syscall API
/// version.
pub const LEGACY_HEADER_BYTES: usize = HEADER_BYTES + LEGACY_GAS_DIGEST_BYTES;
/// The gas-schedule digest a [`LEGACY_HEADER_BYTES`] header carries after its
/// gas table version; every later field sits this much further on.
const LEGACY_GAS_DIGEST_BYTES: usize = 8;
/// VM-profile digest the runtime validates the artifact against.
pub const VM_PROFILE_DIGEST: u64 = 0x55c5_3a68_5f0e_b33d;
/// Entry ABI version (3 = enter at PC 0 with the method id seeded in a0/a1
/// and `sp` seeded to 0 — the guest owns stack placement and must set `sp`
/// before touching the stack; v2 seeded `sp` to the top of data memory).
pub const ENTRY_ABI_VERSION: u32 = 3;
/// Major version of the syscall API (`syscall_api_version.major` in the
/// protocol's `syscalls.yaml`): the calling convention (register-native,
/// args in a0..a7, no frame) and every syscall's number and signature. This
/// format describes artifacts for this major only. v5 calls the same field
/// its syscall ABI version.
pub const SYSCALL_API_MAJOR: u32 = 1;

/// The syscall API version a guest was built against: what a v6 artifact
/// declares so a chain can decide whether it implements everything the guest
/// may call. The protocol's `syscalls.yaml` states the versioning rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SyscallApiVersion {
    /// Changes when anything a guest can observe changes other than by adding
    /// a syscall. Always [`SYSCALL_API_MAJOR`] in an artifact this crate
    /// writes.
    pub major: u32,
    /// Rises by one whenever syscalls are added.
    pub minor: u32,
}

impl SyscallApiVersion {
    /// The syscalls every chain on [`SYSCALL_API_MAJOR`] offers. For an
    /// artifact assembled without a guest to ask, such as a test fixture or
    /// a size probe; an artifact for a real guest declares the version its
    /// SDK was built against.
    pub const BASELINE: Self = Self {
        major: SYSCALL_API_MAJOR,
        minor: 0,
    };
}

impl std::fmt::Display for SyscallApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}
/// Digest size of each section hash stamped into the artifact header.
pub const DIGEST_BYTES: usize = 32;

/// Fixed v6 header offsets. In a [`LEGACY_HEADER_BYTES`] header the fields
/// from `MEMORY_BYTES_OFFSET` on sit 8 bytes later.
pub const FORMAT_VERSION_OFFSET: usize = 8;
pub const HEADER_SIZE_OFFSET: usize = 12;
pub const VM_PROFILE_OFFSET: usize = 16;
pub const FLAGS_OFFSET: usize = 24;
pub const ENTRY_ABI_OFFSET: usize = 28;
pub const SYSCALL_API_MAJOR_OFFSET: usize = 32;
pub const SYSCALL_API_MINOR_OFFSET: usize = 36;
pub const MEMORY_BYTES_OFFSET: usize = 40;
pub const CODE_SIZE_OFFSET: usize = 44;
pub const INITIAL_MEMORY_SIZE_OFFSET: usize = 48;
pub const INITIAL_MEMORY_START_OFFSET: usize = 52;
pub const CODE_DIGEST_OFFSET: usize = 56;
pub const INITIAL_MEMORY_DIGEST_OFFSET: usize = CODE_DIGEST_OFFSET + DIGEST_BYTES;

const _: () = assert!(INITIAL_MEMORY_DIGEST_OFFSET + DIGEST_BYTES == HEADER_BYTES);

// Project-specific seeded FNV-1a fold. NOTE: FNV_OFFSET_BASIS is intentionally
// NOT the canonical 64-bit FNV-1a basis (0xcbf29ce484222325); it is a fixed
// project seed shared byte-for-byte with hogd's `kFnvOffsetBasis`. The fold
// only needs a *consistent* seed across the two implementations, which the
// cross-repo fingerprint check enforces. Do not "fix" to the canonical value.
const FNV_OFFSET_BASIS: u64 = 1_469_598_103_934_665_603;
const FNV_PRIME: u64 = 1_099_511_628_211;
const FORMAT_FINGERPRINT_DOMAIN: &[u8] = b"groundhog.rv32.artifact.format.v6";
const SECTION_DIGEST_ALGORITHM: &[u8] = b"sha256";
const CANONICAL_SECTION_ORDER: &[u8] = b"header|code|initial_memory|extensions";
const FLAGS_REQUIRED_VALUE: u32 = 0;

/// Pinned v6 artifact-format contract fingerprint. hogd pins the same value
/// as `kArtifactFormatFingerprint` in `rv32/artifact.h`.
pub const FORMAT_FINGERPRINT: u64 = 0x1fa5_48ec_574d_b1b4;
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

/// Compute the v6 artifact-format contract fingerprint.
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

// Intentionally a long, flat enumeration: it folds every runtime-validated v6
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
    digest = format_fingerprint_u64(digest, b"syscall_api_major", SYSCALL_API_MAJOR as u64);

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
        b"header.syscall_api_major.offset",
        b"header.syscall_api_major.size",
        SYSCALL_API_MAJOR_OFFSET,
        4,
    );
    digest = fold_layout_field(
        digest,
        b"header.syscall_api_minor.offset",
        b"header.syscall_api_minor.size",
        SYSCALL_API_MINOR_OFFSET,
        4,
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
    // The minor itself is not folded: it is the guest's, and a chain admits
    // any minor up to its own.
    digest = format_fingerprint_u64(digest, b"syscall_api_minor_le_host", 1);
    // The code and memory bounds are a chain's configuration, not part of the
    // format: the header fields are 32-bit either way, and what a chain admits
    // is checked where a contract is attached.

    digest = format_fingerprint_u64(digest, b"memory_bytes_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"memory_bytes_le_max", 1);
    digest = format_fingerprint_u64(digest, b"code_size_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"initial_memory_start_plus_size_le_memory_bytes", 1);
    digest = format_fingerprint_u64(digest, b"initial_memory_start_zero_when_empty", 1);
    // The extension list's framing rules. Individual kinds are not folded:
    // adding one does not change the format.
    digest = format_fingerprint_bytes(digest, b"extensions_encoding", b"xdr");
    digest = format_fingerprint_u64(digest, b"extensions_kind_nonzero", 1);
    digest = format_fingerprint_u64(digest, b"extensions_kind_strictly_ascending", 1);
    digest = format_fingerprint_u64(digest, b"extensions_xdr_padding_zero", 1);
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

/// Lowercase-hex SHA-256 of `bytes`.
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

/// Assemble a v6 artifact with no extensions from code and the stored
/// data-memory span.
///
/// `initial_memory` is staged at data address `initial_memory_start` — the
/// runtime zero-fills `[0, base)` and `[base + initial_memory.len(),
/// memory_bytes)`. `memory_bytes` is the guest data address space the
/// artifact declares: the stored span must fit inside it, and an empty image
/// must use base 0. How much code and memory a chain admits is its
/// configuration, checked where the contract is attached rather than here.
///
/// The artifact declares [`SyscallApiVersion::BASELINE`]: this is for
/// artifacts assembled without a guest, such as test fixtures and size
/// probes. Lowering a real guest stamps the version its ELF records.
pub fn build_artifact(
    code: &[u8],
    initial_memory: &[u8],
    initial_memory_start: usize,
    memory_bytes: usize,
) -> Result<Vec<u8>> {
    build_artifact_with_extensions(
        SyscallApiVersion::BASELINE,
        code,
        initial_memory,
        initial_memory_start,
        memory_bytes,
        &[],
    )
}

/// Assemble a v6 artifact with an optional contract spec, carried as a
/// [`EXTENSION_KIND_CONTRACT_SPEC`] extension.
pub fn build_artifact_with_spec(
    syscall_api: SyscallApiVersion,
    code: &[u8],
    initial_memory: &[u8],
    initial_memory_start: usize,
    memory_bytes: usize,
    spec: Option<ArtifactSpec<'_>>,
) -> Result<Vec<u8>> {
    let body = spec.map(encode_spec_extension).transpose()?;
    let extensions: Vec<ArtifactExtension<'_>> = body
        .as_deref()
        .map(|body| ArtifactExtension {
            kind: EXTENSION_KIND_CONTRACT_SPEC,
            body,
        })
        .into_iter()
        .collect();
    build_artifact_with_extensions(
        syscall_api,
        code,
        initial_memory,
        initial_memory_start,
        memory_bytes,
        &extensions,
    )
}

/// Assemble a v6 artifact carrying `extensions`, which must be in strictly
/// ascending, nonzero `kind` order, for a guest built against `syscall_api`.
pub fn build_artifact_with_extensions(
    syscall_api: SyscallApiVersion,
    code: &[u8],
    initial_memory: &[u8],
    initial_memory_start: usize,
    memory_bytes: usize,
    extensions: &[ArtifactExtension<'_>],
) -> Result<Vec<u8>> {
    let extensions = encode_extensions(extensions)?;
    // This format describes one major. The minor is the guest's: a chain,
    // not the producer, decides how recent a minor it admits.
    if syscall_api.major != SYSCALL_API_MAJOR {
        return Err(invalid(format!(
            "syscall API major {} is not {SYSCALL_API_MAJOR}, the one this artifact format \
             describes",
            syscall_api.major
        )));
    }
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
    header.extend_from_slice(&syscall_api.major.to_le_bytes());
    header.extend_from_slice(&syscall_api.minor.to_le_bytes());
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
    artifact.extend_from_slice(&extensions);
    Ok(artifact)
}

/// The sizes an artifact header declares, as a chain's admission bounds
/// (`max_rv32_memory_bytes` and `max_rv32_code_bytes` in its network config)
/// read them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredSizes {
    /// Guest data address space the artifact asks the VM to provision
    /// (`memory_bytes` in the header).
    pub memory_bytes: u32,
    /// Bytes in the code section (`code_size` in the header).
    pub code_bytes: u32,
}

/// Read the sizes a v5 or v6 artifact header declares, without verifying the
/// rest.
///
/// `None` when `artifact` does not carry a readable header (too short, wrong
/// magic, or a format version not in [`READABLE_FORMAT_VERSIONS`]): such
/// bytes are not an artifact this crate can speak for, and what to do with
/// them is the caller's decision. The sizes are reported as stamped, so a
/// header that disagrees with its body is reported as it is;
/// [`verify_artifact`] checks the shape.
pub fn declared_sizes(artifact: &[u8]) -> Option<DeclaredSizes> {
    let layout = HeaderLayout::read(artifact)?;
    Some(DeclaredSizes {
        memory_bytes: layout.u32_at(artifact, MEMORY_BYTES_OFFSET),
        code_bytes: layout.u32_at(artifact, CODE_SIZE_OFFSET),
    })
}

/// Which of the readable header layouts an artifact has.
#[derive(Debug, Clone, Copy)]
struct HeaderLayout {
    format_version: u32,
    /// [`HEADER_BYTES`], or [`LEGACY_HEADER_BYTES`] for a header that declares
    /// a gas schedule.
    header_bytes: usize,
}

impl HeaderLayout {
    /// `None` unless `artifact` starts with a whole header of a readable
    /// format version and the size that version allows: 128 bytes for v5, and
    /// for v6 either that or 120.
    fn read(artifact: &[u8]) -> Option<Self> {
        if artifact.len() < HEADER_BYTES || artifact[..MAGIC.len()] != MAGIC {
            return None;
        }
        let read_u32 = |off: usize| -> u32 {
            u32::from_le_bytes(
                artifact[off..off + 4]
                    .try_into()
                    .expect("bug: 4-byte slice"),
            )
        };
        let format_version = read_u32(FORMAT_VERSION_OFFSET);
        let header_bytes = read_u32(HEADER_SIZE_OFFSET) as usize;
        let readable = matches!(
            (format_version, header_bytes),
            (FORMAT_VERSION, HEADER_BYTES)
                | (
                    FORMAT_VERSION | LEGACY_FORMAT_VERSION_V5,
                    LEGACY_HEADER_BYTES
                )
        );
        (readable && artifact.len() >= header_bytes).then_some(Self {
            format_version,
            header_bytes,
        })
    }

    /// Whether the header declares a syscall API version, as opposed to a gas
    /// schedule.
    fn declares_syscall_api(self) -> bool {
        self.header_bytes == HEADER_BYTES
    }

    /// Where the field at v6 offset `off` sits in this header.
    fn at(self, off: usize) -> usize {
        if off >= MEMORY_BYTES_OFFSET {
            off + (self.header_bytes - HEADER_BYTES)
        } else {
            off
        }
    }

    fn u32_at(self, artifact: &[u8], off: usize) -> u32 {
        let at = self.at(off);
        u32::from_le_bytes(artifact[at..at + 4].try_into().expect("bug: 4-byte slice"))
    }
}

/// The syscall API version a v6 artifact declares.
///
/// `None` when `artifact` does not carry a readable header that declares one:
/// a v5 artifact, or a v6 artifact written before v6 did, declares a gas
/// schedule instead. The rest of the artifact is not verified.
pub fn declared_syscall_api(artifact: &[u8]) -> Option<SyscallApiVersion> {
    let layout = HeaderLayout::read(artifact)?;
    layout.declares_syscall_api().then(|| SyscallApiVersion {
        major: layout.u32_at(artifact, SYSCALL_API_MAJOR_OFFSET),
        minor: layout.u32_at(artifact, SYSCALL_API_MINOR_OFFSET),
    })
}

/// Re-parse a freshly-written artifact and confirm magic, the format/ABI
/// stamps, the canonical section layout, the two section digests, and the
/// extension list's framing. Cheap insurance against a writer regression
/// slipping past until the C++ parser flags it.
///
/// This is the producer check, so it accepts only [`FORMAT_VERSION`] with a
/// [`HEADER_BYTES`] header. Readers of deployed artifacts use
/// [`artifact_extensions`] or [`artifact_spec`], which also accept the
/// headers that declare a gas schedule.
pub fn verify_artifact(artifact: &[u8]) -> Result<()> {
    let container = verify_container(artifact)?;
    if container.format_version != FORMAT_VERSION {
        return Err(verify(format!(
            "format_version {} (producers emit {FORMAT_VERSION})",
            container.format_version
        )));
    }
    if !container.layout.declares_syscall_api() {
        return Err(verify(format!(
            "header_bytes {LEGACY_HEADER_BYTES} (producers emit {HEADER_BYTES})"
        )));
    }
    if artifact[MEMORY_BYTES_OFFSET..MEMORY_BYTES_OFFSET + 4] == [0; 4] {
        return Err(verify("memory_bytes out of range"));
    }
    Ok(())
}

/// What [`verify_container`] established about a structurally valid artifact.
struct Container<'a> {
    format_version: u32,
    layout: HeaderLayout,
    extensions: Vec<ArtifactExtension<'a>>,
}

// Readers validate the envelope, not the producer's execution settings.
fn verify_container(artifact: &[u8]) -> Result<Container<'_>> {
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

    let format_version = read_u32(FORMAT_VERSION_OFFSET);
    if !READABLE_FORMAT_VERSIONS.contains(&format_version) {
        return Err(verify(format!("format_version {format_version}")));
    }
    // v6 fixes the header at HEADER_BYTES; a header that declares a gas
    // schedule, in v5 or an early v6, is LEGACY_HEADER_BYTES.
    let Some(layout) = HeaderLayout::read(artifact) else {
        return Err(verify(format!(
            "header_bytes {}",
            read_u32(HEADER_SIZE_OFFSET)
        )));
    };
    let header_bytes = layout.header_bytes;
    if read_u64(VM_PROFILE_OFFSET) != VM_PROFILE_DIGEST {
        return Err(verify("vm_profile mismatch"));
    }
    if read_u32(ENTRY_ABI_OFFSET) != ENTRY_ABI_VERSION {
        return Err(verify("entry_abi mismatch"));
    }
    if read_u32(SYSCALL_API_MAJOR_OFFSET) != SYSCALL_API_MAJOR {
        return Err(verify("syscall API major mismatch"));
    }
    if read_u32(FLAGS_OFFSET) != FLAGS_REQUIRED_VALUE {
        return Err(verify("flags must be zero"));
    }

    let memory_bytes = layout.u32_at(artifact, MEMORY_BYTES_OFFSET) as usize;
    let code_size = layout.u32_at(artifact, CODE_SIZE_OFFSET) as usize;
    let init_mem_size = layout.u32_at(artifact, INITIAL_MEMORY_SIZE_OFFSET) as usize;
    let init_mem_start = layout.u32_at(artifact, INITIAL_MEMORY_START_OFFSET) as usize;

    // Canonical layout: header || code || initial_memory || extensions (v6)
    // with no gaps or trailing bytes, and the stored memory prefix fits the
    // declared data address space. This mirrors the structural acceptance set
    // the consensus C++ loader enforces, so the writer's self-check rejects
    // anything the loader would on shape alone. How large `code_size` and
    // `memory_bytes` may be is the deploying chain's configuration, checked
    // there rather than here.
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
    // Section positions are implicit: code after the header, initial_memory
    // directly after. A v5 file ends there; a v6 file's extension list runs
    // from there to the end. Checked math for the same 32-bit wrap reason as
    // above.
    let sections_end = header_bytes
        .checked_add(code_size)
        .and_then(|len| len.checked_add(init_mem_size))
        .ok_or_else(|| verify("non-canonical section layout"))?;
    if sections_end > artifact.len() {
        return Err(verify("non-canonical section layout"));
    }
    let extensions = if format_version == LEGACY_FORMAT_VERSION_V5 {
        if sections_end != artifact.len() {
            return Err(verify("non-canonical section layout"));
        }
        Vec::new()
    } else {
        decode_extensions(&artifact[sections_end..])?
    };

    let code_dig_at = layout.at(CODE_DIGEST_OFFSET);
    let mem_dig_at = layout.at(INITIAL_MEMORY_DIGEST_OFFSET);
    let code_dig = &artifact[code_dig_at..code_dig_at + DIGEST_BYTES];
    let mem_dig = &artifact[mem_dig_at..mem_dig_at + DIGEST_BYTES];

    let code = &artifact[header_bytes..header_bytes + code_size];
    let memory = &artifact[header_bytes + code_size..sections_end];

    if sha256(code).as_slice() != code_dig {
        return Err(verify("code digest mismatch"));
    }
    if sha256(memory).as_slice() != mem_dig {
        return Err(verify("initial_memory digest mismatch"));
    }

    Ok(Container {
        format_version: layout.format_version,
        layout,
        extensions,
    })
}

/// Validate a v5 or v6 container and borrow its extensions, without
/// interpreting any body. A v5 artifact has none. Callers must verify the
/// enclosing contract hash to authenticate these bytes.
pub fn artifact_extensions(artifact: &[u8]) -> Result<Vec<ArtifactExtension<'_>>> {
    Ok(verify_container(artifact)?.extensions)
}

/// Validate a v5 or v6 container and borrow its contract spec, if it carries
/// one. The spec's schema is not interpreted. Callers must verify the
/// enclosing contract hash to authenticate these bytes.
pub fn artifact_spec(artifact: &[u8]) -> Result<Option<ArtifactSpec<'_>>> {
    artifact_extensions(artifact)?
        .into_iter()
        .find(|extension| extension.kind == EXTENSION_KIND_CONTRACT_SPEC)
        .map(|extension| decode_spec_extension(extension.body))
        .transpose()
}

/// Rewrites of this crate's v6 output into the header layouts only readers
/// handle, for tests: nothing produces them any more.
#[doc(hidden)]
pub mod testing {
    use super::{
        FORMAT_VERSION_OFFSET, HEADER_SIZE_OFFSET, LEGACY_FORMAT_VERSION_V5, LEGACY_HEADER_BYTES,
        MEMORY_BYTES_OFFSET, SYSCALL_API_MINOR_OFFSET,
    };

    /// The gas table version and digest the last gas-stamping lowerer wrote.
    pub const GAS_TABLE_VERSION: u32 = 3;
    pub const GAS_SCHEDULE_DIGEST: u64 = 0x9183_17c2_1c46_bb60;

    /// A v6 artifact as written before v6 declared a syscall API version:
    /// the same body under a 128-byte header that declares a gas schedule.
    #[must_use]
    pub fn with_gas_schedule_header(artifact: &[u8]) -> Vec<u8> {
        let mut out = artifact[..MEMORY_BYTES_OFFSET].to_vec();
        out[SYSCALL_API_MINOR_OFFSET..].copy_from_slice(&GAS_TABLE_VERSION.to_le_bytes());
        out.extend_from_slice(&GAS_SCHEDULE_DIGEST.to_le_bytes());
        out.extend_from_slice(&artifact[MEMORY_BYTES_OFFSET..]);
        let header_bytes = u32::try_from(LEGACY_HEADER_BYTES).expect("bug: fits in u32");
        out[HEADER_SIZE_OFFSET..HEADER_SIZE_OFFSET + 4]
            .copy_from_slice(&header_bytes.to_le_bytes());
        out
    }

    /// The v5 artifact with the same image as a v6 artifact that has no
    /// extensions: a gas-schedule header, and no extension list.
    #[must_use]
    pub fn as_v5(artifact: &[u8]) -> Vec<u8> {
        let mut out = with_gas_schedule_header(artifact);
        out[FORMAT_VERSION_OFFSET..FORMAT_VERSION_OFFSET + 4]
            .copy_from_slice(&LEGACY_FORMAT_VERSION_V5.to_le_bytes());
        // Drop the empty XDR extension list.
        out.truncate(out.len() - 4);
        out
    }
}
