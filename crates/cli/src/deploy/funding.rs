use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::{decimal, network_config::ExecutionConfig};
use schemars::JsonSchema;
use serde::Serialize;

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
    pub execution: ExecutionConfig,
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
    execution: ExecutionConfig,
    initializer: bool,
) -> Result<FundingReport, CliError> {
    let max = execution
        .effective_limits
        .max_contract_size_bytes
        .ok_or_else(|| invalid("runtime did not report the effective contract-size limit"))?;
    if bytes > u64::from(max) {
        return Err(invalid(format!(
            "artifact has {bytes} bytes; network limit is {max}"
        )));
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
    groundhog_rv32_artifact::verify_artifact(artifact)
        .map_err(|error| invalid(error.to_string()))?;
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
        execution,
        initializer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(balance: &str, base: &str, per_byte: &str) -> ExecutionConfig {
        serde_json::from_value(serde_json::json!({
            "observedTip": { "height": "42", "blockHash": format!("0x{}", "11".repeat(32)) },
            "configHash": format!("0x{}", "22".repeat(32)),
            "effectiveLimits": { "maxContractSizeBytes": u32::MAX },
            "storageCosts": {
                "baseDataItemCost": "12", "bytestringCostPerByte": "10",
                "baseContractDeploymentCost": base, "contractDeploymentCostPerByte": per_byte,
                "authPerKeyCost": "11", "rcqCostPerEntry": "8", "replayCacheCostPerEntry": "7",
                "balanceCost": balance
            }
        }))
        .unwrap()
    }

    #[test]
    fn minimum_covers_first_credit_and_code_but_not_initializer() {
        let report = calculate(100, rules("1234", "12000", "10"), true).unwrap();
        assert_eq!(report.minimum_funding(), 14_234);
        assert_eq!(report.minimum_funding_hog, "0.0014234");
        assert_eq!(report.initializer_funding_raw, None);
        let changed = calculate(100, rules("20", "30", "0"), false).unwrap();
        assert_eq!(changed.minimum_funding(), 50);
        assert_eq!(changed.initializer_funding_raw.as_deref(), Some("0"));
    }

    #[test]
    fn maximum_wire_prices_remain_exact() {
        let max = u64::MAX.to_string();
        let result = calculate(u64::from(u32::MAX), rules(&max, &max, &max), false).unwrap();
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
        assert!(calculate(1, missing, false).is_err());
        assert!(calculate(u64::from(u32::MAX) + 1, rules("0", "0", "0"), false).is_err());
        for invalid_price in ["", "-1", "+1", "1.5", "18446744073709551616"] {
            assert!(calculate(1, rules(invalid_price, "0", "0"), false).is_err());
        }
        assert_eq!(
            calculate(1, rules("0", "0", "0"), false)
                .unwrap()
                .minimum_funding(),
            0
        );
    }
}
