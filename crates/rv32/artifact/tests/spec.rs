use thruput_rv32_artifact::{
    ArtifactSpec, artifact_spec, build_artifact, build_artifact_with_spec, sha256, verify_artifact,
};

fn fixture(version: u32) -> Vec<u8> {
    build_artifact_with_spec(
        &[0; 4],
        &[7, 8],
        14,
        16,
        Some(ArtifactSpec {
            schema_version: version,
            bytes: &[1, 2, 3],
        }),
    )
    .unwrap()
}

#[test]
fn spec_is_outside_guest_memory_and_unknown_schemas_round_trip() {
    let artifact = fixture(99);
    assert_eq!(artifact.len(), 136 + 4 + 2 + 3);
    assert_eq!(&artifact[140..142], &[7, 8]);
    assert_eq!(
        artifact_spec(&artifact).unwrap(),
        Some(ArtifactSpec {
            schema_version: 99,
            bytes: &[1, 2, 3],
        })
    );
}

#[test]
fn absent_spec_has_one_encoding() {
    let artifact = build_artifact(&[0; 4], &[], 0, 16).unwrap();
    assert_eq!(artifact_spec(&artifact).unwrap(), None);
    for (version, size) in [(0u32, 1u32), (1, 0)] {
        let mut malformed = artifact.clone();
        malformed[128..132].copy_from_slice(&version.to_le_bytes());
        malformed[132..136].copy_from_slice(&size.to_le_bytes());
        assert!(verify_artifact(&malformed).is_err());
    }
}

#[test]
fn reader_accepts_runtime_gas_schedule_without_relaxing_producer_checks() {
    let mut artifact = fixture(1);
    artifact[36..40].copy_from_slice(&4u32.to_le_bytes());
    artifact[40..48].copy_from_slice(&0xa958d226df658598u64.to_le_bytes());
    assert_eq!(artifact_spec(&artifact).unwrap().unwrap().bytes, &[1, 2, 3]);
    assert!(verify_artifact(&artifact).is_err());
    artifact[136] ^= 1;
    assert!(artifact_spec(&artifact).is_err());
}

#[test]
fn reader_accepts_zero_memory_but_still_checks_initial_memory_span() {
    let mut artifact = build_artifact_with_spec(
        &[0; 4],
        &[],
        0,
        16,
        Some(ArtifactSpec {
            schema_version: 1,
            bytes: &[1, 2, 3],
        }),
    )
    .unwrap();
    artifact[48..52].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(artifact_spec(&artifact).unwrap().unwrap().bytes, &[1, 2, 3]);
    assert!(verify_artifact(&artifact).is_err());
    artifact[60..64].copy_from_slice(&1u32.to_le_bytes());
    assert!(artifact_spec(&artifact).is_err());
}

#[test]
fn spec_length_must_match_the_complete_body() {
    for size in [0u32, 2, 4, u32::MAX] {
        let mut artifact = fixture(1);
        artifact[132..136].copy_from_slice(&size.to_le_bytes());
        assert!(artifact_spec(&artifact).is_err());
    }
    let artifact = fixture(1);
    for end in 0..artifact.len() {
        assert!(artifact_spec(&artifact[..end]).is_err());
    }
}

#[test]
fn spec_changes_artifact_identity_without_changing_execution_sections() {
    let artifact = fixture(1);
    let mut changed = artifact.clone();
    *changed.last_mut().unwrap() ^= 1;
    verify_artifact(&changed).unwrap();
    assert_eq!(&artifact[..142], &changed[..142]);
    assert_ne!(sha256(&artifact), sha256(&changed));
}
