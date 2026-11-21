use thruput_xdr::{
    contract::{Contract, ContractBlob, kDefaultMaxContractSizeBytes},
    network_config::{ConfigParam, NetworkConfig, NetworkConfigV0},
    precompiles::HogexecPrecompiledNativeContracts,
    transaction::{SignedTransaction, Transaction},
};
use xdr_common::{BytesM, VecM};

use thruput_json_rpc_types::error::TxInvalidDetail;
use thruput_rv32_artifact::{DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES};
use thruput_tx_validation::{ConfigParamName, NetworkConfigParams, check_transaction};

/// Canonical signed-transaction vector shared with `json-rpc-server`'s
/// tests; only its inner transaction shape matters here.
const CANONICAL_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
13df30e7ceeb7c4c78ba6287e9d8830cabf5a744f3d6be3139f2428d7bb4f9e7000000000000000000000001\
000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031c9d3fe2554e5c\
e62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d000000010000000050ec72dbafd4898c\
5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a";

fn transaction_with(contracts: Vec<Contract>) -> Transaction {
    let canonical = hex::decode(CANONICAL_SIGNED_TX_XDR).unwrap();
    let mut signed: SignedTransaction = xdrs::txs::decode(&canonical).unwrap();
    signed.tx.contracts_to_deploy = VecM(contracts);
    signed.tx
}

fn rv32(size: usize, fill: u8) -> Contract {
    Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(vec![fill; size]),
        },
    }
}

fn network_config(params: Vec<ConfigParam>) -> NetworkConfig {
    NetworkConfig::_0 {
        v0: NetworkConfigV0 {
            params: VecM(params),
        },
    }
}

/// The `ConfigParam` that states `value` for `name`.
///
/// Exhaustive on purpose: a parameter added to [`ConfigParamName`] does
/// not compile here until it is given its union arm, and the test below
/// then fails until `from_network_config` lands it on its field.
fn stated(name: ConfigParamName, value: u32) -> ConfigParam {
    match name {
        ConfigParamName::MaxSha256Buffers => ConfigParam::kConfigParamMaxSHA256Buffers {
            max_sha256_buffers: value,
        },
        ConfigParamName::MaxCrossInvokeDepth => ConfigParam::kConfigParamMaxCrossInvokeDepth {
            max_cross_invoke_depth: value,
        },
        ConfigParamName::MaxNewContractsPerTx => ConfigParam::kConfigParamMaxNewContractsPerTx {
            max_new_contracts_per_tx: value,
        },
        ConfigParamName::MaxBytestringLen => ConfigParam::kConfigParamMaxBytestringLen {
            max_bytestring_len: value,
        },
        ConfigParamName::MaxContractSizeBytes => ConfigParam::kConfigParamMaxContractSizeBytes {
            max_contract_size_bytes: value,
        },
        ConfigParamName::MaxSavepointDepth => ConfigParam::kConfigParamMaxSavepointDepth {
            max_savepoint_depth: value,
        },
        ConfigParamName::MaxAuthTokenDepth => ConfigParam::kConfigParamMaxAuthTokenDepth {
            max_auth_token_depth: value,
        },
        ConfigParamName::MaxRv32CodeBytes => ConfigParam::kConfigParamMaxRv32CodeBytes {
            max_rv32_code_bytes: value,
        },
        ConfigParamName::MaxRv32MemoryBytes => ConfigParam::kConfigParamMaxRv32MemoryBytes {
            max_rv32_memory_bytes: value,
        },
        ConfigParamName::MaxTxGas => ConfigParam::kConfigParamMaxTxGas {
            max_tx_gas: u64::from(value),
        },
        other => panic!("missing config parameter fixture for {other}"),
    }
}

#[test]
fn every_named_parameter_flattens_onto_the_field_it_names() {
    for (name, value) in ConfigParamName::ALL.iter().copied().zip(1_000u32..) {
        let params =
            NetworkConfigParams::from_network_config(&network_config(vec![stated(name, value)]))
                .unwrap();

        assert_eq!(name.stated_in(&params), Some(u64::from(value)), "{name}");
        for other in ConfigParamName::ALL.iter().copied() {
            if other != name {
                assert_eq!(other.stated_in(&params), None, "{name} also stated {other}");
            }
        }
    }
}

#[test]
fn names_round_trip_and_an_unknown_one_lists_the_known_names() {
    for name in ConfigParamName::ALL.iter().copied() {
        assert_eq!(name.as_str().parse::<ConfigParamName>(), Ok(name));
        assert_eq!(name.to_string(), name.as_str());
    }

    let error = "max_rv32_memmory_bytes"
        .parse::<ConfigParamName>()
        .unwrap_err();

    assert_eq!(error.name, "max_rv32_memmory_bytes");
    assert!(
        error.to_string().contains("max_rv32_memory_bytes"),
        "{error}"
    );
}

