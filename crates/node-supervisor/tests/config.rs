use std::path::PathBuf;

use groundhog_json_rpc_types::NetworkHashHex;
use serde_json::json;

use groundhog_node_supervisor::config::{BlockDriverConfig, BlockDriverMode, HogdDevConfig};

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
        groundhog_node_supervisor::config::NodeSupervisorConfig::default()
            .rpc_batch_concurrency
            .get(),
        128
    );
    for value in [0, 1025] {
        assert!(groundhog_node_supervisor::RpcBatchConcurrency::try_from(value).is_err());
    }
    for value in [1, 1024] {
        let config = groundhog_node_supervisor::config::NodeSupervisorConfig::default()
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
fn hogd_dev_config_lowers_to_bootstrap_manifest() {
    let config = HogdDevConfig::default()
        .with_dev_account_count(3)
        .with_dev_account_seed("bench-seed")
        .with_dev_account_tokens(42);

    let manifest = config
        .to_bootstrap_manifest(NetworkHashHex::from([0x51; 32]))
        .expect("bug: account count fits u32");

    let value = serde_json::to_value(manifest).expect("bug: manifest serializes");

    assert_eq!(
        value,
        json!({
            "schemaVersion": 3,
            "kind": "groundhog-bootstrap",
            "networkHash": "5151515151515151515151515151515151515151515151515151515151515151",
            "profile": "local-dev",
            "bootstrapFaucet": {
                "index": 0,
                "initialStorageTokens": "10000000000000",
            },
            "bootstrapAccounts": {
                "count": 3,
                "firstIndex": 1,
                "seed": "bench-seed",
                "initialStorageTokens": "42",
            },
            "storageCostModel": {
                "profile": "hogd-default",
            },
        })
    );
}

#[test]
fn hogd_dev_default_funds_each_user_with_one_hundred_hog() {
    assert_eq!(
        HogdDevConfig::default().dev_account_tokens,
        100 * 10_000_000
    );
}
