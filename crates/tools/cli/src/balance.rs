use std::borrow::Cow;
use std::path::Path;

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::{
    ContractAddress, balance::BalanceResponse, runtime_manifest::RuntimeManifestStorageCostModel,
};
use serde_json::json;

use crate::account_resolution::resolve_address_target_or_default_wallet;
use crate::checks::{Finding, FindingLevel};
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::hog;
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Schema version of the `balance --json` data payload.
pub const BALANCE_JSON_SCHEMA_VERSION: u32 = 2;
const LOW_LOCAL_FAUCET_BALANCE_RAW: u128 = 10 * hog::SCALE;

/// Read native HOG balance for a wallet, contract, dev account, or address.
pub fn run(
    target: Option<&str>,
    project_start: &Path,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let resolved_profile = resolve_profile_with_overrides(project_start, profile, overrides)?;
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();

    let (target, address) = resolve_address_target_or_default_wallet(&resolved_profile, target)?;
    let address_strkey = format_contract_address(&address);
    let address = ContractAddress::from(address);

    let balance = fetch_balance_blocking(&rpc_url, address)?;
    let raw_balance = parse_raw_balance(&balance.raw_balance)?;
    let balance_hog = hog::format_amount(raw_balance);

    print.status(format_args!("Balance: {balance_hog} HOG"));
    print.status(format_args!("Address: {address_strkey}"));
    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": BALANCE_JSON_SCHEMA_VERSION,
            "rpcUrl": rpc_url,
            "target": target,
            "address": address_strkey,
            "rawBalance": balance.raw_balance,
            "balanceHog": balance_hog,
            "height": balance.height,
        }),
    ))?;

    Ok(())
}

pub(crate) fn fetch_balance_blocking(
    rpc_url: &str,
    address: ContractAddress,
) -> Result<BalanceResponse, CliError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    rt.block_on(fetch_balance(rpc_url, address))
}

async fn fetch_balance(
    rpc_url: &str,
    address: ContractAddress,
) -> Result<BalanceResponse, CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    client
        .get_balance(address)
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "getBalance",
            message: e.to_string(),
        })
}

fn parse_raw_balance(raw: &str) -> Result<u128, CliError> {
    raw.parse::<u128>()
        .map_err(|error| CliError::Other(format!("invalid raw balance `{raw}` from RPC: {error}")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalFundingProjection {
    pub(crate) committed_balance: u128,
    pub(crate) transaction_storage_cost: u128,
    pub(crate) estimated_max: u128,
    pub(crate) projected_balance: Option<u128>,
}

pub(crate) fn local_funding_projection(
    raw_balance: &str,
    costs: &RuntimeManifestStorageCostModel,
    requested: u128,
    transaction_count: u128,
) -> Result<LocalFundingProjection, CliError> {
    let committed_balance = parse_raw_balance(raw_balance)?;
    let base_data_item_cost = parse_storage_cost("baseDataItemCost", &costs.base_data_item_cost)?;
    let replay_cache_cost = parse_storage_cost(
        "replayCacheCostPerEntry",
        &costs.replay_cache_cost_per_entry,
    )?;
    let transaction_storage_cost = base_data_item_cost
        .checked_add(replay_cache_cost)
        .and_then(|cost| cost.checked_mul(transaction_count))
        .ok_or_else(|| CliError::Other("local transaction storage cost overflows u128".into()))?;
    let estimated_max = committed_balance.saturating_sub(transaction_storage_cost);
    let projected_balance = estimated_max.checked_sub(requested);

    Ok(LocalFundingProjection {
        committed_balance,
        transaction_storage_cost,
        estimated_max,
        projected_balance,
    })
}

pub(crate) fn insufficient_local_faucet_error(requested: u128, estimated_max: u128) -> CliError {
    CliError::Other(format!(
        "local faucet has insufficient actionable balance: requested {} HOG, estimated maximum is {} HOG; reduce the amount or start a fresh local chain",
        hog::format_amount(requested),
        hog::format_amount(estimated_max)
    ))
}

pub(crate) fn low_local_faucet_balance_warning(projected_balance: Option<u128>) -> Option<Finding> {
    let projected_balance = projected_balance?;
    if projected_balance >= LOW_LOCAL_FAUCET_BALANCE_RAW {
        return None;
    }

    Some(Finding {
        level: FindingLevel::Warn,
        code: "low_local_faucet_balance",
        message: format!(
            "local faucet balance is projected to fall below 10 HOG ({} HOG remaining)",
            hog::format_amount(projected_balance)
        ),
        hint: Some(Cow::Borrowed(
            "reduce the amount or start a fresh local chain",
        )),
    })
}

fn parse_storage_cost(field: &str, raw: &str) -> Result<u128, CliError> {
    raw.parse::<u128>().map_err(|error| {
        CliError::Other(format!(
            "invalid runtime manifest storage cost `{field}` value `{raw}`: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::runtime_manifest::RuntimeManifestStorageCostModel;
    use serde_json::json;

    use super::{local_funding_projection, low_local_faucet_balance_warning};

    #[test]
    fn projection_reserves_active_transaction_storage_cost() {
        let projection = local_funding_projection("10000000000000", &costs(), 500_000_000, 1)
            .expect("bug: projection should succeed");

        assert_eq!(projection.transaction_storage_cost, 12_007);
        assert_eq!(projection.estimated_max, 9_999_999_987_993);
        assert_eq!(projection.projected_balance, Some(9_999_499_987_993));
    }

    #[test]
    fn projection_marks_request_over_estimate() {
        let projection = local_funding_projection("12006", &costs(), 1, 1)
            .expect("bug: projection should succeed");

        assert_eq!(projection.estimated_max, 0);
        assert_eq!(projection.projected_balance, None);
    }

    #[test]
    fn projection_reserves_exact_storage_cost_for_two_transactions() {
        let projection = local_funding_projection("100000", &costs(), 1, 2)
            .expect("bug: projection should succeed");

        assert_eq!(projection.transaction_storage_cost, 24_014);
        assert_eq!(projection.estimated_max, 75_986);
        assert_eq!(projection.projected_balance, Some(75_985));
    }

    #[test]
    fn projection_rejects_transaction_storage_cost_overflow() {
        let error = local_funding_projection("100000", &costs(), 1, u128::MAX)
            .expect_err("bug: transaction cost multiplication should be checked");

        assert!(error.to_string().contains("overflows u128"));
    }

    #[test]
    fn low_faucet_warning_is_emitted_below_ten_hog() {
        let warning = low_local_faucet_balance_warning(Some(10 * crate::hog::SCALE - 1))
            .expect("bug: balance below threshold should warn");

        assert_eq!(warning.code, "low_local_faucet_balance");
    }

    #[test]
    fn low_faucet_warning_is_not_emitted_at_ten_hog() {
        assert!(low_local_faucet_balance_warning(Some(10 * crate::hog::SCALE)).is_none());
    }

    fn costs() -> RuntimeManifestStorageCostModel {
        serde_json::from_value(json!({
            "hash": format!("0x{}", "00".repeat(32)),
            "baseDataItemCost": "12000",
            "bytestringCostPerByte": "10",
            "baseContractDeploymentCost": "12000",
            "contractDeploymentCostPerByte": "10",
            "authPerKeyCost": "1111",
            "rcqCostPerEntry": "8",
            "replayCacheCostPerEntry": "7",
            "balanceCost": "1234"
        }))
        .expect("bug: storage costs should deserialize")
    }
}
