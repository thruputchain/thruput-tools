use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use thruput_json_rpc_client::GroundhogApiClient;
use thruput_json_rpc_types::{decimal, network_config::ExecutionConfig};
use thruput_rv32_artifact::DeclaredSizes;

use crate::CliError;

/// Storage collateral for a new, unfunded RV32 contract address.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(super) struct FundingReport {
    schema_version: u32,
    #[serde(with = "decimal")]
    #[schemars(with = "decimal::Decimal<u64>")]
    pub artifact_bytes: u64,
    #[serde(with = "decimal")]
    #[schemars(with = "decimal::Decimal<u128>")]
    balance_deposit_raw: u128,
    #[serde(with = "decimal")]
    #[schemars(with = "decimal::Decimal<u128>")]
    deployment_cost_raw: u128,
    #[serde(with = "decimal")]
    #[schemars(with = "decimal::Decimal<u128>")]
    minimum_funding_raw: u128,
    pub minimum_funding_hog: String,
    /// None means initializer effects were not estimated.
    initializer_funding_raw: Option<String>,
    pub execution: Arc<ExecutionConfig>,
}

impl FundingReport {
    pub fn minimum_funding(&self) -> u128 {
        self.minimum_funding_raw
    }
    pub fn balance_deposit(&self) -> u128 {
        self.balance_deposit_raw
    }
    pub fn deployment_cost(&self) -> u128 {
        self.deployment_cost_raw
    }
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::DeploymentFunding {
        message: message.into(),
    }
}

fn price(value: &str) -> Result<u128, CliError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("storage prices must be unsigned decimal integers"));
    }
    value
        .parse::<u64>()
        .map(u128::from)
        .map_err(|_| invalid("storage price exceeds u64"))
}

fn calculate(
    bytes: u64,
    sizes: DeclaredSizes,
    execution: impl Into<Arc<ExecutionConfig>>,
    initializer: bool,
) -> Result<FundingReport, CliError> {
    let execution = execution.into();
    let limits = &execution.effective_limits;
    for (name, actual, limit) in [
        ("contract size", bytes, limits.max_contract_size_bytes),
        (
            "RV32 memory",
            u64::from(sizes.memory_bytes),
            limits.max_rv32_memory_bytes,
        ),
        (
            "RV32 code",
            u64::from(sizes.code_bytes),
            limits.max_rv32_code_bytes,
        ),
    ] {
        let max = limit
            .ok_or_else(|| invalid(format!("runtime did not report the effective {name} limit")))?;
        if actual > u64::from(max) {
            return Err(invalid(format!(
                "{name} is {actual} bytes; network limit is {max}"
            )));
        }
    }
    let costs = &execution.storage_costs;
    let balance = price(&costs.balance_cost)?;
    let deployment = price(&costs.base_contract_deployment_cost)?
        + u128::from(bytes) * price(&costs.contract_deployment_cost_per_byte)?;
    let minimum = balance + deployment;
    Ok(FundingReport {
        schema_version: 1,
        artifact_bytes: bytes,
        balance_deposit_raw: balance,
        deployment_cost_raw: deployment,
        minimum_funding_raw: minimum,
        minimum_funding_hog: crate::hog::format_amount(minimum),
        initializer_funding_raw: (!initializer).then(|| "0".into()),
        execution,
    })
}

