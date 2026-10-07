use std::{
    env,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tempfile::TempDir;

use super::{
    DEFAULT_HOGD_DEV_BINARY, DEVNODE_BINARY_ENV, HOGD_LOG_FORMAT_ENV, HogdDevProcess,
    PERSISTENT_PID_FILE_NAME, RuntimeManifestChainState,
    build_hogd_dev_spec_with_bootstrap_manifest, parse_readiness_line, prepare_persistent_data_dir,
    resolve_hogd_dev_binary, resolve_hogd_dev_binary_before_path, resolve_hogd_dev_binary_on_path,
    validate_bootstrap_accounts, validate_runtime_identity, write_bootstrap_manifest,
};
use crate::{
    ConfigParamName, HogdDevConfig, NodeSupervisorError,
    test_support::{FAKE_HOGD_DEV_MODE_ENV, fake_hogd_dev_binary},
};

const V1_READY: &str = r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345"}}"#;
const COMPACT_READY: &str = r#"{"status":"ready","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler":"127.0.0.1:12345","read":"127.0.0.1:12346","gateway":"127.0.0.1:12347","view":"127.0.0.1:12348"}}"#;
const MANIFEST_SCHEDULER_ENDPOINTS: &str =
    r#"{"scheduler":"127.0.0.1:12345","schedulerGateway":"127.0.0.1:12349"}"#;
const GRACEFUL_SHUTDOWN_FILE_ENV: &str = "GROUNDHOG_FAKE_GRACEFUL_SHUTDOWN_FILE";

#[test]
fn command_shape_matches_contract() {
    let config = HogdDevConfig::default()
        .with_listen_host("127.0.0.2")
        .with_data_dir("/tmp/hog-devnode-data");
    let spec = build_hogd_dev_spec_with_bootstrap_manifest(
        "/tmp/hog-devnode",
        &config,
        "/tmp/bootstrap-manifest.json",
    );

    assert_eq!(spec.program(), std::ffi::OsStr::new("/tmp/hog-devnode"));
    let args = spec.program_args();
    assert_eq!(
        args,
        &[
            OsString::from("--listen-host"),
            OsString::from("127.0.0.2"),
            OsString::from("--scheduler-port"),
            OsString::from("0"),
            OsString::from("--readiness"),
            OsString::from("stdout"),
            OsString::from("--bootstrap-manifest"),
            OsString::from("/tmp/bootstrap-manifest.json"),
            OsString::from("--data-dir"),
            OsString::from("/tmp/hog-devnode-data"),
        ]
    );
    assert_eq!(
        spec.program_envs(),
        &[(OsString::from(HOGD_LOG_FORMAT_ENV), OsString::from("json"))]
    );
}

#[test]
fn network_config_overrides_are_forwarded_in_both_launch_paths() {
    let config = HogdDevConfig::default().with_network_config_overrides(
        [
            (ConfigParamName::MaxRv32MemoryBytes, 16_777_216),
            (ConfigParamName::MaxBytestringLen, 32_768),
        ]
        .into(),
    );
    let specs = [
        build_hogd_dev_spec_with_bootstrap_manifest(
            "/tmp/hog-devnode",
            &config,
            "/tmp/bootstrap.json",
        ),
        build_hogd_dev_spec_with_bootstrap_manifest(
            "/tmp/hog-devnode",
            &config,
            "/tmp/bootstrap.json",
        ),
    ];
    for spec in specs {
        let forwarded: Vec<_> = spec
            .program_args()
            .windows(2)
            .filter(|pair| pair[0] == "--network-config")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .collect();
        // Protocol id order, since that is how `ConfigParamName` sorts.
        assert_eq!(
            forwarded,
            ["max_bytestring_len=32768", "max_rv32_memory_bytes=16777216"]
        );
    }
}

#[test]
fn ramdb_bytes_forwarded_when_set() {
    let config = HogdDevConfig::default().with_ramdb_bytes(2_147_483_648);

    let supervisor_spec = build_hogd_dev_spec_with_bootstrap_manifest(
        "/tmp/hog-devnode",
        &config,
        "/tmp/bootstrap-manifest.json",
    );
    let compat_spec = build_hogd_dev_spec_with_bootstrap_manifest(
        "/tmp/hog-devnode",
        &config,
        "/tmp/bootstrap.json",
    );

    for spec in [supervisor_spec, compat_spec] {
        let args = spec.program_args();
        let flag_index = args
            .iter()
            .position(|arg| arg == "--ramdb-bytes")
            .expect("bug: --ramdb-bytes forwarded");
        assert_eq!(args[flag_index + 1], OsString::from("2147483648"));
    }
}

#[test]
fn contract_artifact_directories_are_forwarded_in_order() {
    let config = HogdDevConfig::default()
        .with_contract_artifact_directories(["/tmp/contracts-a", "/tmp/contracts-b"]);

    let supervisor_spec = build_hogd_dev_spec_with_bootstrap_manifest(
        "/tmp/hog-devnode",
        &config,
        "/tmp/bootstrap-manifest.json",
    );
    let compat_spec = build_hogd_dev_spec_with_bootstrap_manifest(
        "/tmp/hog-devnode",
        &config,
        "/tmp/bootstrap.json",
    );

    for spec in [supervisor_spec, compat_spec] {
        let forwarded = spec
            .program_args()
            .windows(2)
            .filter(|args| args[0] == "--contract-artifacts-dir")
            .map(|args| args[1].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            forwarded,
            [
                OsString::from("/tmp/contracts-a"),
                OsString::from("/tmp/contracts-b"),
            ]
        );
    }
}

#[test]
fn bootstrap_manifest_file_serializes_config() {
    let config = HogdDevConfig::default().with_bootstrap_accounts(vec![
        groundhog_json_rpc_types::bootstrap_manifest::BootstrapManifestAllocation::new(
            fixture_public_key(1),
            "1234",
        ),
        groundhog_json_rpc_types::bootstrap_manifest::BootstrapManifestAllocation::new(
            fixture_public_key(2),
            "1234",
        ),
    ]);

    let manifest = write_bootstrap_manifest(&config, &[0; 32].into(), fixture_public_key(0))
        .expect("bug: bootstrap manifest file should be written");
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(manifest.path()).expect("bug: bootstrap manifest should be readable"),
    )
    .expect("bug: bootstrap manifest should parse");

    assert_eq!(value["kind"], "groundhog-bootstrap");
    assert_eq!(
        value["networkHash"],
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
    assert_eq!(value["profile"], "local-dev");
    assert!(value.get("devWallet").is_none());
    assert_eq!(value["schemaVersion"], 4);
    assert!(value["bootstrapFaucet"].get("index").is_none());
    assert_eq!(
        value["bootstrapFaucet"]["initialStorageTokens"],
        "10000000000000"
    );
    assert_eq!(value["bootstrapAccounts"].as_array().unwrap().len(), 2);
    assert_eq!(
        value["bootstrapAccounts"][0]["initialStorageTokens"],
        "1234"
    );
    assert_eq!(
        value["bootstrapAccounts"][0]["publicKey"],
        serde_json::to_value(fixture_public_key(1)).unwrap()
    );
    assert_eq!(value["storageCostModel"]["profile"], "hogd-default");
}

#[test]
fn runtime_identity_rejects_non_bootstrap_network() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut readiness =
        parse_readiness_line("hogd-dev", &line).expect("bug: readiness should parse");
    readiness.network_hash = groundhog_json_rpc_types::NetworkHashHex::from([0xff; 32]);

    let err = validate_runtime_identity(&readiness, "hogp/test00000000000", &[0; 32].into())
        .expect_err("bug: a non-bootstrap network should fail closed");
    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeNetworkHashMismatch { expected, observed }
            if expected == groundhog_json_rpc_types::NetworkHashHex::from([0; 32])
                && observed == groundhog_json_rpc_types::NetworkHashHex::from([0xff; 32])
    ));
}

