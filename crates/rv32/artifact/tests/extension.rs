//! The extension list: XDR framing, canonical order, the contract spec
//! extension, and reading v5 and v6 artifacts.

use thruput_rv32_artifact::format::testing::{as_v5, as_v6};
use thruput_rv32_artifact::{
    ArtifactExtension, ArtifactSpec, EXTENSION_KIND_CONTRACT_SPEC, HEADER_BYTES,
    V5_V6_HEADER_BYTES, artifact_extensions, artifact_spec, build_artifact,
    build_artifact_with_extensions, build_artifact_with_spec, sha256, verify_artifact,
};

const SPEC: ArtifactSpec<'static> = ArtifactSpec {
    schema_version: 1,
    bytes: &[1, 2, 3],
};

// header || code (4) || initial_memory (2), before the extension list.
const SECTIONS_END: usize = HEADER_BYTES + 4 + 2;

fn with_spec(spec: ArtifactSpec<'_>) -> Vec<u8> {
    build_artifact_with_spec(
        thruput_rv32_artifact::SyscallApiVersion::BASELINE,
        &[0; 4],
        &[7, 8],
        14,
        16,
        Some(spec),
    )
    .unwrap()
}

fn with_extensions(extensions: &[ArtifactExtension<'_>]) -> Vec<u8> {
    build_artifact_with_extensions(
        thruput_rv32_artifact::SyscallApiVersion::BASELINE,
        &[0; 4],
        &[7, 8],
        14,
        16,
        extensions,
    )
    .unwrap()
}

/// Replace the extension list of a v7 artifact with raw bytes.
fn with_raw_extensions(raw: &[u8]) -> Vec<u8> {
    let mut artifact = build_artifact(&[0; 4], &[7, 8], 14, 16).unwrap();
    artifact.truncate(SECTIONS_END);
    artifact.extend_from_slice(raw);
    artifact
}

fn be(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

#[test]
fn spec_is_an_xdr_extension_outside_guest_memory() {
    let artifact = with_spec(SPEC);
    assert_eq!(&artifact[HEADER_BYTES + 4..SECTIONS_END], &[7, 8]);
    let mut expected = be(&[1, EXTENSION_KIND_CONTRACT_SPEC, 12, 1, 3]);
    expected.extend_from_slice(&[1, 2, 3, 0]);
    assert_eq!(&artifact[SECTIONS_END..], expected.as_slice());
    assert_eq!(artifact_spec(&artifact).unwrap(), Some(SPEC));
    verify_artifact(&artifact).unwrap();
}

#[test]
fn absent_extensions_have_one_encoding() {
    let artifact = build_artifact(&[0; 4], &[7, 8], 14, 16).unwrap();
    assert_eq!(&artifact[SECTIONS_END..], &[0; 4]);
    assert_eq!(artifact_extensions(&artifact).unwrap(), vec![]);
    assert_eq!(artifact_spec(&artifact).unwrap(), None);
    // A v7 artifact must carry the list, even when it is empty.
    assert!(artifact_extensions(&artifact[..SECTIONS_END]).is_err());
}

#[test]
fn unknown_kinds_and_spec_schemas_round_trip() {
    let spec_body = thruput_rv32_artifact::encode_spec_extension(ArtifactSpec {
        schema_version: 99,
        bytes: &[4, 5],
    })
    .unwrap();
    let extensions = [
        ArtifactExtension {
            kind: EXTENSION_KIND_CONTRACT_SPEC,
            body: &spec_body,
        },
        ArtifactExtension {
            kind: 7,
            body: &[9; 5],
        },
        ArtifactExtension {
            kind: u32::MAX,
            body: &[],
        },
    ];
    let artifact = with_extensions(&extensions);
    verify_artifact(&artifact).unwrap();
    assert_eq!(artifact_extensions(&artifact).unwrap(), extensions);
    assert_eq!(
        artifact_spec(&artifact).unwrap(),
        Some(ArtifactSpec {
            schema_version: 99,
            bytes: &[4, 5],
        })
    );
}

#[test]
fn builder_refuses_non_canonical_order() {
    let ext = |kind| ArtifactExtension { kind, body: &[] };
    for extensions in [vec![ext(0)], vec![ext(2), ext(2)], vec![ext(3), ext(2)]] {
        assert!(
            build_artifact_with_extensions(
                thruput_rv32_artifact::SyscallApiVersion::BASELINE,
                &[0; 4],
                &[],
                0,
                16,
                &extensions
            )
            .is_err(),
            "{extensions:?}"
        );
    }
}

#[test]
fn reader_enforces_the_chain_framing_rules() {
    let mut nonzero_padding = be(&[1, 7, 1]);
    nonzero_padding.extend_from_slice(&[0xaa, 0, 0, 1]);
    let mut trailing = be(&[1, 7, 0]);
    trailing.push(0);
    for (raw, why) in [
        (be(&[1, 0, 0]), "kind 0"),
        (be(&[2, 5, 0, 5, 0]), "duplicate kind"),
        (be(&[2, 5, 0, 4, 0]), "descending kinds"),
        (nonzero_padding, "nonzero padding"),
        (trailing, "trailing bytes"),
        (be(&[u32::MAX]), "count past the end"),
        (be(&[1, 7, 8]), "body past the end"),
        (vec![], "missing list"),
    ] {
        let artifact = with_raw_extensions(&raw);
        assert!(artifact_extensions(&artifact).is_err(), "{why}");
        assert!(verify_artifact(&artifact).is_err(), "{why}");
    }
}

#[test]
fn reader_rejects_a_malformed_spec_body_but_not_the_container() {
    // The chain does not interpret bodies, so the container stays valid.
    let artifact = with_extensions(&[ArtifactExtension {
        kind: EXTENSION_KIND_CONTRACT_SPEC,
        body: &[0, 0, 0, 1],
    }]);
    verify_artifact(&artifact).unwrap();
    assert!(artifact_spec(&artifact).is_err());
}

#[test]
fn reader_rejects_inner_spec_padding_but_not_the_container() {
    let mut body = be(&[1, 1]);
    body.extend_from_slice(&[0xaa, 0, 0, 1]);
    let artifact = with_extensions(&[ArtifactExtension {
        kind: EXTENSION_KIND_CONTRACT_SPEC,
        body: &body,
    }]);
    verify_artifact(&artifact).unwrap();
    let error = artifact_spec(&artifact).unwrap_err();
    assert!(error.to_string().contains("nonzero XDR padding"), "{error}");
}

#[test]
fn reader_rejects_inner_spec_trailing_bytes_but_not_the_container() {
    let body = be(&[1, 0, 0]);
    let artifact = with_extensions(&[ArtifactExtension {
        kind: EXTENSION_KIND_CONTRACT_SPEC,
        body: &body,
    }]);
    verify_artifact(&artifact).unwrap();
    let error = artifact_spec(&artifact).unwrap_err();
    assert!(error.to_string().contains("trailing bytes"), "{error}");
}

#[test]
fn every_truncation_is_rejected() {
    let artifact = with_spec(SPEC);
    for end in 0..artifact.len() {
        assert!(artifact_extensions(&artifact[..end]).is_err(), "{end}");
    }
}

#[test]
fn v5_reads_as_v6_without_extensions() {
    let v5 = as_v5(&build_artifact(&[0; 4], &[7, 8], 14, 16).unwrap());
    assert_eq!(v5.len(), V5_V6_HEADER_BYTES + 4 + 2);
    assert_eq!(artifact_extensions(&v5).unwrap(), vec![]);
    assert_eq!(artifact_spec(&v5).unwrap(), None);
    // Producers emit v7 only.
    assert!(verify_artifact(&v5).is_err());

    // v5 has no extension list: anything after initial memory is trailing.
    let mut trailing = v5.clone();
    trailing.extend_from_slice(&[0; 4]);
    assert!(artifact_extensions(&trailing).is_err());

    let mut unknown = v5;
    unknown[8..12].copy_from_slice(&4u32.to_le_bytes());
    assert!(artifact_extensions(&unknown).is_err());
}

#[test]
fn reader_opens_v6_artifacts() {
    // Deployed v6 artifacts have a 128-byte header with a gas table version
    // and digest. Readers still open them; the producer check does not accept
    // them.
    let current = with_spec(SPEC);
    let mut artifact = as_v6(&current);
    assert_eq!(artifact.len(), current.len() + 8);
    assert_eq!(artifact_spec(&artifact).unwrap(), Some(SPEC));
    assert!(verify_artifact(&artifact).is_err());
    assert_eq!(thruput_rv32_artifact::declared_syscall_api(&artifact), None);
    assert_eq!(
        thruput_rv32_artifact::declared_sizes(&artifact),
        thruput_rv32_artifact::declared_sizes(&current)
    );
    // The gas fields are not interpreted.
    artifact[36..40].copy_from_slice(&4u32.to_le_bytes());
    artifact[40..48].copy_from_slice(&0xa958_d226_df65_8598_u64.to_le_bytes());
    assert_eq!(artifact_spec(&artifact).unwrap(), Some(SPEC));
    artifact[V5_V6_HEADER_BYTES] ^= 1;
    assert!(artifact_spec(&artifact).is_err());

    // A header size belongs to its version: v5 and v6 never have the v7 one,
    // and v7 never has theirs.
    for version in [5u32, 6] {
        let mut short = current.clone();
        short[8..12].copy_from_slice(&version.to_le_bytes());
        assert!(artifact_spec(&short).is_err(), "v{version}");
    }
    let mut long_v7 = as_v6(&current);
    long_v7[8..12].copy_from_slice(&7u32.to_le_bytes());
    assert!(artifact_spec(&long_v7).is_err());
}

#[test]
fn reader_accepts_zero_memory_but_still_checks_initial_memory_span() {
    let mut artifact = build_artifact_with_spec(
        thruput_rv32_artifact::SyscallApiVersion::BASELINE,
        &[0; 4],
        &[],
        0,
        16,
        Some(SPEC),
    )
    .unwrap();
    artifact[40..44].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(artifact_spec(&artifact).unwrap(), Some(SPEC));
    assert!(verify_artifact(&artifact).is_err());
    artifact[52..56].copy_from_slice(&1u32.to_le_bytes());
    assert!(artifact_spec(&artifact).is_err());
}

#[test]
fn extensions_change_artifact_identity_without_changing_execution_sections() {
    let artifact = with_spec(SPEC);
    let mut changed = artifact.clone();
    // The last spec byte, before its XDR padding.
    changed[artifact.len() - 2] ^= 1;
    verify_artifact(&changed).unwrap();
    assert_eq!(&artifact[..SECTIONS_END], &changed[..SECTIONS_END]);
    assert_ne!(sha256(&artifact), sha256(&changed));
}
