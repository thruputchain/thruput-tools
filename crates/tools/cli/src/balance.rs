use std::path::Path;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::{ContractAddress, balance::BalanceResponse};
use serde_json::json;

use crate::account_resolution::lookup_dev_account_address;
use crate::airdrop::format_hog_amount;
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{
    ProfileOverrides, ResolvedProfile, resolve_default_key, resolve_profile_with_overrides,
};
use crate::dev_keys;
use crate::result::CommandResult;
use crate::runtime_manifest::apply_local_node_state_for_dev_key;
use crate::{CliError, Print};

/// Read native HOG balance for a wallet, contract, dev account, or address.
pub fn run(
    target: &str,
    project_start: &Path,
    profile: &str,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let mut resolved_profile =
        resolve_profile_with_overrides(project_start, Some(profile), overrides)?;
    if let Some(key_name) = resolve_default_key(project_start, Some(profile))? {
        apply_local_node_state_for_dev_key(&mut resolved_profile, &key_name)?;
    }
    if dev_keys::dev_key_index(target).is_some() {
        apply_local_node_state_for_dev_key(&mut resolved_profile, target)?;
    }
    let rpc_url = resolved_profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?;

    let address_hex = resolve_balance_target(&resolved_profile, target)?;
    let address = ContractAddress::from(parse_contract_address(&address_hex)?);

    let rt = tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let balance = rt.block_on(fetch_balance(rpc_url, address))?;
    let raw_balance = parse_raw_balance(&balance.raw_balance)?;
    let balance_hog = format_hog_amount(raw_balance);

    print.status(format_args!("Address: {address_hex}"));
    print.status(format_args!("Balance: {balance_hog} HOG"));
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

fn resolve_balance_target(profile: &ResolvedProfile, target: &str) -> Result<String, CliError> {
    if let Some(wallet) = profile.resolve_wallet(target) {
        return Ok(wallet);
    }
    let contract = profile.resolve_contract(target);
    if contract != target {
        return Ok(contract);
    }
    if target.starts_with("0x") || target.starts_with("0X") {
        return Ok(target.to_owned());
    }
    if let Some(index) = dev_keys::dev_key_index(target) {
        let address = lookup_dev_account_address(profile, target, index)?;
        return Ok(format_contract_address(&address.0));
    }
    Err(CliError::Other(format!(
        "unknown balance target `{target}`; pass a 0x-prefixed address, wallet name, contract alias, or dev-N"
    )))
}

fn parse_raw_balance(raw: &str) -> Result<u128, CliError> {
    raw.parse::<u128>()
        .map_err(|error| CliError::Other(format!("invalid raw balance `{raw}` from RPC: {error}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn resolves_wallet_before_contract_alias() {
        let mut profile = ResolvedProfile {
            name: "default".into(),
            rpc_url: None,
            default_key: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        };
        profile.wallets.insert("alice".into(), format_address(1));
        profile.contracts.insert("alice".into(), format_address(2));

        assert_eq!(
            resolve_balance_target(&profile, "alice").unwrap(),
            format_address(1)
        );
    }

    #[test]
    fn resolves_contract_alias() {
        let mut profile = ResolvedProfile {
            name: "default".into(),
            rpc_url: None,
            default_key: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        };
        profile
            .contracts
            .insert("counter".into(), format_address(3));

        assert_eq!(
            resolve_balance_target(&profile, "counter").unwrap(),
            format_address(3)
        );
    }

    #[test]
    fn resolves_direct_address() {
        let profile = ResolvedProfile {
            name: "default".into(),
            rpc_url: None,
            default_key: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::new(),
            wallets: BTreeMap::new(),
            source: None,
        };
        let address = format_address(4);

        assert_eq!(resolve_balance_target(&profile, &address).unwrap(), address);
    }

    fn format_address(last: u8) -> String {
        let mut bytes = [0u8; 32];
        bytes[31] = last;
        format!("0x{}", hex::encode(bytes))
    }
}