#[test]
fn configured_network_hash_reaches_bootstrap_and_readiness_validation() {
    for hash in [[0; 32], [0xab; 32]] {
        let network_hash = groundhog_json_rpc_types::NetworkHashHex::from(hash);
        let config = HogdDevConfig::default().with_init_network_hash(network_hash.clone());
        let file = write_bootstrap_manifest(&config, &network_hash, fixture_public_key(0)).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(value["networkHash"], network_hash.as_str());
        let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
        let mut readiness = parse_readiness_line("hogd-dev", &line).unwrap();
        readiness.network_hash = network_hash.clone();
        validate_runtime_identity(&readiness, "hogp/test00000000000", &network_hash).unwrap();
        assert!(matches!(
            validate_runtime_identity(&readiness, "hogp/test00000000000", &[0xff; 32].into()),
            Err(NodeSupervisorError::RuntimeNetworkHashMismatch { .. })
        ));
        assert!(matches!(
            validate_runtime_identity(&readiness, "hogp/expected0000000", &network_hash),
            Err(NodeSupervisorError::RuntimeProtocolMismatch { .. })
        ));
    }
}

#[test]
fn runtime_identity_rejects_protocol_mismatch() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let readiness = parse_readiness_line("hogd-dev", &line).expect("bug: readiness should parse");

    let err = validate_runtime_identity(&readiness, "hogp/expected0000000", &[0; 32].into())
        .expect_err("bug: a protocol mismatch should fail closed");
    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeProtocolMismatch { expected, observed }
            if expected == "hogp/expected0000000" && observed.as_deref() == Some("hogp/test00000000000")
    ));
}

#[test]
fn log_fields_extract_file_line_and_preserve_raw_fields() {
    let raw = serde_json::json!({
        "file": "rpc_from_coord.cc",
        "line": 1833,
        "stage": "commit",
    });

    let fields = super::parse_log_fields(Some(&raw));

    assert_eq!(fields.file.as_deref(), Some("rpc_from_coord.cc"));
    assert_eq!(fields.line, Some(1833));
    let round_trip: serde_json::Value =
        serde_json::from_str(&fields.raw).expect("bug: raw fields should stay JSON");
    assert_eq!(round_trip, raw);
}

#[test]
fn log_fields_tolerate_absent_metadata() {
    let fields = super::parse_log_fields(None);

    assert_eq!(fields.file, None);
    assert_eq!(fields.line, None);
    assert!(fields.raw.is_empty());
}

#[test]
fn forward_log_line_reports_malformed_json() {
    let outcome = super::forward_log_line("not json");

    assert_eq!(outcome, super::ForwardLogOutcome::InvalidJson);
}

