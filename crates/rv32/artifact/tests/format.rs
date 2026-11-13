//! v6 format writer/verifier contract tests: header field placement, the
//! `initial_memory_start` load-base semantics, and the canonicality rules the
//! verifier enforces on it.

use thruput_rv32_artifact::{
    DEFAULT_MEMORY_BYTES, HEADER_BYTES, build_artifact, sha256, verify_artifact,
};

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

#[test]
fn v6_header_layout_and_field_values() {
    let code = [0u8; 8];
    let memory = [0xAAu8, 0xBB, 0xCC];
    let start = 0x1008;
    let artifact = build_artifact(&code, &memory, start, DEFAULT_MEMORY_BYTES).expect("builds");

    assert_eq!(&artifact[..8], b"GHRV32A1");
    assert_eq!(read_u32(&artifact, 8), 6, "format_version");
    assert_eq!(read_u32(&artifact, 12), 120, "header_bytes");
    assert_eq!(HEADER_BYTES, 120);
    assert_eq!(
        read_u32(&artifact, 28),
        3,
        "entry_abi_version (sp seeded 0)"
    );
    assert_eq!(read_u32(&artifact, 32), 1, "syscall_api_major");
    assert_eq!(read_u32(&artifact, 36), 0, "syscall_api_minor (baseline)");
    assert_eq!(read_u32(&artifact, 40) as usize, DEFAULT_MEMORY_BYTES);
    assert_eq!(read_u32(&artifact, 44), 8, "code_size");
    assert_eq!(read_u32(&artifact, 48), 3, "initial_memory_size");
    assert_eq!(
        read_u32(&artifact, 52) as usize,
        start,
        "initial_memory_start"
    );
    // Section digests directly follow the scalar fields; section positions
    // are implicit (code at HEADER_BYTES, initial_memory right after code).
    assert_eq!(&artifact[56..88], sha256(&code).as_slice(), "code digest");
    assert_eq!(
        &artifact[88..120],
        sha256(&memory).as_slice(),
        "initial_memory digest"
    );
    assert_eq!(&artifact[120..128], &code, "code section placement");
    assert_eq!(
        &artifact[128..131],
        &memory,
        "initial_memory section placement"
    );
    assert_eq!(&artifact[131..], &[0; 4], "empty XDR extension list");
    assert_eq!(artifact.len(), 120 + 8 + 3 + 4);

    verify_artifact(&artifact).expect("self-verifies");
}

#[test]
fn verify_rejects_trailing_bytes_and_truncation() {
    let artifact =
        build_artifact(&[0u8; 8], &[1, 2, 3], 0x100, DEFAULT_MEMORY_BYTES).expect("builds");

    let mut padded = artifact.clone();
    padded.push(0);
    let err = verify_artifact(&padded).expect_err("trailing byte should reject");
    assert!(err.to_string().contains("trailing bytes"), "got: {err}");

    // Cutting into the extension list, or into the sections before it.
    let truncated = &artifact[..artifact.len() - 1];
    let err = verify_artifact(truncated).expect_err("truncation should reject");
    assert!(err.to_string().contains("truncated"), "got: {err}");
    let truncated = &artifact[..artifact.len() - 5];
    let err = verify_artifact(truncated).expect_err("truncation should reject");
    assert!(err.to_string().contains("non-canonical"), "got: {err}");
}

#[test]
fn build_rejects_start_plus_size_past_memory_end() {
    let code = [0u8; 4];
    let memory = vec![1u8; 16];
    // start + 16 == DEFAULT_MEMORY_BYTES is the last valid placement.
    assert!(
        build_artifact(
            &code,
            &memory,
            DEFAULT_MEMORY_BYTES - 16,
            DEFAULT_MEMORY_BYTES
        )
        .is_ok()
    );
    assert!(
        build_artifact(
            &code,
            &memory,
            DEFAULT_MEMORY_BYTES - 15,
            DEFAULT_MEMORY_BYTES
        )
        .is_err()
    );
    // Also guard the overflow path.
    assert!(build_artifact(&code, &memory, usize::MAX, DEFAULT_MEMORY_BYTES).is_err());
}

#[test]
fn build_rejects_nonzero_start_for_empty_image() {
    let code = [0u8; 4];
    assert!(build_artifact(&code, &[], 0, DEFAULT_MEMORY_BYTES).is_ok());
    assert!(build_artifact(&code, &[], 8, DEFAULT_MEMORY_BYTES).is_err());
}

#[test]
fn verify_rejects_start_pushed_past_memory_end() {
    let code = [0u8; 4];
    let memory = vec![1u8; 16];
    let mut artifact = build_artifact(&code, &memory, 0x100, DEFAULT_MEMORY_BYTES).expect("builds");
    // Tamper initial_memory_start (u32 @ 52) so start + size > memory_bytes.
    let bad = u32::try_from(DEFAULT_MEMORY_BYTES - 15).unwrap();
    artifact[52..56].copy_from_slice(&bad.to_le_bytes());
    let err = verify_artifact(&artifact).expect_err("should reject");
    assert!(
        err.to_string().contains("initial_memory_start"),
        "unexpected error: {err}"
    );
}

