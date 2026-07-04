//! v5 format writer/verifier contract tests: header field placement, the
//! `initial_memory_start` load-base semantics, and the canonicality rules the
//! verifier enforces on it.

use groundhog_rv32_artifact::{
    HEADER_BYTES, MAX_MEMORY_BYTES, build_artifact, sha256, verify_artifact,
};

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

#[test]
fn v5_header_layout_and_field_values() {
    let code = [0u8; 8];
    let memory = [0xAAu8, 0xBB, 0xCC];
    let start = 0x1008;
    let artifact = build_artifact(&code, &memory, start).expect("builds");

    assert_eq!(&artifact[..8], b"GHRV32A1");
    assert_eq!(read_u32(&artifact, 8), 5, "format_version");
    assert_eq!(read_u32(&artifact, 12), 128, "header_bytes");
    assert_eq!(HEADER_BYTES, 128);
    assert_eq!(
        read_u32(&artifact, 28),
        3,
        "entry_abi_version (sp seeded 0)"
    );
    assert_eq!(read_u32(&artifact, 48) as usize, MAX_MEMORY_BYTES);
    assert_eq!(read_u32(&artifact, 52), 8, "code_size");
    assert_eq!(read_u32(&artifact, 56), 3, "initial_memory_size");
    assert_eq!(
        read_u32(&artifact, 60) as usize,
        start,
        "initial_memory_start"
    );
    // Section digests directly follow the scalar fields; section positions
    // are implicit (code at HEADER_BYTES, initial_memory right after code).
    assert_eq!(&artifact[64..96], sha256(&code).as_slice(), "code digest");
    assert_eq!(
        &artifact[96..128],
        sha256(&memory).as_slice(),
        "initial_memory digest"
    );
    assert_eq!(&artifact[128..136], &code, "code section placement");
    assert_eq!(
        &artifact[136..],
        &memory,
        "initial_memory section placement"
    );
    assert_eq!(artifact.len(), 128 + 8 + 3);

    verify_artifact(&artifact).expect("self-verifies");
}

#[test]
fn verify_rejects_trailing_bytes_and_truncation() {
    let artifact = build_artifact(&[0u8; 8], &[1, 2, 3], 0x100).expect("builds");

    let mut padded = artifact.clone();
    padded.push(0);
    let err = verify_artifact(&padded).expect_err("trailing byte should reject");
    assert!(err.to_string().contains("non-canonical"), "got: {err}");

    let truncated = &artifact[..artifact.len() - 1];
    let err = verify_artifact(truncated).expect_err("truncation should reject");
    assert!(err.to_string().contains("non-canonical"), "got: {err}");
}

#[test]
fn build_rejects_start_plus_size_past_memory_end() {
    let code = [0u8; 4];
    let memory = vec![1u8; 16];
    // start + 16 == MAX_MEMORY_BYTES is the last valid placement.
    assert!(build_artifact(&code, &memory, MAX_MEMORY_BYTES - 16).is_ok());
    assert!(build_artifact(&code, &memory, MAX_MEMORY_BYTES - 15).is_err());
    // Also guard the overflow path.
    assert!(build_artifact(&code, &memory, usize::MAX).is_err());
}

#[test]
fn build_rejects_nonzero_start_for_empty_image() {
    let code = [0u8; 4];
    assert!(build_artifact(&code, &[], 0).is_ok());
    assert!(build_artifact(&code, &[], 8).is_err());
}

#[test]
fn verify_rejects_start_pushed_past_memory_end() {
    let code = [0u8; 4];
    let memory = vec![1u8; 16];
    let mut artifact = build_artifact(&code, &memory, 0x100).expect("builds");
    // Tamper initial_memory_start (u32 @ 60) so start + size > memory_bytes.
    let bad = u32::try_from(MAX_MEMORY_BYTES - 15).unwrap();
    artifact[60..64].copy_from_slice(&bad.to_le_bytes());
    let err = verify_artifact(&artifact).expect_err("should reject");
    assert!(
        err.to_string().contains("initial_memory_start"),
        "unexpected error: {err}"
    );
}

#[test]
fn verify_rejects_nonzero_start_for_empty_image() {
    let code = [0u8; 4];
    let mut artifact = build_artifact(&code, &[], 0).expect("builds");
    artifact[60..64].copy_from_slice(&8u32.to_le_bytes());
    let err = verify_artifact(&artifact).expect_err("should reject");
    assert!(
        err.to_string().contains("initial_memory_start"),
        "unexpected error: {err}"
    );
}