#[test]
fn forward_log_line_tolerates_missing_level_and_message() {
    let outcome = super::forward_log_line(r#"{"target":"indexer"}"#);

    let super::ForwardLogOutcome::Forwarded(record) = outcome else {
        panic!("expected forwarded log record");
    };
    assert_eq!(record.level, super::ForwardedLogLevel::Missing);
    assert_eq!(record.timestamp_ms, None);
    assert_eq!(record.hogd_target, "indexer");
    assert!(record.message.is_empty());
    assert_eq!(record.fields, super::ForwardedLogFields::default());
}

#[test]
fn forward_log_line_tolerates_empty_fields() {
    let outcome = super::forward_log_line(
        r#"{"timestamp_ms":1770000000123,"level":"info","target":"hogdb","message":"ready","fields":{}}"#,
    );

    let super::ForwardLogOutcome::Forwarded(record) = outcome else {
        panic!("expected forwarded log record");
    };
    assert_eq!(record.timestamp_ms, Some(1_770_000_000_123));
    assert_eq!(record.level, super::ForwardedLogLevel::Info);
    assert_eq!(record.hogd_target, "hogdb");
    assert_eq!(record.message, "ready");
    assert_eq!(
        record.fields,
        super::ForwardedLogFields {
            raw: "{}".to_owned(),
            file: None,
            line: None,
        }
    );
}

#[tokio::test]
async fn read_capped_line_discards_oversized_line_tail() {
    let input = b"abcdef\n{\"level\":\"info\",\"message\":\"next\"}\n";
    let mut reader = tokio::io::BufReader::with_capacity(4, &input[..]);

    assert!(matches!(
        super::read_capped_line_discarding_tail(&mut reader, 3).await,
        Ok(super::CappedLine::TooLarge)
    ));

    let next = super::read_capped_line(&mut reader, 64)
        .await
        .expect("bug: next line should read");
    let super::CappedLine::Line(line) = next else {
        panic!("expected next physical line after oversized line");
    };
    assert_eq!(line, "{\"level\":\"info\",\"message\":\"next\"}\n");
}

#[test]
fn readiness_line_parses_endpoint_addresses() {
    let line = manifest_readiness_line(
        r#"{
              "scheduler":"127.0.0.1:12345",
              "schedulerGateway":"127.0.0.1:12349",
              "indexer":"127.0.0.1:12346",
              "dbGateway":"127.0.0.1:12347",
              "viewGateway":"127.0.0.1:12348"
            }"#,
        r"[]",
    );
    let pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let pointer_keys = pointer
        .as_object()
        .expect("bug: pointer should be an object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        sorted(pointer_keys),
        sorted(["manifestHash", "manifestPath", "schemaVersion", "status"])
    );

    let readiness =
        parse_readiness_line("hogd-dev", &line).expect("bug: readiness payload should parse");

    assert_eq!(readiness.status, "ready");
    assert_eq!(
        readiness.chain_state,
        Some(RuntimeManifestChainState::Fresh)
    );
    assert_eq!(readiness.network_hash.as_str(), "00".repeat(32));
    assert_eq!(readiness.protocol.as_deref(), Some("hogp/test00000000000"));
    assert_eq!(
        readiness.manifest_hash,
        pointer["manifestHash"].as_str().map(str::to_owned)
    );
    assert!(readiness.manifest_path.is_some());
    assert!(readiness.runtime_manifest.is_some());
    assert_eq!(
        readiness.endpoints.scheduler_coord,
        "127.0.0.1:12345"
            .parse()
            .expect("bug: address should parse")
    );
    assert_eq!(
        readiness.endpoints.scheduler_gateway,
        Some(
            "127.0.0.1:12349"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(
        readiness.endpoints.indexer_block_explorer,
        Some(
            "127.0.0.1:12346"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(readiness.endpoints.dev_control, None);
    assert_eq!(
        readiness.endpoints.db_gateway,
        Some(
            "127.0.0.1:12347"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(
        readiness.endpoints.view_gateway,
        Some(
            "127.0.0.1:12348"
                .parse()
                .expect("bug: address should parse")
        )
    );
}

#[test]
fn readiness_line_carries_recovered_chain_state() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
            .expect("bug: manifest should parse");
    manifest["chainState"] = serde_json::json!("recovered");
    let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
    fs::write(&path, &bytes).expect("bug: manifest should be writable");
    pointer["manifestHash"] = serde_json::json!(super::sha256_prefixed_hex(&bytes));

    let readiness = parse_readiness_line("hogd-dev", &pointer.to_string())
        .expect("bug: recovered readiness should parse");

    assert_eq!(
        readiness.chain_state,
        Some(RuntimeManifestChainState::Recovered)
    );
}

#[test]
fn manifest_readiness_requires_chain_state() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
            .expect("bug: manifest should parse");
    manifest
        .as_object_mut()
        .expect("bug: manifest is an object")
        .remove("chainState");
    let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
    fs::write(&path, &bytes).expect("bug: manifest should be writable");
    pointer["manifestHash"] = serde_json::json!(super::sha256_prefixed_hex(&bytes));

    let error = parse_readiness_line("hogd-dev", &pointer.to_string())
        .expect_err("manifest readiness without chainState should fail");

    assert!(matches!(
        error,
        NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
            if reason.contains("chainState")
    ));
}

#[test]
fn manifest_readiness_requires_scheduler_gateway() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
            .expect("bug: manifest should parse");
    manifest["endpoints"]
        .as_object_mut()
        .expect("bug: endpoints is an object")
        .remove("schedulerGateway");
    let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
    fs::write(&path, &bytes).expect("bug: manifest should be writable");
    pointer["manifestHash"] = serde_json::json!(super::sha256_prefixed_hex(&bytes));

    let error = parse_readiness_line("hogd-dev", &pointer.to_string())
        .expect_err("manifest readiness without schedulerGateway should fail");

    assert!(matches!(
        error,
        NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
            if reason.contains("schedulerGateway")
    ));
}

#[test]
fn manifest_readiness_requires_hogd_dev_runtime_kind() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("bug: manifest should be readable"))
            .expect("bug: manifest should parse");
    manifest["kind"] = serde_json::json!("hog-devnode-runtime");
    let bytes = serde_json::to_vec(&manifest).expect("bug: manifest should serialize");
    fs::write(&path, &bytes).expect("bug: manifest should be writable");
    pointer["manifestHash"] = serde_json::json!(super::sha256_prefixed_hex(&bytes));

    let error = parse_readiness_line("hog-devnode", &pointer.to_string())
        .expect_err("unexpected runtime manifest kind should fail");

    assert!(matches!(
        error,
        NodeSupervisorError::InvalidRuntimeManifest { reason, .. }
            if reason.contains("unsupported runtime manifest kind hog-devnode-runtime")
    ));
}

#[test]
fn readiness_line_accepts_scheduler_endpoints_without_optional_read_endpoints() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let readiness = parse_readiness_line("hogd-dev", &line)
        .expect("bug: minimal current readiness payload should parse");

    assert_eq!(readiness.status, "ready");
    assert_eq!(readiness.network_hash.as_str(), "00".repeat(32));
    assert_eq!(
        readiness.endpoints.scheduler_coord,
        "127.0.0.1:12345"
            .parse()
            .expect("bug: address should parse")
    );
    assert_eq!(readiness.endpoints.indexer_block_explorer, None);
    assert_eq!(
        readiness.endpoints.scheduler_gateway,
        Some(
            "127.0.0.1:12349"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(readiness.endpoints.db_gateway, None);
    assert_eq!(readiness.endpoints.view_gateway, None);
    assert_eq!(readiness.endpoints.dev_control, None);
}

#[test]
fn readiness_line_accepts_compact_endpoint_names() {
    let readiness = parse_readiness_line("hogd-dev", COMPACT_READY)
        .expect("bug: compact readiness payload should parse");

    assert_eq!(
        readiness.endpoints.scheduler_coord,
        "127.0.0.1:12345"
            .parse()
            .expect("bug: address should parse")
    );
    assert_eq!(
        readiness.endpoints.indexer_block_explorer,
        Some(
            "127.0.0.1:12346"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(
        readiness.endpoints.db_gateway,
        Some(
            "127.0.0.1:12347"
                .parse()
                .expect("bug: address should parse")
        )
    );
    assert_eq!(
        readiness.endpoints.view_gateway,
        Some(
            "127.0.0.1:12348"
                .parse()
                .expect("bug: address should parse")
        )
    );
}

#[test]
fn readiness_manifest_hash_mismatch_is_rejected() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    fs::write(&path, b"tampered").expect("bug: manifest should be writable");
    pointer["manifestHash"] = serde_json::Value::String(super::sha256_prefixed_hex(b"original"));
    let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

    let err = parse_readiness_line("hogd-dev", &line)
        .expect_err("bug: manifest hash mismatch should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeManifestHashMismatch { .. }
    ));
}

#[test]
fn readiness_manifest_hash_accepts_uppercase_hex_digits() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let hash = pointer["manifestHash"]
        .as_str()
        .expect("bug: pointer has manifest hash")
        .to_owned();
    pointer["manifestHash"] = serde_json::Value::String(format!("0x{}", hash[2..].to_uppercase()));
    let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

    let readiness =
        parse_readiness_line("hogd-dev", &line).expect("bug: uppercase manifest hash should parse");

    assert_eq!(readiness.manifest_hash, Some(hash));
}

#[test]
fn readiness_manifest_rejects_group_or_world_permissions() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("bug: permissions should be set");

    let err = parse_readiness_line("hogd-dev", &line)
        .expect_err("bug: unsafe manifest permissions should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
    ));
}