#[test]
fn verify_rejects_nonzero_start_for_empty_image() {
    let code = [0u8; 4];
    let mut artifact = build_artifact(&code, &[], 0, DEFAULT_MEMORY_BYTES).expect("builds");
    artifact[52..56].copy_from_slice(&8u32.to_le_bytes());
    let err = verify_artifact(&artifact).expect_err("should reject");
    assert!(
        err.to_string().contains("initial_memory_start"),
        "unexpected error: {err}"
    );
}

#[test]
fn build_stamps_the_requested_memory_bytes() {
    // memory_bytes is the caller's: a larger data address space is stamped as
    // given, the stored span is bounded by it rather than by the default, and
    // the verifier accepts the result -- how large a chain admits is checked
    // at deploy, not here.
    let code = [0u8; 4];
    let memory = vec![1u8; 16];
    let larger = 2 * DEFAULT_MEMORY_BYTES;
    let artifact = build_artifact(&code, &memory, larger - 16, larger).expect("builds");
    assert_eq!(read_u32(&artifact, 40) as usize, larger);
    verify_artifact(&artifact).expect("verifies");
    assert!(build_artifact(&code, &memory, larger - 15, larger).is_err());
    assert!(build_artifact(&code, &memory, 0, 0).is_err(), "zero memory");

    // Code past the former 64 KiB constant builds and verifies too.
    let big_code = vec![0u8; DEFAULT_MEMORY_BYTES + 4];
    let artifact = build_artifact(&big_code, &[], 0, DEFAULT_MEMORY_BYTES).expect("builds");
    verify_artifact(&artifact).expect("verifies");
}

#[test]
fn declared_sizes_reads_the_header_and_nothing_else() {
    use thruput_rv32_artifact::{DeclaredSizes, declared_sizes};
    let code = [0u8; 12];
    let memory = vec![1u8; 16];
    let larger = 2 * DEFAULT_MEMORY_BYTES;
    let artifact = build_artifact(&code, &memory, 0x100, larger).expect("builds");

    assert_eq!(
        declared_sizes(&artifact),
        Some(DeclaredSizes {
            memory_bytes: u32::try_from(larger).unwrap(),
            code_bytes: 12,
        })
    );
    // The header alone is enough; the body is not consulted.
    assert_eq!(
        declared_sizes(&artifact[..HEADER_BYTES]),
        declared_sizes(&artifact)
    );

    // v5 headers, and v6 headers that declare a gas schedule, are 8 bytes
    // longer and remain readable.
    let legacy = thruput_rv32_artifact::format::testing::with_gas_schedule_header(&artifact);
    assert_eq!(declared_sizes(&legacy), declared_sizes(&artifact));
    let mut v5 = legacy;
    v5[8..12].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(declared_sizes(&v5), declared_sizes(&artifact));

    // Not a readable artifact: too short, wrong magic, or another format version.
    assert_eq!(declared_sizes(&artifact[..HEADER_BYTES - 1]), None);
    let mut bad_magic = artifact.clone();
    bad_magic[0] ^= 0xff;
    assert_eq!(declared_sizes(&bad_magic), None);
    let mut other_version = artifact.clone();
    other_version[8..12].copy_from_slice(&7u32.to_le_bytes());
    assert_eq!(declared_sizes(&other_version), None);
    assert_eq!(declared_sizes(&[]), None);
}

#[test]
fn declares_the_syscall_api_version_it_is_given() {
    use thruput_rv32_artifact::{
        SYSCALL_API_MAJOR, SyscallApiVersion, build_artifact_with_extensions, declared_syscall_api,
    };

    let baseline = build_artifact(&[0u8; 8], &[], 0, DEFAULT_MEMORY_BYTES).expect("builds");
    assert_eq!(
        declared_syscall_api(&baseline),
        Some(SyscallApiVersion::BASELINE)
    );
    assert_eq!(SyscallApiVersion::BASELINE.to_string(), "1.0");

    let version = SyscallApiVersion {
        major: SYSCALL_API_MAJOR,
        minor: 9,
    };
    let artifact =
        build_artifact_with_extensions(version, &[0u8; 8], &[], 0, DEFAULT_MEMORY_BYTES, &[])
            .expect("builds");
    assert_eq!(declared_syscall_api(&artifact), Some(version));
    assert_eq!(artifact[36..40], 9u32.to_le_bytes());
    verify_artifact(&artifact).expect("verifies");

    // Only the minor is the guest's to choose.
    let other_major = SyscallApiVersion {
        major: SYSCALL_API_MAJOR + 1,
        minor: 0,
    };
    assert!(
        build_artifact_with_extensions(other_major, &[0u8; 8], &[], 0, DEFAULT_MEMORY_BYTES, &[])
            .is_err()
    );

    // A header that declares a gas schedule declares no version.
    let gas = thruput_rv32_artifact::format::testing::with_gas_schedule_header(&baseline);
    assert_eq!(declared_syscall_api(&gas), None);
    assert!(verify_artifact(&gas).is_err());
    assert_eq!(
        declared_syscall_api(&thruput_rv32_artifact::format::testing::as_v5(&baseline)),
        None
    );
}