fn tx_gas_cap(max_tx_gas: u64) -> NetworkConfigParams {
    NetworkConfigParams {
        max_tx_gas: Some(max_tx_gas),
        ..NetworkConfigParams::default()
    }
}

fn transaction_budgeting(gas_limit: u64) -> Transaction {
    let mut tx = transaction_with(vec![]);
    tx.invocation.gas_limit = gas_limit;
    tx
}

#[test]
fn root_gas_limit_is_bounded_by_the_default_cap_when_unstated() {
    let params = NetworkConfigParams::default();
    assert_eq!(
        check_transaction(&transaction_budgeting(1_000_000_000), &params),
        Ok(())
    );
    assert_eq!(
        check_transaction(&transaction_budgeting(1_000_000_001), &params),
        Err(TxInvalidDetail::GasLimitTooHigh {
            gas_limit: 1_000_000_001,
            max_gas: 1_000_000_000,
        })
    );
    // The SDK's "all remaining gas" sentinel is no root budget.
    assert_eq!(
        check_transaction(&transaction_budgeting(u64::MAX), &params),
        Err(TxInvalidDetail::GasLimitTooHigh {
            gas_limit: u64::MAX,
            max_gas: 1_000_000_000,
        })
    );
}

#[test]
fn root_gas_limit_is_bounded_by_the_stated_cap() {
    // Lower than the default: the stated cap is the one in force.
    assert_eq!(
        check_transaction(&transaction_budgeting(500_001), &tx_gas_cap(500_000)),
        Err(TxInvalidDetail::GasLimitTooHigh {
            gas_limit: 500_001,
            max_gas: 500_000,
        })
    );
    // Higher than the default: a budget past 1B is admitted.
    assert_eq!(
        check_transaction(
            &transaction_budgeting(5_000_000_000),
            &tx_gas_cap(5_000_000_000)
        ),
        Ok(())
    );
}

#[test]
fn gas_limit_is_checked_before_attachments() {
    // hogd refuses the budget before it looks at any attachment, so an
    // over-budget transaction that also attaches too many contracts is
    // reported for its gas.
    let mut tx = transaction_with(vec![rv32(8, 1), rv32(8, 2)]);
    tx.invocation.gas_limit = 1_000_000_001;
    assert_eq!(
        check_transaction(&tx, &max_contracts(1)),
        Err(TxInvalidDetail::GasLimitTooHigh {
            gas_limit: 1_000_000_001,
            max_gas: 1_000_000_000,
        })
    );
}

fn max_contracts(max_new_contracts_per_tx: u32) -> NetworkConfigParams {
    NetworkConfigParams {
        max_new_contracts_per_tx: Some(max_new_contracts_per_tx),
        ..NetworkConfigParams::default()
    }
}

fn contract_size_cap(max_contract_size_bytes: u32) -> NetworkConfigParams {
    NetworkConfigParams {
        max_contract_size_bytes: Some(max_contract_size_bytes),
        ..NetworkConfigParams::default()
    }
}

fn rv32_caps(
    max_rv32_code_bytes: Option<u32>,
    max_rv32_memory_bytes: Option<u32>,
) -> NetworkConfigParams {
    NetworkConfigParams {
        max_rv32_code_bytes,
        max_rv32_memory_bytes,
        ..NetworkConfigParams::default()
    }
}

/// A well-formed v5 artifact carrying `code_bytes` of code and declaring
/// `memory_bytes` of guest memory, as an attached contract.
fn artifact(code_bytes: usize, memory_bytes: usize) -> Contract {
    let artifact =
        thruput_rv32_artifact::build_artifact(&vec![0u8; code_bytes], &[], 0, memory_bytes)
            .unwrap();
    Contract::kRv32 {
        rv32_contract: ContractBlob {
            data: BytesM(artifact),
        },
    }
}