#[test]
fn readiness_manifest_rejects_symlink_path() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let path = PathBuf::from(
        pointer["manifestPath"]
            .as_str()
            .expect("bug: pointer has manifest path"),
    );
    let symlink_path = path.with_file_name("runtime-manifest-link.json");
    std::os::unix::fs::symlink(&path, &symlink_path).expect("bug: symlink should be created");
    pointer["manifestPath"] = serde_json::Value::String(symlink_path.display().to_string());
    let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

    let err = parse_readiness_line("hogd-dev", &line)
        .expect_err("bug: symlink manifest path should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
    ));
}

#[test]
fn readiness_manifest_rejects_relative_path() {
    let line = manifest_readiness_line(MANIFEST_SCHEDULER_ENDPOINTS, r"[]");
    let mut pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    pointer["manifestPath"] = serde_json::Value::String("runtime-manifest.json".into());
    let line = serde_json::to_string(&pointer).expect("bug: pointer serializes");

    let err = parse_readiness_line("hogd-dev", &line)
        .expect_err("bug: relative manifest path should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::RuntimeManifestUnsafePermissions { .. }
    ));
}

#[test]
fn manifest_pointer_fields_do_not_fall_back_to_legacy_readiness() {
    let line = format!(
        r#"{{"schemaVersion":3,"status":"ready","manifestPath":"/tmp/missing","manifestHash":"{}","network_id":"groundhog-local","endpoints":{{"scheduler_coord":"127.0.0.1:12345"}}}}"#,
        super::sha256_prefixed_hex(b"missing"),
    );

    let err = parse_readiness_line("hogd-dev", &line)
        .expect_err("bug: mixed pointer and legacy readiness should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::InvalidReadinessJson { .. }
    ));
}

#[test]
fn manifest_pointer_rejects_future_schema_before_decoding_body() {
    let err = parse_readiness_line(
        "hogd-dev",
        r#"{"schemaVersion":99,"manifestPath":42,"futureBody":true}"#,
    )
    .expect_err("bug: future schema should fail before pointer body decoding");

    assert!(matches!(
        err,
        NodeSupervisorError::InvalidReadinessSchemaVersion {
            expected: 3,
            actual: 99,
            ..
        }
    ));
}

#[test]
fn full_runtime_manifest_uses_canonical_endpoint_names() {
    let line = manifest_readiness_line(
        r#"{
              "scheduler":"127.0.0.1:12345",
              "schedulerGateway":"127.0.0.1:12349",
              "indexer":"127.0.0.1:12346",
              "dbGateway":"127.0.0.1:12347",
              "viewGateway":"127.0.0.1:12348"
            }"#,
        r"[]",
    );
    let pointer: serde_json::Value =
        serde_json::from_str(&line).expect("bug: pointer line should parse");
    let manifest_path = pointer["manifestPath"]
        .as_str()
        .expect("bug: pointer has manifest path");
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(manifest_path).expect("bug: manifest should be readable"))
            .expect("bug: manifest should parse");
    let endpoint_keys = manifest["endpoints"]
        .as_object()
        .expect("bug: endpoints should be object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(
        sorted(endpoint_keys),
        sorted([
            "dbGateway",
            "indexer",
            "scheduler",
            "schedulerGateway",
            "viewGateway"
        ])
    );
}

fn fixture_public_key(index: u8) -> groundhog_json_rpc_types::Ed25519PublicKey {
    groundhog_json_rpc_types::hex::Hex0x(groundhog_xdr::types::Ed25519PublicKey::from(
        groundhog_signing::LocalSigner::from_secret_key([index; 32]).verifying_key_bytes(),
    ))
}

