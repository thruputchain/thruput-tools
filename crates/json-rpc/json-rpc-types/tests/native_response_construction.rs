//! Integration tests compile as a separate crate, so these exercise the public
//! construction API of the non-exhaustive native discovery responses.

use serde_json::json;
use thruput_json_rpc_types::responses::{
    ChainConfig, ChainTip, ConfigLimits, ExecutionConfig, ServerLimits, SimulationBatchLimits,
    StorageCosts, VersionInfo,
};

#[test]
fn construct_version_and_attach_diagnostics() {
    let mut version = VersionInfo::new("14.0.0", vec!["getVersionInfo".into()], "test");
    assert_eq!(
        serde_json::to_value(&version).unwrap(),
        json!({"wireFormat": "14.0.0", "supportedMethods": ["getVersionInfo"],
            "implementation": "test"})
    );

    version.commit_hash = Some("commit".into());
    version.platform_version = Some("platform".into());
    version.release_version = Some("release".into());
    version.bundle_manifest_digest = Some("digest".into());
    assert_eq!(
        serde_json::to_value(&version).unwrap(),
        json!({"wireFormat": "14.0.0", "supportedMethods": ["getVersionInfo"],
            "implementation": "test", "commitHash": "commit", "platformVersion": "platform",
            "releaseVersion": "release", "bundleManifestDigest": "digest"})
    );
}

#[test]
fn construct_config_and_attach_execution() {
    let stated = ConfigLimits {
        max_sha256_buffers: Some(8),
        ..ConfigLimits::default()
    };
    let mut config = ChainConfig::new(stated);
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        json!({"stated": {"maxSha256Buffers": 8}})
    );

    let execution = ExecutionConfig {
        observed_tip: ChainTip {
            block_height: 42.into(),
            block_hash: thruput_xdr::types::SHA256Hash::from([1; 32]).into(),
        },
        config_hash: thruput_xdr::types::SHA256Hash::from([2; 32]).into(),
        effective_limits: config.stated.clone(),
        storage_costs: StorageCosts {
            base_data_item_cost: 1,
            bytestring_cost_per_byte: 2,
            base_contract_deployment_cost: 3,
            contract_deployment_cost_per_byte: 4,
            auth_per_key_cost: 5,
            rcq_cost_per_entry: 6,
            replay_cache_cost_per_entry: 7,
            balance_cost: 8,
        },
    };
    let expected = serde_json::to_value(&execution).unwrap();
    config.execution = Some(execution);
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        json!({"stated": {"maxSha256Buffers": 8}, "execution": expected})
    );
}

#[test]
fn construct_limits_and_attach_batch() {
    let mut limits = ServerLimits::new(1_000_000, 4096, 100, 200, 1024);
    let mut expected = json!({
        "maxSimulationGas": "1000000", "maxSimulationCalldataBytes": "4096",
        "maxEventsPerPage": 100, "maxTransactionsPerPage": 200, "maxCursorBytes": "1024"
    });
    assert_eq!(serde_json::to_value(&limits).unwrap(), expected);

    limits.simulation_batch = Some(SimulationBatchLimits {
        max_invocations: 10,
        max_calldata_bytes: 8192,
        max_gas: 2_000_000,
        max_backend_response_bytes: 16384,
        timeout_ms: 5000,
    });
    expected["simulationBatch"] = json!({
        "maxInvocations": 10, "maxCalldataBytes": 8192, "maxGas": "2000000",
        "maxBackendResponseBytes": 16384, "timeoutMs": "5000"
    });
    assert_eq!(serde_json::to_value(&limits).unwrap(), expected);
}
