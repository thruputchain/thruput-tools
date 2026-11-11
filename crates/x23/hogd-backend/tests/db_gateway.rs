use thruput_db_gateway::{DbGatewayConfig, DbGatewayConfigResult, DbGatewayResult};
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::NetworkHashText;
use thruput_node_rpc::{DbTip, RPCError, consensus::ConsensusValue};
use thruput_xdr::contract::{ContractType, DeployedContractHash};
use thruput_xdr::header::ProposalHeader;
use thruput_xdr::network_config::{ConfigParam, NetworkConfig, NetworkConfigV0};
use thruput_xdr::storage_cost::StorageCostModelXdr;
use thruput_xdr::types::{ExtensionPoint, SHA256Hash};

use thruput_hogd_backend::db_gateway::*;

#[test]
fn an_outer_batch_error_is_named_with_the_service() {
    let error = payload(
        "DbGateway",
        DbGatewayResult::Default {
            error: xdrs::XDRDiscriminant::from_discriminant(RPCError::kRPCNotFound.0),
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("DbGateway"), "{error}");
}

#[test]
fn db_snapshot_preserves_tip_effective_limits_and_exact_prices() {
    let mut config = db_config();
    config.effective_config = NetworkConfig::_0 {
        v0: NetworkConfigV0 {
            params: vec![ConfigParam::kConfigParamMaxSHA256Buffers {
                max_sha256_buffers: 17,
            }]
            .try_into()
            .unwrap(),
        },
    };
    config.storage_cost_model.base_data_item_cost = u64::MAX;
    let expected_tip =
        thruput_hogd_backend::scheduler::chain_tip("test", &config.tip.value).unwrap();
    let stated = config.tip.network_config.clone();
    let reading = tip_reading(
        "test",
        DbGatewayConfigResult::kRPCNone { config },
        Some(&NetworkHashText::from([0x41; 32])),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&reading.tip).unwrap(),
        serde_json::to_value(&expected_tip).unwrap()
    );
    assert_eq!(reading.network_config, stated);
    let execution = reading.execution.unwrap();
    assert_eq!(
        serde_json::to_value(&execution.observed_tip).unwrap(),
        serde_json::to_value(&expected_tip).unwrap()
    );
    assert_eq!(execution.effective_limits.max_sha256_buffers, Some(17));
    assert_eq!(
        execution.storage_costs.base_data_item_cost,
        u64::MAX.to_string()
    );
}

#[test]
fn db_snapshot_rejects_wrong_network_and_uncommitted_tip() {
    assert!(
        tip_reading(
            "test",
            DbGatewayConfigResult::kRPCNone {
                config: db_config()
            },
            Some(&NetworkHashText::from([0x42; 32]))
        )
        .is_err()
    );
    let mut config = db_config();
    config.tip.value.proposal_header.block_height = (-1).into();
    assert!(matches!(
        tip_reading(
            "test",
            DbGatewayConfigResult::kRPCNone {
                config: config.clone()
            },
            None
        ),
        Err(RpcServerError::BackendUnavailable(_))
    ));
    // Below the pre-genesis -1 is not "uncommitted" but inconsistent data.
    config.tip.value.proposal_header.block_height = (-2).into();
    assert!(matches!(
        tip_reading("test", DbGatewayConfigResult::kRPCNone { config }, None),
        Err(RpcServerError::BackendDataInvalid { .. })
    ));
}

#[test]
fn tip_reading_preserves_native_and_rv32_default_contract_identity() {
    for (contract_type, contract_size) in [(ContractType::kNative, 0), (ContractType::kRv32, 6421)]
    {
        let mut config = db_config();
        config.tip.default_wallet = DeployedContractHash {
            hash: [7; 32].into(),
            contract_type,
            contract_size,
        };
        let reading =
            tip_reading("test", DbGatewayConfigResult::kRPCNone { config }, None).unwrap();
        let wallet = reading.default_wallet.unwrap();
        assert_eq!(*reading.tip.height, 1);
        assert_eq!(wallet.hash.0, SHA256Hash::from([7; 32]));
        assert_eq!(wallet.contract_type, contract_type.0);
        assert_eq!(wallet.size, contract_size);
    }
}

/// A configuration reading committed at height 1 on network `[0x41; 32]`.
fn db_config() -> DbGatewayConfig {
    let hash = |byte: u8| SHA256Hash::from([byte; 32]);
    let network_config = NetworkConfig::_0 {
        v0: NetworkConfigV0 {
            params: vec![
                ConfigParam::kConfigParamMaxSHA256Buffers {
                    max_sha256_buffers: 0,
                },
                ConfigParam::kConfigParamMaxCrossInvokeDepth {
                    max_cross_invoke_depth: 0,
                },
            ]
            .try_into()
            .unwrap(),
        },
    };
    DbGatewayConfig {
        effective_config: network_config.clone(),
        tip: DbTip {
            id: 1,
            storage_long_hash: [0u8; 2048].into(),
            default_wallet: DeployedContractHash {
                hash: hash(0),
                contract_size: 0,
                contract_type: ContractType::kNative,
            },
            network_config,
            value: ConsensusValue {
                protocol: 0.into(),
                ext: ExtensionPoint::_0,
                prev_hash: hash(0x42).into(),
                proposal_header: ProposalHeader {
                    ext: ExtensionPoint::_0,
                    network_hash: hash(0x41).into(),
                    config_hash: hash(0).into(),
                    proposer: [0x40; 32].into(),
                    block_timepoint: 1.into(),
                    block_height: 1.into(),
                    txs_hash: hash(0x43).into(),
                    storage_hash: hash(0x44).into(),
                    events_hash: hash(0x45).into(),
                },
                proposal_txs_hash: hash(0x46).into(),
                upgrades: xdr_common::VecM::default(),
            },
        },
        storage_cost_model: StorageCostModelXdr {
            ext: ExtensionPoint::_0,
            base_data_item_cost: 12_000,
            bytestring_cost_per_byte: 10,
            base_contract_deployment_cost: 12_000,
            contract_deployment_cost_per_byte: 10,
            auth_per_key_cost: 1111,
            rcq_cost_per_entry: 8,
            replay_cache_cost_per_entry: 7,
            balance_cost: 1234,
        },
    }
}