#[test]
fn explicit_allocations_validate_native_identity_roles_and_amounts() {
    let config = HogdDevConfig::default().with_bootstrap_accounts(vec![
        groundhog_json_rpc_types::bootstrap_manifest::BootstrapManifestAllocation::new(
            fixture_public_key(1),
            "42",
        ),
    ]);
    let allocations = config.to_bootstrap_manifest([0; 32].into(), fixture_public_key(0));
    let accounts: Vec<_> = std::iter::once(&allocations.bootstrap_faucet).chain(allocations.bootstrap_accounts.iter())
            .enumerate().map(|(position, allocation)| {
                let key: [u8;32] = allocation.public_key.as_ref().try_into().unwrap();
                serde_json::json!({
                    "role": if position == 0 { "faucet" } else { "local_user" },
                    "publicKey": allocation.public_key,
                    "walletAddress": groundhog_json_rpc_types::hex::Hex0x(groundhog_signing::PubkeyAccount::from_public_key_bytes(key, 0).address()),
                    "initialStorageTokens": allocation.initial_storage_tokens,
                })
            }).collect();
    let line = manifest_readiness_line(
        MANIFEST_SCHEDULER_ENDPOINTS,
        &serde_json::to_string(&accounts).unwrap(),
    );
    let readiness = parse_readiness_line("hogd-dev", &line).unwrap();
    validate_bootstrap_accounts(&readiness, &config, &[0; 32].into(), fixture_public_key(0))
        .unwrap();
    for recovered in [false, true] {
        for field in [
            "role",
            "publicKey",
            "walletAddress",
            "initialStorageTokens",
            "count",
        ] {
            let mut invalid = readiness.clone();
            if recovered {
                invalid.chain_state = Some(RuntimeManifestChainState::Recovered);
            }
            let manifest = invalid.runtime_manifest.as_mut().unwrap();
            match field {
                "role" => manifest.bootstrap_accounts[0].role = "local_user".into(),
                "publicKey" => {
                    manifest.bootstrap_accounts[0].public_key = fixture_public_key(2);
                }
                "walletAddress" => {
                    manifest.bootstrap_accounts[0].wallet_address =
                        groundhog_xdr::types::ContractAddress::from([0; 32]).into();
                }
                "initialStorageTokens" => {
                    manifest.bootstrap_accounts[0].initial_storage_tokens = "999".into();
                }
                _ => {
                    manifest.bootstrap_accounts.pop();
                }
            }
            assert!(
                validate_bootstrap_accounts(
                    &invalid,
                    &config,
                    &[0; 32].into(),
                    fixture_public_key(0)
                )
                .is_err(),
                "{field}"
            );
        }
    }
    assert_eq!(readiness.dev_accounts.len(), 2);
}

#[test]
fn legacy_readiness_without_manifest_is_rejected_by_supervisor_validation() {
    let readiness = parse_readiness_line("hogd-dev", V1_READY).unwrap();
    assert!(
        validate_bootstrap_accounts(
            &readiness,
            &HogdDevConfig::default(),
            &[0; 32].into(),
            fixture_public_key(0)
        )
        .is_err()
    );
}

#[test]
fn non_ready_status_is_rejected() {
    let err = parse_readiness_line(
            "hogd-dev",
            r#"{"status":"starting","network_hash":"0000000000000000000000000000000000000000000000000000000000000000","endpoints":{"scheduler_coord":"127.0.0.1:12345","indexer_block_explorer":"127.0.0.1:12346","dev_control":"127.0.0.1:12347"}}"#,
        )
        .expect_err("bug: non-ready status should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::InvalidReadinessStatus { status, .. } if status == "starting"
    ));
}

#[test]
fn configured_binary_must_exist() {
    let config = HogdDevConfig::default().with_binary_path(missing_path());
    let err = resolve_hogd_dev_binary(&config).expect_err("bug: missing path should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::ConfiguredPathMissing { .. }
    ));
}

#[test]
fn configured_binary_must_be_executable() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let path = tmp.path().join(DEFAULT_HOGD_DEV_BINARY);
    fs::write(&path, b"").expect("bug: file should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("bug: permissions should be set");
    let config = HogdDevConfig::default().with_binary_path(path);
    let err = resolve_hogd_dev_binary(&config).expect_err("bug: non-executable path should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::ConfiguredPathNotExecutable { .. }
    ));
}

#[test]
fn explicit_binary_path_resolves() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
    let config = HogdDevConfig::default().with_binary_path(path.clone());

    assert_eq!(
        resolve_hogd_dev_binary(&config).expect("bug: path should resolve"),
        path
    );
}

#[test]
fn persistent_data_dir_is_private() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let data_dir = tmp.path().join("persistent");
    fs::create_dir(&data_dir).expect("bug: data dir should be created");
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755))
        .expect("bug: fixture permissions should be set");
    let config = HogdDevConfig::default().with_data_dir(&data_dir);

    prepare_persistent_data_dir(&config).expect("bug: data dir should be prepared");

    assert_eq!(
        fs::metadata(data_dir)
            .expect("bug: data dir metadata should be readable")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn persistent_data_dir_rejects_symlink() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let target = tmp.path().join("target");
    let data_dir = tmp.path().join("persistent");
    fs::create_dir(&target).expect("bug: target dir should be created");
    symlink(&target, &data_dir).expect("bug: data dir symlink should be created");
    let config = HogdDevConfig::default().with_data_dir(data_dir);

    let err = prepare_persistent_data_dir(&config)
        .expect_err("bug: symlinked data dir should fail closed");
    assert!(matches!(err, NodeSupervisorError::DataDir { .. }));
}

#[tokio::test]
async fn persistent_start_uses_data_dir_pid_file() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let data_dir = temp.path().join("persistent");
    let pid_file = data_dir.join(PERSISTENT_PID_FILE_NAME);
    let config = HogdDevConfig::default()
        .with_data_dir(&data_dir)
        .with_startup_timeout(Duration::from_secs(2));

    let mut process = start_fake_hogd_dev(&config, "ready")
        .await
        .expect("bug: persistent startup should create its canonical PID file");
    assert!(pid_file.exists());
    process.stop().await.expect("bug: fake child should stop");
    assert!(!pid_file.exists());
}

