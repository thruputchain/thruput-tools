use super::invalid;
use crate::{RpcServerError, TipReading};
use std::str::FromStr;
use thruput_json_rpc_types::{network_config as legacy, responses as out};

pub(super) fn tip(
    value: &thruput_json_rpc_types::block::ChainTip,
) -> Result<out::ChainTip, RpcServerError> {
    super::service::snapshot(&value.height)?;
    Ok(out::ChainTip {
        block_height: value.height.clone(),
        block_hash: value.block_hash,
    })
}

fn amount<T: FromStr + ToString>(value: &str) -> Result<T, RpcServerError> {
    let parsed: T = value
        .parse()
        .map_err(|_| invalid("invalid storage price"))?;
    if parsed.to_string() != value {
        return Err(invalid("noncanonical storage price"));
    }
    Ok(parsed)
}

fn limits(value: legacy::NetworkConfigParams) -> out::ConfigLimits {
    let legacy::NetworkConfigParams {
        max_sha256_buffers,
        max_cross_invoke_depth,
        max_new_contracts_per_tx,
        max_bytestring_len,
        max_contract_size_bytes,
        max_savepoint_depth,
        max_auth_token_depth,
        max_rv32_code_bytes,
        max_rv32_memory_bytes,
    } = value;
    out::ConfigLimits {
        max_sha256_buffers,
        max_cross_invoke_depth,
        max_new_contracts_per_tx,
        max_bytestring_len,
        max_contract_size_bytes,
        max_savepoint_depth,
        max_auth_token_depth,
        max_rv32_code_bytes,
        max_rv32_memory_bytes,
    }
}

pub(super) fn config(reading: TipReading) -> Result<out::ChainConfig, RpcServerError> {
    tip(&reading.tip)?;
    let stated = legacy::NetworkConfigParams::from_network_config(&reading.network_config)
        .map_err(|error| {
            RpcServerError::invalid_data(
                thruput_json_rpc_types::error::native::BackendDataInvalidReason::UnsupportedValue,
                error.to_string(),
            )
        })?;
    let mut result = out::ChainConfig::new(limits(stated));
    if let Some(execution) = reading.execution {
        if execution.observed_tip.height != reading.tip.height
            || execution.observed_tip.block_hash != reading.tip.block_hash
        {
            return Err(invalid(
                "execution configuration is bound to a different tip",
            ));
        }
        let prices = &execution.storage_costs;
        result.execution = Some(out::ExecutionConfig {
            observed_tip: tip(&execution.observed_tip)?,
            config_hash: execution.config_hash,
            effective_limits: limits(execution.effective_limits),
            storage_costs: out::StorageCosts {
                base_data_item_cost: amount(&prices.base_data_item_cost)?,
                bytestring_cost_per_byte: amount(&prices.bytestring_cost_per_byte)?,
                base_contract_deployment_cost: amount(&prices.base_contract_deployment_cost)?,
                contract_deployment_cost_per_byte: amount(
                    &prices.contract_deployment_cost_per_byte,
                )?,
                auth_per_key_cost: amount(&prices.auth_per_key_cost)?,
                rcq_cost_per_entry: amount(&prices.rcq_cost_per_entry)?,
                replay_cache_cost_per_entry: amount(&prices.replay_cache_cost_per_entry)?,
                balance_cost: amount(&prices.balance_cost)?,
            },
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_json_rpc_types::block::ChainTip;
    use thruput_xdr::network_config::{NetworkConfig, NetworkConfigV0};
    use xdr_common::VecM;

    fn reading() -> TipReading {
        TipReading::new(
            ChainTip {
                height: 7.into(),
                block_hash: thruput_json_rpc_types::base58::Base58(
                    thruput_xdr::types::SHA256Hash::from([8; 32]),
                ),
            },
            NetworkConfig::_0 {
                v0: NetworkConfigV0 {
                    params: VecM(vec![]),
                },
            },
        )
    }
    fn execution() -> legacy::ExecutionConfig {
        let prices = serde_json::from_value(serde_json::json!({
            "baseDataItemCost":"9007199254740993", "bytestringCostPerByte":"2", "baseContractDeploymentCost":"3", "contractDeploymentCostPerByte":"4", "authPerKeyCost":"5", "rcqCostPerEntry":"6", "replayCacheCostPerEntry":"7", "balanceCost":"8"
        })).unwrap();
        legacy::ExecutionConfig::new(
            reading().tip,
            thruput_json_rpc_types::base58::Base58(thruput_xdr::types::SHA256Hash::from([9; 32])),
            legacy::NetworkConfigParams::default(),
            prices,
        )
    }
    #[test]
    fn missing_execution_and_limits_are_not_fabricated() {
        let value = serde_json::to_value(config(reading()).unwrap()).unwrap();
        assert!(value.get("execution").is_none());
        assert!(value["stated"].as_object().unwrap().is_empty());
    }
    #[test]
    fn execution_provenance_and_prices_are_preserved_or_rejected() {
        let value =
            serde_json::to_value(config(reading().with_execution(execution())).unwrap()).unwrap();
        assert_eq!(value["execution"]["observedTip"]["blockHeight"], "7");
        assert_eq!(
            value["execution"]["storageCosts"]["baseDataItemCost"],
            "9007199254740993"
        );
        for malformed in ["-1", "01", "18446744073709551616"] {
            let mut execution = execution();
            execution.storage_costs.balance_cost = malformed.into();
            assert!(matches!(
                config(reading().with_execution(execution)),
                Err(RpcServerError::BackendDataInvalid { .. })
            ));
        }
        let mut execution = execution();
        execution.observed_tip.height = 8.into();
        assert!(config(reading().with_execution(execution)).is_err());
    }
}