#[test]
fn every_stated_parameter_lands_on_its_field() {
    let config = network_config(vec![
        ConfigParam::kConfigParamMaxSHA256Buffers {
            max_sha256_buffers: 3,
        },
        ConfigParam::kConfigParamMaxCrossInvokeDepth {
            max_cross_invoke_depth: 17,
        },
        ConfigParam::kConfigParamMaxNewContractsPerTx {
            max_new_contracts_per_tx: 7,
        },
        ConfigParam::kConfigParamMaxBytestringLen {
            max_bytestring_len: 4096,
        },
        ConfigParam::kConfigParamMaxContractSizeBytes {
            max_contract_size_bytes: 131_072,
        },
        ConfigParam::kConfigParamMaxSavepointDepth {
            max_savepoint_depth: 9,
        },
        ConfigParam::kConfigParamMaxSHA256BufferBytes {
            max_sha256_buffer_bytes: 2048,
        },
        ConfigParam::kConfigParamMaxAuthTokenDepth {
            max_auth_token_depth: 5,
        },
        ConfigParam::kConfigParamMaxRv32CodeBytes {
            max_rv32_code_bytes: 65536,
        },
        ConfigParam::kConfigParamMaxRv32MemoryBytes {
            max_rv32_memory_bytes: 1_048_576,
        },
        ConfigParam::kConfigParamMaxAuthTokens {
            max_auth_tokens: 64,
        },
        ConfigParam::kConfigParamMaxCalldataLen {
            max_calldata_len: 65_536,
        },
        ConfigParam::kConfigParamMaxReturnBufferLen {
            max_return_buffer_len: 1024,
        },
        ConfigParam::kConfigParamMaxTxGas {
            max_tx_gas: 5_000_000_000,
        },
        ConfigParam::kConfigParamMinRv32ArtifactVersion {
            min_rv32_artifact_version: 5,
        },
        ConfigParam::kConfigParamMaxRv32ArtifactVersion {
            max_rv32_artifact_version: 7,
        },
    ]);

    let params = NetworkConfigParams::from_network_config(&config).unwrap();

    assert_eq!(
        params,
        NetworkConfigParams {
            max_sha256_buffers: Some(3),
            max_cross_invoke_depth: Some(17),
            max_new_contracts_per_tx: Some(7),
            max_bytestring_len: Some(4096),
            max_contract_size_bytes: Some(131_072),
            max_savepoint_depth: Some(9),
            max_auth_token_depth: Some(5),
            max_rv32_code_bytes: Some(65536),
            max_rv32_memory_bytes: Some(1_048_576),
            max_auth_tokens: Some(64),
            max_calldata_len: Some(65_536),
            max_return_buffer_len: Some(1024),
            max_tx_gas: Some(5_000_000_000),
            min_rv32_artifact_version: Some(5),
            max_rv32_artifact_version: Some(7),
        }
    );
}

#[test]
fn unstated_parameters_stay_unset() {
    // A list that stops after the second id is what a chain predating the
    // later parameters states; nothing is invented for the rest.
    let config = network_config(vec![
        ConfigParam::kConfigParamMaxSHA256Buffers {
            max_sha256_buffers: 8,
        },
        ConfigParam::kConfigParamMaxCrossInvokeDepth {
            max_cross_invoke_depth: 64,
        },
    ]);

    let params = NetworkConfigParams::from_network_config(&config).unwrap();

    assert_eq!(
        params,
        NetworkConfigParams {
            max_sha256_buffers: Some(8),
            max_cross_invoke_depth: Some(64),
            ..NetworkConfigParams::default()
        }
    );
    assert_eq!(
        NetworkConfigParams::from_network_config(&network_config(vec![])).unwrap(),
        NetworkConfigParams::default()
    );
}

#[test]
fn transaction_within_bounds_passes() {
    let tx = transaction_with(vec![
        rv32(16, 1),
        rv32(kDefaultMaxContractSizeBytes as usize, 2),
    ]);

    assert_eq!(check_transaction(&tx, &max_contracts(2)), Ok(()));
    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Ok(())
    );
}

#[test]
fn too_many_contracts_is_rejected_only_when_the_bound_is_stated() {
    let tx = transaction_with(vec![rv32(1, 1), rv32(1, 2), rv32(1, 3)]);

    assert_eq!(
        check_transaction(&tx, &max_contracts(2)),
        Err(TxInvalidDetail::TooManyContracts {
            attached: 3,
            max: 2
        })
    );
    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Ok(())
    );
}

#[test]
fn oversized_contract_is_rejected_under_the_protocol_default() {
    let tx = transaction_with(vec![
        rv32(1, 1),
        rv32(kDefaultMaxContractSizeBytes as usize + 1, 2),
    ]);

    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Err(TxInvalidDetail::ContractTooLarge {
            index: 1,
            size_bytes: kDefaultMaxContractSizeBytes + 1,
            max_bytes: kDefaultMaxContractSizeBytes,
        })
    );
}