#[tokio::test]
async fn persistent_data_dir_cannot_use_multiple_explicit_pid_files() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let data_dir = temp.path().join("persistent");
    let first_config = HogdDevConfig::default()
        .with_data_dir(&data_dir)
        .with_pid_file(temp.path().join("first.pid"))
        .with_startup_timeout(Duration::from_secs(2));
    let second_config = HogdDevConfig::default()
        .with_data_dir(&data_dir)
        .with_pid_file(temp.path().join("second.pid"))
        .with_startup_timeout(Duration::from_secs(2));
    let mut process = start_fake_hogd_dev(&first_config, "ready")
        .await
        .expect("bug: first persistent child should start");

    let err = start_fake_hogd_dev(&second_config, "ready")
        .await
        .expect_err("bug: the data-directory PID guard should prevent a second child");
    assert!(matches!(
        err,
        NodeSupervisorError::LocalNodeAlreadyRunning { path, .. }
            if path == data_dir.join(PERSISTENT_PID_FILE_NAME)
    ));

    process.stop().await.expect("bug: fake child should stop");
}

#[tokio::test]
async fn fake_child_reports_readiness_and_stops_cleanly() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let marker = temp.path().join("graceful-shutdown");
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(500));

    let mut process = start_fake_hogd_dev_with_env(
        &config,
        "ready",
        GRACEFUL_SHUTDOWN_FILE_ENV,
        marker.as_os_str(),
    )
    .await
    .expect("bug: fake hogd-dev should start");

    assert_eq!(process.readiness().network_hash.as_str(), "00".repeat(32));
    assert!(process.readiness().manifest_path.is_some());
    assert!(process.readiness().manifest_hash.is_some());
    assert!(process.readiness().runtime_manifest.is_some());
    assert_ne!(
        process.readiness().endpoints.scheduler_coord,
        process
            .readiness()
            .endpoints
            .scheduler_gateway
            .expect("bug: fake readiness should advertise schedulerGateway")
    );

    let status = process.stop().await.expect("bug: fake child should stop");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(marker).expect("bug: graceful marker should be written"),
        "graceful\n"
    );
}

#[tokio::test]
async fn start_with_pid_file_writes_and_cleans_up() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let pid_file = temp.path().join("nested").join("hogd-dev.pid");
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(500))
        .with_pid_file(pid_file.clone());

    let mut process = start_fake_hogd_dev(&config, "ready")
        .await
        .expect("bug: fake hogd-dev should start");
    let child_id = process
        .child_id()
        .expect("bug: fake hogd-dev pid should be available");

    assert_eq!(
        fs::read_to_string(&pid_file).expect("bug: PID file should be readable"),
        format!("{child_id}\n")
    );

    let status = process.stop().await.expect("bug: fake child should stop");
    assert!(status.success());
    assert!(!pid_file.exists());
}

#[tokio::test]
async fn fake_child_ignoring_sigterm_is_killed_after_grace() {
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(100));

    let mut process = start_fake_hogd_dev(&config, "ignore-term")
        .await
        .expect("bug: fake hogd-dev should start");

    let status = process
        .stop()
        .await
        .expect("bug: SIGKILL escalation should return child status");
    assert!(!status.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }
}

#[tokio::test]
async fn fake_child_helper_is_killed_after_leader_exits() {
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(100));

    let mut process = start_fake_hogd_dev(&config, "spawn-helper-ignore-term")
        .await
        .expect("bug: fake hogd-dev should start");
    let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
        .expect("bug: child pid should fit in pid_t");

    let status = process.stop().await.expect("bug: fake child should stop");
    assert!(status.success());
    assert_process_group_exits(process_group).await;
}

#[tokio::test]
async fn unexpected_leader_exit_cleans_descendants_before_releasing_pid_file() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let pid_file = temp.path().join("hog-devnode.pid");
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(100))
        .with_pid_file(pid_file.clone());
    let mut process = start_fake_hogd_dev(&config, "spawn-helper-then-exit")
        .await
        .expect("bug: fake hogd-dev should start");
    let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
        .expect("bug: child pid should fit in pid_t");

    let status = process
        .stopped()
        .await
        .expect("bug: unexpected leader exit cleanup should complete");

    assert!(status.success());
    assert!(!pid_file.exists());
    assert_process_group_exits(process_group).await;
}

#[tokio::test]
async fn cancelled_shutdown_preserves_pid_file_for_recovery() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let pid_file = temp.path().join("hog-devnode.pid");
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_secs(1))
        .with_pid_file(pid_file.clone());
    let mut process = start_fake_hogd_dev(&config, "ignore-term")
        .await
        .expect("bug: fake hogd-dev should start");
    let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
        .expect("bug: child pid should fit in pid_t");

    let result = tokio::time::timeout(Duration::from_millis(20), process.stop()).await;
    assert!(
        result.is_err(),
        "shutdown should still be in its grace period"
    );
    drop(process);

    assert!(pid_file.exists());
    assert_process_group_exits(process_group).await;
    fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
}

#[tokio::test]
async fn dropping_persistent_process_sends_sigterm_and_preserves_pid_file() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let marker = temp.path().join("graceful-shutdown");
    let pid_file = temp.path().join("hog-devnode.pid");
    let config = HogdDevConfig::default()
        .with_data_dir(temp.path().join("data"))
        .with_startup_timeout(Duration::from_secs(2))
        .with_pid_file(pid_file.clone());
    let process = start_fake_hogd_dev_with_env(
        &config,
        "ready",
        GRACEFUL_SHUTDOWN_FILE_ENV,
        marker.as_os_str(),
    )
    .await
    .expect("bug: persistent fake hogd-dev should start");
    let process_group = i32::try_from(process.child_id().expect("bug: child should have pid"))
        .expect("bug: child pid should fit in pid_t");

    drop(process);

    assert_file_contents(&marker, b"graceful\n").await;
    assert_process_group_exits(process_group).await;
    assert!(pid_file.exists());
    fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
}

