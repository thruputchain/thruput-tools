use std::path::Path;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::{ContractAddress, balance::BalanceResponse};
use serde_json::json;

use crate::account_resolution::resolve_address_target;
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, resolve_profile_with_overrides};
use crate::hog;
use crate::result::CommandResult;
use crate::{CliError, Print};

/// Read native HOG balance for a wallet, contract, dev account, or address.
pub fn run(
    target: &str,
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

    let address = resolve_address_target(&resolved_profile, target)?;
    let address_hex = format_contract_address(&address);
    let address = ContractAddress::from(address);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let balance = rt.block_on(fetch_balance(&rpc_url, address))?;
    let raw_balance = parse_raw_balance(&balance.raw_balance)?;
    let balance_hog = hog::format_amount(raw_balance);

    print.status(format_args!("Balance: {balance_hog} HOG"));
    print.status(format_args!("Address: {address_hex}"));
    print.json(&CommandResult::new(
        cmd,
        json!({
            "rpc_url": rpc_url,
            "target": target,
            "address": address_hex,
            "raw_balance": balance.raw_balance,
            "balance_hog": balance_hog,
            "height": balance.height,
        }),
    ))?;

    Ok(())
}

async fn fetch_balance(
    rpc_url: &str,
    address: ContractAddress,
) -> Result<BalanceResponse, CliError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;
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