#[test]
fn stated_contract_size_cap_replaces_the_protocol_default() {
    // A stated cap governs in both directions: below the default it
    // rejects what the default would admit, above it admits what the
    // default would reject.
    let small = transaction_with(vec![rv32(1025, 1)]);
    let large = transaction_with(vec![rv32(kDefaultMaxContractSizeBytes as usize + 1, 1)]);

    assert_eq!(
        check_transaction(&small, &contract_size_cap(1024)),
        Err(TxInvalidDetail::ContractTooLarge {
            index: 0,
            size_bytes: 1025,
            max_bytes: 1024,
        })
    );
    assert_eq!(
        check_transaction(&large, &contract_size_cap(kDefaultMaxContractSizeBytes + 1)),
        Ok(())
    );
}

#[test]
fn count_check_runs_before_size_check() {
    // Cheapest check first: the count is knowable without touching any
    // blob, and the executor orders them the same way.
    let tx = transaction_with(vec![rv32(kDefaultMaxContractSizeBytes as usize + 1, 1); 2]);

    assert!(matches!(
        check_transaction(&tx, &max_contracts(1)),
        Err(TxInvalidDetail::TooManyContracts { .. })
    ));
}

#[test]
fn duplicate_contract_reports_both_positions() {
    let tx = transaction_with(vec![rv32(4, 1), rv32(4, 2), rv32(4, 1)]);

    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Err(TxInvalidDetail::DuplicateContract {
            index: 2,
            first_index: 0,
        })
    );
}

#[test]
fn rv32_sizes_at_the_protocol_defaults_pass() {
    let tx = transaction_with(vec![artifact(DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES)]);

    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Ok(())
    );
}

#[test]
fn rv32_memory_over_the_bound_is_rejected_and_checked_before_code() {
    // Both fields over: memory is reported, as hogd's artifact parser
    // orders the two checks.
    let tx = transaction_with(vec![
        artifact(16, 1),
        artifact(DEFAULT_MAX_CODE_BYTES + 4, DEFAULT_MEMORY_BYTES + 1),
    ]);

    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Err(TxInvalidDetail::Rv32MemoryTooLarge {
            index: 1,
            memory_bytes: 65_537,
            max_bytes: 65_536,
        })
    );
}

#[test]
fn rv32_code_over_the_bound_is_rejected() {
    let tx = transaction_with(vec![artifact(
        DEFAULT_MAX_CODE_BYTES + 4,
        DEFAULT_MEMORY_BYTES,
    )]);

    assert_eq!(
        check_transaction(&tx, &NetworkConfigParams::default()),
        Err(TxInvalidDetail::Rv32CodeTooLarge {
            index: 0,
            code_bytes: 262_148,
            max_bytes: 262_144,
        })
    );
}

#[test]
fn stated_rv32_bounds_replace_the_protocol_defaults() {
    // Above the defaults, a chain configured for it admits the artifact.
    let large = transaction_with(vec![artifact(
        DEFAULT_MAX_CODE_BYTES + 4,
        DEFAULT_MEMORY_BYTES + 1,
    )]);
    assert_eq!(
        check_transaction(&large, &rv32_caps(Some(262_148), Some(65_537))),
        Ok(())
    );

    // Below the defaults, a stated cap rejects what the default admits.
    let small = transaction_with(vec![artifact(1024, 4096)]);
    assert_eq!(
        check_transaction(&small, &rv32_caps(Some(1020), None)),
        Err(TxInvalidDetail::Rv32CodeTooLarge {
            index: 0,
            code_bytes: 1024,
            max_bytes: 1020,
        })
    );
    assert_eq!(
        check_transaction(&small, &rv32_caps(None, Some(4095))),
        Err(TxInvalidDetail::Rv32MemoryTooLarge {
            index: 0,
            memory_bytes: 4096,
            max_bytes: 4095,
        })
    );
}

#[test]
fn a_blob_without_an_artifact_header_is_left_to_the_executor() {
    // Not a v5 artifact: the executor rejects it as unverifiable, and this
    // pre-check has no header to read a declared size from.
    let blob = rv32(DEFAULT_MAX_CODE_BYTES + 1024, 0x5a);

    let tx = transaction_with(vec![blob]);
    assert_eq!(
        check_transaction(&tx, &rv32_caps(Some(16), Some(16))),
        Ok(())
    );
}

#[test]
fn native_contracts_count_but_carry_no_code() {
    let native = Contract::kNative {
        native_contract_id: HogexecPrecompiledNativeContracts::from(0),
    };
    assert_eq!(
        check_transaction(
            &transaction_with(vec![native.clone()]),
            &contract_size_cap(0)
        ),
        Ok(())
    );
    let tx = transaction_with(vec![native, rv32(4, 1)]);
    assert_eq!(
        check_transaction(&tx, &max_contracts(1)),
        Err(TxInvalidDetail::TooManyContracts {
            attached: 2,
            max: 1
        })
    );
}