#[tokio::test]
async fn fake_child_exit_before_readiness_is_typed() {
    let config = HogdDevConfig::default().with_startup_timeout(Duration::from_secs(2));

    let err = start_fake_hogd_dev(&config, "exit-before-ready")
        .await
        .expect_err("bug: early child exit should fail");

    assert!(matches!(
        err,
        NodeSupervisorError::ExitedBeforeReady { status, .. } if status.code() == Some(9)
    ));
}

#[tokio::test]
async fn startup_timeout_kills_fake_child() {
    let config = HogdDevConfig::default().with_startup_timeout(Duration::from_millis(100));

    let err = start_fake_hogd_dev(&config, "timeout")
        .await
        .expect_err("bug: startup should time out");

    assert!(
        matches!(err, NodeSupervisorError::ReadinessTimeout { .. }),
        "unexpected startup error: {err:?}"
    );
}

#[tokio::test]
async fn unready_cleanup_sends_sigterm_before_escalating_to_sigkill() {
    let temp = TempDir::new().expect("bug: temp dir should be created");
    let marker = temp.path().join("sigterm-observed");
    let pid_file = temp.path().join("hog-devnode.pid");
    // The fake records SIGTERM only after its handler is installed, so the
    // startup timeout must outlast cold-start exec latency under a loaded
    // parallel test run; a shorter timeout lets the readiness-timeout
    // SIGTERM kill the fake before it can arm the handler.
    let config = HogdDevConfig::default()
        .with_data_dir(temp.path().join("data"))
        .with_pid_file(pid_file.clone())
        .with_startup_timeout(Duration::from_secs(2));

    let error = start_fake_hogd_dev_with_env(
        &config,
        "timeout-record-term-ignore",
        GRACEFUL_SHUTDOWN_FILE_ENV,
        marker.as_os_str(),
    )
    .await
    .expect_err("bug: unready fake should time out and be killed");
    assert!(
        matches!(&error, NodeSupervisorError::ReadinessTimeout { .. }),
        "unexpected startup error: {error:?}"
    );
    assert_eq!(
        fs::read_to_string(marker).expect("bug: SIGTERM marker should be readable"),
        "graceful\n"
    );
    let process_group = fs::read_to_string(&pid_file)
        .expect("bug: recovery PID should remain")
        .trim()
        .parse::<i32>()
        .expect("bug: recovery PID should parse");
    assert_process_group_exits(process_group).await;
    fs::remove_file(pid_file).expect("bug: test should remove recovery PID file");
}

#[tokio::test]
async fn oversized_readiness_line_is_rejected() {
    let config = HogdDevConfig::default().with_startup_timeout(Duration::from_secs(2));

    let err = start_fake_hogd_dev(&config, "oversized-readiness")
        .await
        .expect_err("bug: oversized readiness should fail");

    assert!(
        matches!(err, NodeSupervisorError::ReadinessTooLarge { .. }),
        "unexpected oversized readiness error: {err:?}"
    );
}

