use std::path::PathBuf;

use serde_json::json;
use thruput_json_rpc_types::NetworkHashHex;

use thruput_node_supervisor::config::{BlockDriverConfig, BlockDriverMode, HogdDevConfig};

#[test]
fn data_dir_is_absolute_and_expands_home() {
    let relative = HogdDevConfig::default().with_data_dir("groundhog-test-data");
    let relative = relative
        .data_dir
        .expect("bug: data dir should be configured");
    assert!(relative.is_absolute());
    assert!(relative.ends_with("groundhog-test-data"));

    if let Some(home) = std::env::var_os("HOME") {
        let expanded = HogdDevConfig::default().with_data_dir("~/groundhog-test-data");
        assert_eq!(
            expanded.data_dir,
            Some(PathBuf::from(home).join("groundhog-test-data"))
        );
    }
}

#[test]
fn contract_artifact_directories_are_absolute_and_expand_home() {
    let relative =
        HogdDevConfig::default().with_contract_artifact_directories(["groundhog-test-contracts"]);
    assert!(relative.contract_artifact_directories[0].is_absolute());
    assert!(relative.contract_artifact_directories[0].ends_with("groundhog-test-contracts"));

    if let Some(home) = std::env::var_os("HOME") {
        let expanded = HogdDevConfig::default()
            .with_contract_artifact_directories(["~/groundhog-test-contracts"]);
        assert_eq!(
            expanded.contract_artifact_directories,
            [PathBuf::from(home).join("groundhog-test-contracts")]
        );
    }
}

#[test]
fn block_driver_default_keeps_instant_mode_with_500ms_interval() {
    let config = BlockDriverConfig::default();

    assert_eq!(config.mode, BlockDriverMode::Instant);
    assert_eq!(config.interval, std::time::Duration::from_millis(500));
    assert_eq!(
        config.manual_request_timeout,
        std::time::Duration::from_secs(30)
    );
}

#[test]
fn rpc_batch_concurrency_defaults_and_builder_are_validated() {
    assert_eq!(
        thruput_node_supervisor::config::NodeSupervisorConfig::default()
            .rpc_batch_concurrency
            .get(),
        128
    );
    for value in [0, 1025] {
        assert!(thruput_node_supervisor::RpcBatchConcurrency::try_from(value).is_err());
    }
    for value in [1, 1024] {
        let config = thruput_node_supervisor::config::NodeSupervisorConfig::default()
            .with_rpc_batch_concurrency(value.try_into().unwrap());
        assert_eq!(config.rpc_batch_concurrency.get(), value);
    }
}

#[test]
fn manual_block_driver_keeps_default_interval_inert() {
    let config = BlockDriverConfig::manual();

    assert_eq!(config.mode, BlockDriverMode::Manual);
    assert_eq!(config.interval, std::time::Duration::from_millis(500));
    assert_eq!(
        config.manual_request_timeout,
        std::time::Duration::from_secs(30)
    );
}

#[test]
fn hogd_dev_config_lowers_to_public_allocations() {
    use thruput_json_rpc_types::{bootstrap_manifest::BootstrapManifestAllocation, hex::Hex0x};
    let key = Hex0x(thruput_xdr::types::Ed25519PublicKey::from([0x42; 32]));
    let config = HogdDevConfig::default()
        .with_bootstrap_accounts(vec![BootstrapManifestAllocation::new(key, "42")]);
    let value =
        serde_json::to_value(config.to_bootstrap_manifest(NetworkHashHex::from([0x51; 32]), key))
            .unwrap();
    assert_eq!(value["schemaVersion"], 4);
    assert_eq!(
        value["bootstrapAccounts"],
        json!([{ "publicKey": key, "initialStorageTokens": "42" }])
    );
    assert_eq!(
        value["bootstrapFaucet"]["publicKey"],
        serde_json::to_value(key).unwrap()
    );
    assert!(HogdDevConfig::default().bootstrap_accounts.is_empty());
    assert_eq!(
        thruput_node_supervisor::config::DEFAULT_LOCAL_ACCOUNT_TOKENS,
        100 * 10_000_000
    );
}