#[tracing::instrument(skip_all)]
pub(super) fn read(
    rpc_url: &str,
    artifact: &[u8],
    initializer: bool,
) -> Result<FundingReport, CliError> {
    thruput_rv32_artifact::verify_artifact(artifact).map_err(|error| invalid(error.to_string()))?;
    let sizes = thruput_rv32_artifact::declared_sizes(artifact)
        .ok_or_else(|| invalid("artifact does not report RV32 code and memory sizes"))?;
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let execution = rt.block_on(async {
        let client = crate::http_client::rpc_client(rpc_url)?;
        client
            .chain_config()
            .await
            .map_err(|error| invalid(error.to_string()))?
            .execution
            .ok_or_else(|| invalid("RPC did not supply authoritative execution rules"))
    })?;
    calculate(
        u64::try_from(artifact.len()).map_err(|_| invalid("artifact size exceeds u64"))?,
        sizes,
        execution,
        initializer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(balance: &str, base: &str, per_byte: &str) -> ExecutionConfig {
        serde_json::from_value(serde_json::json!({
            "observedTip": { "height": "42", "blockHash": thruput_json_rpc_types::NetworkHashText::from([0x11; 32]) },
            "configHash": thruput_json_rpc_types::NetworkHashText::from([0x22; 32]),
            "effectiveLimits": {
                "maxContractSizeBytes": u32::MAX,
                "maxRv32CodeBytes": 16,
                "maxRv32MemoryBytes": 4096
            },
            "storageCosts": {
                "baseDataItemCost": "12", "bytestringCostPerByte": "10",
                "baseContractDeploymentCost": base, "contractDeploymentCostPerByte": per_byte,
                "authPerKeyCost": "11", "rcqCostPerEntry": "8", "replayCacheCostPerEntry": "7",
                "balanceCost": balance
            }
        }))
        .unwrap()
    }

    const SIZES: DeclaredSizes = DeclaredSizes {
        code_bytes: 16,
        memory_bytes: 4096,
    };

    #[test]
    fn guest_limits_are_checked_even_when_the_artifact_fits() {
        let artifact = thruput_rv32_artifact::build_artifact(&[0; 16], &[], 0, 4096).unwrap();
        thruput_rv32_artifact::verify_artifact(&artifact).unwrap();
        let sizes = thruput_rv32_artifact::declared_sizes(&artifact).unwrap();
        let bytes = u64::try_from(artifact.len()).unwrap();

        for (code_limit, memory_limit, expected_error) in [
            (Some(16), Some(4096), None),
            (
                Some(15),
                Some(4096),
                Some("RV32 code is 16 bytes; network limit is 15"),
            ),
            (
                Some(16),
                Some(4095),
                Some("RV32 memory is 4096 bytes; network limit is 4095"),
            ),
            (
                None,
                Some(4096),
                Some("runtime did not report the effective RV32 code limit"),
            ),
            (
                Some(16),
                None,
                Some("runtime did not report the effective RV32 memory limit"),
            ),
        ] {
            let mut execution = rules("30", "200", "2");
            execution.effective_limits.max_contract_size_bytes =
                Some(u32::try_from(bytes).unwrap());
            execution.effective_limits.max_rv32_code_bytes = code_limit;
            execution.effective_limits.max_rv32_memory_bytes = memory_limit;
            let result = calculate(bytes, sizes, execution, false);
            if let Some(message) = expected_error {
                assert!(result.unwrap_err().to_string().contains(message));
            } else {
                assert_eq!(
                    result.unwrap().minimum_funding(),
                    230 + u128::from(bytes) * 2
                );
            }
        }
    }

    #[test]
    fn minimum_covers_first_credit_and_code_but_not_initializer() {
        let report = calculate(100, SIZES, rules("1234", "12000", "10"), true).unwrap();
        assert_eq!(report.minimum_funding(), 14_234);
        assert_eq!(report.minimum_funding_hog, "0.0014234");
        assert_eq!(report.initializer_funding_raw, None);
        let changed = calculate(100, SIZES, rules("20", "30", "0"), false).unwrap();
        assert_eq!(changed.minimum_funding(), 50);
        assert_eq!(changed.initializer_funding_raw.as_deref(), Some("0"));
    }

    #[test]
    fn maximum_wire_prices_remain_exact() {
        let max = u64::MAX.to_string();
        let result = calculate(u64::from(u32::MAX), SIZES, rules(&max, &max, &max), false).unwrap();
        assert_eq!(
            result.minimum_funding(),
            u128::from(u64::MAX) * (u128::from(u32::MAX) + 2)
        );
        assert_eq!(
            serde_json::to_value(&result).unwrap()["minimumFundingRaw"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap(),
            result.minimum_funding()
        );
    }

    #[test]
    fn unavailable_or_invalid_inputs_are_not_zero_cost() {
        let mut missing = rules("0", "0", "0");
        missing.effective_limits.max_contract_size_bytes = None;
        assert!(calculate(1, SIZES, missing, false).is_err());
        assert!(calculate(u64::from(u32::MAX) + 1, SIZES, rules("0", "0", "0"), false).is_err());
        for invalid_price in ["", "-1", "+1", "1.5", "18446744073709551616"] {
            assert!(calculate(1, SIZES, rules(invalid_price, "0", "0"), false).is_err());
        }
        assert_eq!(
            calculate(1, SIZES, rules("0", "0", "0"), false)
                .unwrap()
                .minimum_funding(),
            0
        );
    }
}