#[tokio::test]
async fn inherited_lease_survives_leader_exit_until_descendant_is_killed() {
    use std::os::fd::AsRawFd;
    let root = TempDir::new().unwrap();
    let lock_path = root.path().join("lease.lock");
    let inherited = fs::File::create(&lock_path).unwrap();
    // SAFETY: both flock calls operate on live, independently opened fixture files.
    assert_eq!(
        unsafe { libc::flock(inherited.as_raw_fd(), libc::LOCK_SH) },
        0
    );
    let observer = fs::File::open(&lock_path).unwrap();
    let config = HogdDevConfig::default()
        .with_startup_timeout(Duration::from_secs(2))
        .with_shutdown_grace(Duration::from_millis(150));
    let bootstrap =
        write_bootstrap_manifest(&config, &[0; 32].into(), fixture_public_key(0)).unwrap();
    let spec = build_hogd_dev_spec_with_bootstrap_manifest(
        fake_hogd_dev_binary(),
        &config,
        bootstrap.path(),
    )
    .env(FAKE_HOGD_DEV_MODE_ENV, "spawn-helper-then-exit");
    let mut process =
        HogdDevProcess::start_with_spec(&config, spec, Some(bootstrap), Some(inherited), None)
            .await
            .unwrap();
    // Reap the leader without invoking process-group cleanup yet.
    assert!(process.child.wait().await.unwrap().success());
    // SAFETY: observer remains open for this test.
    assert_eq!(
        unsafe { libc::flock(observer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EWOULDBLOCK)
    );
    let started = std::time::Instant::now();
    process.stop().await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "helper must survive SIGTERM until the SIGKILL deadline"
    );
    // SAFETY: observer remains open, and all inheriting processes have exited.
    assert_eq!(
        unsafe { libc::flock(observer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}

async fn start_fake_hogd_dev(
    config: &HogdDevConfig,
    mode: &str,
) -> Result<HogdDevProcess, NodeSupervisorError> {
    let bootstrap_manifest =
        write_bootstrap_manifest(config, &[0; 32].into(), fixture_public_key(0))
            .expect("bug: bootstrap manifest file should be written");
    let spec = build_hogd_dev_spec_with_bootstrap_manifest(
        fake_hogd_dev_binary(),
        config,
        bootstrap_manifest.path(),
    )
    .env(FAKE_HOGD_DEV_MODE_ENV, mode);
    HogdDevProcess::start_with_spec(config, spec, Some(bootstrap_manifest), None, None).await
}

async fn start_fake_hogd_dev_with_env(
    config: &HogdDevConfig,
    mode: &str,
    key: &str,
    value: &std::ffi::OsStr,
) -> Result<HogdDevProcess, NodeSupervisorError> {
    let bootstrap_manifest =
        write_bootstrap_manifest(config, &[0; 32].into(), fixture_public_key(0))
            .expect("bug: bootstrap manifest file should be written");
    let spec = build_hogd_dev_spec_with_bootstrap_manifest(
        fake_hogd_dev_binary(),
        config,
        bootstrap_manifest.path(),
    )
    .env(FAKE_HOGD_DEV_MODE_ENV, mode)
    .env(key, value);
    HogdDevProcess::start_with_spec(config, spec, Some(bootstrap_manifest), None, None).await
}

async fn assert_process_group_exits(process_group: i32) {
    for _ in 0..50 {
        if !process_group_exists(process_group) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("bug: process group {process_group} should exit");
}

async fn assert_file_contents(path: &Path, expected: &[u8]) {
    for _ in 0..100 {
        if fs::read(path).is_ok_and(|contents| contents == expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "bug: expected file contents at {}: {:?}",
        path.display(),
        String::from_utf8_lossy(expected)
    );
}

fn process_group_exists(process_group: i32) -> bool {
    crate::process::process_group_has_live_member(process_group).unwrap_or(false)
}

fn executable_file(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"").expect("bug: executable file should be written");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("bug: permissions should be set");
    path
}

fn missing_path() -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("bug: system time should be after epoch")
        .as_nanos();
    env::temp_dir().join(format!("missing-hog-devnode-{unique}"))
}

fn manifest_readiness_line(endpoints: &str, accounts: &str) -> String {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let manifest_dir = tmp.keep();
    fs::set_permissions(&manifest_dir, fs::Permissions::from_mode(0o700))
        .expect("bug: manifest dir permissions should be set");
    let manifest_path = manifest_dir.join("runtime-manifest.json");
    let manifest = format!(
        r#"{{
              "schemaVersion":4,
              "kind":"hogd-dev-runtime",
              "chainState":"fresh",
              "networkHash":"0000000000000000000000000000000000000000000000000000000000000000",
              "protocol":"hogp/test00000000000",
              "producer":{{"binary":"hog-devnode","version":"0.1.0","commitHash":"abcdef0"}},
              "endpoints":{endpoints},
              "bootstrapAccounts":{accounts},
              "bootstrap":{{
                "storageCostModel":{{
                  "hash":"0x{}",
                  "baseDataItemCost":"12000",
                  "bytestringCostPerByte":"10",
                  "baseContractDeploymentCost":"12000",
                  "contractDeploymentCostPerByte":"10",
                  "authPerKeyCost":"1111",
                  "rcqCostPerEntry":"8",
                  "replayCacheCostPerEntry":"7",
                  "balanceCost":"1234"
                }}
              }}
            }}"#,
        "51".repeat(32),
    );
    fs::write(&manifest_path, manifest.as_bytes()).expect("bug: manifest should be written");
    fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600))
        .expect("bug: manifest permissions should be set");
    format!(
        r#"{{"schemaVersion":3,"status":"ready","manifestPath":"{}","manifestHash":"{}"}}"#,
        manifest_path.display(),
        super::sha256_prefixed_hex(manifest.as_bytes()),
    )
}

fn sorted<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut values = values.into_iter().map(Into::into).collect::<Vec<_>>();
    values.sort();
    values
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        unsafe { env::remove_var(key) };
        Self { key, previous }
    }

    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = env::var_os(key);
        unsafe { env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { env::set_var(self.key, value) },
            None => unsafe { env::remove_var(self.key) },
        }
    }
}

struct FileGuard {
    path: PathBuf,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[test]
#[serial]
fn env_binary_path_resolves() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
    let _guard = EnvGuard::set(DEVNODE_BINARY_ENV, &path);

    assert_eq!(
        resolve_hogd_dev_binary(&HogdDevConfig::default()).expect("bug: env path should resolve"),
        path
    );
}

#[test]
#[serial]
fn path_binary_resolves() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
    let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
    let _path_guard = EnvGuard::set("PATH", tmp.path());

    assert_eq!(
        resolve_hogd_dev_binary(&HogdDevConfig::default())
            .expect("bug: PATH binary should resolve"),
        path
    );
}

#[test]
#[serial]
fn pre_path_resolution_leaves_path_for_managed_orchestration() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let path = executable_file(tmp.path(), DEFAULT_HOGD_DEV_BINARY);
    let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
    let _path_guard = EnvGuard::set("PATH", tmp.path());

    assert_eq!(
        resolve_hogd_dev_binary_before_path(&HogdDevConfig::default())
            .expect("bug: local resolution should succeed"),
        None
    );
    assert_eq!(resolve_hogd_dev_binary_on_path(), Some(path));
}

#[test]
#[serial]
fn sibling_binary_path_resolves() {
    let current_exe = env::current_exe().expect("bug: current exe should resolve");
    let sibling_dir = current_exe
        .parent()
        .expect("bug: current exe should have a parent");
    let sibling = sibling_dir.join(DEFAULT_HOGD_DEV_BINARY);
    assert!(
        !sibling.exists(),
        "test would clobber existing sibling hog-devnode at {}",
        sibling.display()
    );
    fs::write(&sibling, b"").expect("bug: sibling file should be written");
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o755))
        .expect("bug: sibling permissions should be set");
    let _file_guard = FileGuard {
        path: sibling.clone(),
    };
    let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
    let empty_path = TempDir::new().expect("bug: temp dir should be created");
    let _path_guard = EnvGuard::set("PATH", empty_path.path());

    assert_eq!(
        resolve_hogd_dev_binary(&HogdDevConfig::default())
            .expect("bug: sibling binary should resolve"),
        sibling
    );
}

#[test]
#[serial]
fn missing_binary_returns_typed_error() {
    let tmp = TempDir::new().expect("bug: temp dir should be created");
    let _new_env_guard = EnvGuard::remove(DEVNODE_BINARY_ENV);
    let _path_guard = EnvGuard::set("PATH", tmp.path());

    let err = resolve_hogd_dev_binary(&HogdDevConfig::default())
        .expect_err("bug: missing hog-devnode should fail");

    assert!(matches!(err, NodeSupervisorError::HogdDevNotFound));
}
