use groundhog_rv32_artifact::format::{
    FORMAT_VERSION, MAGIC, compute_format_fingerprint_with_domain,
    compute_format_fingerprint_with_format_version, compute_format_fingerprint_with_magic,
};
use groundhog_rv32_artifact::{FORMAT_FINGERPRINT, compute_format_fingerprint};

#[test]
fn fingerprint_is_pinned() {
    assert_eq!(compute_format_fingerprint(), 0x3892_7fa2_342c_674a);
    assert_eq!(FORMAT_FINGERPRINT, 0x3892_7fa2_342c_674a);
}

#[test]
fn fingerprint_is_sensitive_to_folded_constants() {
    let base = compute_format_fingerprint();

    assert_ne!(
        compute_format_fingerprint_with_format_version(FORMAT_VERSION + 1),
        base
    );

    let mut magic = MAGIC;
    magic[0] = magic[0].wrapping_add(1);
    assert_ne!(compute_format_fingerprint_with_magic(magic), base);

    assert_ne!(
        compute_format_fingerprint_with_domain(b"groundhog.rv32.artifact.format.v4"),
        base
    );
}
