use std::path::Path;

use groundhog_json_rpc_types::receipt::ExecStatus;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::account_resolution::{
    LocalBootstrapRole, lookup_dev_account_address, resolve_local_bootstrap_account,
};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::dev_keys;
use crate::faucet_client::post_json;
use crate::hog;
use crate::result::CommandResult;
use crate::runtime_manifest::{
    apply_local_node_state_for_local_bootstrap, load_configured_runtime_manifest,
};
use crate::tx_utils::{
    ensure_tx_success, format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_blocking,
};
use crate::{CliError, Print};

pub const AIRDROP_JSON_SCHEMA_VERSION: u32 = 1;
const DEFAULT_AIRDROP_HOG: &str = "10";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparedAirdrop {
    Local(PreparedLocalAirdrop),
    Remote(PreparedRemoteAirdrop),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedLocalAirdrop {
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    pub(crate) amount_hog: String,
    pub(crate) raw_amount: u128,
    pub(crate) faucet_key_name: String,
    pub(crate) faucet_role: String,
    pub(crate) tx_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedRemoteAirdrop {
    pub(crate) faucet_url: String,
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
}

#[derive(Debug, Clone, Serialize)]
struct RemoteAirdropRequest {
    recipient: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAirdropResponse {
    raw_amount: String,
    tx_hash: TxHash,
    height: groundhog_json_rpc_types::BlockHeight,
    status: ExecStatus,
}

pub fn run(
    recipient: &str,
    amount: Option<&str>,
    dry_run: bool,
    profile: &str,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let prepared = prepare_airdrop(&cwd, recipient, amount, profile, overrides)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    match prepared {
        PreparedAirdrop::Local(prepared) => run_local_airdrop(&prepared, cmd, print)?,
        PreparedAirdrop::Remote(prepared) => run_remote_airdrop(&prepared, cmd, print)?,
    }

    Ok(())
}

fn run_local_airdrop(
    prepared: &PreparedLocalAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Airdropping {} HOG to {}...",
        prepared.amount_hog, prepared.recipient_wallet
    ));
    let (tx_hash, receipt) = submit_and_poll_blocking(&prepared.rpc_url, &prepared.tx_hex)?;
    let tx_hash = format_tx_hash(&tx_hash);
    ensure_tx_success("airdrop", &receipt, &tx_hash)?;
    print.status(format_args!("Airdrop sent: {tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "local",
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "amount_hog": prepared.amount_hog,
            "raw_amount": prepared.raw_amount.to_string(),
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "tx_hash": tx_hash,
            "height": receipt.height,
            "status": receipt.status,
        }),
    ))
}

fn run_remote_airdrop(
    prepared: &PreparedRemoteAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Requesting remote airdrop to {}...",
        prepared.recipient_wallet
    ));
    let client = reqwest::blocking::Client::new();
    let response: RemoteAirdropResponse = post_json(
        &client,
        &format!("{}/v1/airdrop", prepared.faucet_url),
        &RemoteAirdropRequest {
            recipient: prepared.recipient_wallet.clone(),
        },
    )?;
    if !matches!(response.status, ExecStatus::Ok) {
        return Err(CliError::Other(
            "remote airdrop transaction did not commit successfully".into(),
        ));
    }
    let tx_hash = format_tx_hash(&response.tx_hash);
    print.status(format_args!("Airdrop sent: {tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "faucet_url": prepared.faucet_url,
            "rpc_url": prepared.rpc_url,
            "raw_amount": response.raw_amount,
            "tx_hash": tx_hash,
            "height": response.height,
            "status": response.status,
        }),
    ))
}

pub(crate) fn prepare_airdrop(
    cwd: &Path,
    recipient: &str,
    amount: Option<&str>,
    profile_name: &str,
    overrides: &ProfileOverrides,
) -> Result<PreparedAirdrop, CliError> {
    let profile = resolve_profile_with_overrides(cwd, Some(profile_name), overrides)?;
    if profile.faucet_url.is_some() && profile.runtime_manifest.is_none() {
        if amount.is_some() {
            return Err(CliError::Other(
                "remote airdrop amount is set by the faucet service".into(),
            ));
        }
        return prepare_remote_airdrop(&profile, recipient);
    }

    prepare_local_airdrop(cwd, profile, recipient, amount)
}

fn prepare_local_airdrop(
    _cwd: &Path,
    mut profile: ResolvedProfile,
    recipient: &str,
    amount: Option<&str>,
) -> Result<PreparedAirdrop, CliError> {
    apply_local_node_state_for_local_bootstrap(&mut profile)?;
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let manifest = load_configured_runtime_manifest(&profile, "faucet")?
        .ok_or_else(|| CliError::Other("airdrop requires a local runtime manifest".into()))?;
    let wallet_entrypoint = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| CliError::Other(error.to_string()))?;
    let recipient_wallet = resolve_airdrop_recipient(&profile, recipient)?;
    let recipient_address = parse_contract_address(&recipient_wallet)?;
    let raw_amount = parse_hog_amount(amount.unwrap_or(DEFAULT_AIRDROP_HOG))?;
    let amount_hog = format_hog_amount(raw_amount);
    let faucet = resolve_local_bootstrap_account(&profile, LocalBootstrapRole::Faucet)?;

    let hog_address = parse_contract_address(hog::TOKEN_ADDRESS)?;
    let transfer_entrypoint = ContractEntrypointID::from(hog::TRANSFER_ENTRYPOINT);
    let transfer_calldata = hog::encode_transfer_calldata(&recipient_address, raw_amount)?;

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let block_expiry_height = rt.block_on(async { resolve_block_expiry_height(&rpc_url).await })?;

    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: faucet.wallet_address,
        wallet_entrypoint,
        target_address: hog_address,
        target_entrypoint: transfer_entrypoint,
        target_calldata: transfer_calldata.as_slice(),
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height,
        nonce: nonce_from_timestamp(),
    })
    .map_err(|error| CliError::Other(error.to_string()))?;
    let tx_hex =
        sign_and_encode_transaction_hex(tx, &faucet.signer, SignTransactionParams::default())
            .map_err(|error| CliError::Other(error.to_string()))?;

    Ok(PreparedAirdrop::Local(PreparedLocalAirdrop {
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        amount_hog,
        raw_amount,
        faucet_key_name: faucet.key_name,
        faucet_role: faucet.role,
        tx_hex,
    }))
}

fn prepare_remote_airdrop(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<PreparedAirdrop, CliError> {
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| CliError::Other("remote airdrop requires faucet_url".into()))?;
    let faucet_url = faucet_url.trim_end_matches('/').to_owned();
    let recipient_wallet = resolve_airdrop_recipient(profile, recipient)?;
    parse_contract_address(&recipient_wallet)?;

    Ok(PreparedAirdrop::Remote(PreparedRemoteAirdrop {
        faucet_url,
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
    }))
}

fn resolve_airdrop_recipient(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<String, CliError> {
    if let Some(wallet) = profile.resolve_wallet(recipient) {
        return Ok(wallet);
    }
    if recipient.starts_with("0x") {
        parse_contract_address(recipient).map_err(|_| {
            CliError::Other("wallet address must be a 0x-prefixed 64-character hex string".into())
        })?;
        return Ok(recipient.to_owned());
    }
    if let Some(index) = dev_keys::dev_key_index(recipient) {
        let address = lookup_dev_account_address(profile, recipient, index)?;
        return Ok(format_contract_address(&address.0));
    }
    Err(CliError::Other(format!(
        "unknown wallet `{recipient}`; run `groundhog wallet create --key {recipient}` or pass a 0x-prefixed wallet address"
    )))
}

fn print_dry_run(
    prepared: &PreparedAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    match prepared {
        PreparedAirdrop::Local(prepared) => {
            print.status(format_args!(
                "Prepared dry-run airdrop of {} HOG to {}.",
                prepared.amount_hog, prepared.recipient_wallet
            ));
            print.status(format_args!("  raw amount: {}", prepared.raw_amount));
            print.status(format_args!(
                "  faucet: {} ({})",
                prepared.faucet_key_name, prepared.faucet_role
            ));
            print.status("  submitted: no (--dry-run)");

            print.json(&CommandResult::new(
                cmd,
                json!({
                    "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
                    "dry_run": true,
                    "submitted": false,
                    "mode": "local",
                    "recipient": prepared.recipient_input,
                    "recipient_wallet": prepared.recipient_wallet,
                    "amount_hog": prepared.amount_hog,
                    "raw_amount": prepared.raw_amount.to_string(),
                    "faucet": {
                        "key": prepared.faucet_key_name,
                        "role": prepared.faucet_role,
                    },
                }),
            ))?;
        }
        PreparedAirdrop::Remote(prepared) => {
            print.status(format_args!(
                "Prepared dry-run remote airdrop to {}.",
                prepared.recipient_wallet
            ));
            print.status(format_args!("  faucet: {}", prepared.faucet_url));
            print.status("  submitted: no (--dry-run)");

            print.json(&CommandResult::new(
                cmd,
                json!({
                    "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
                    "dry_run": true,
                    "submitted": false,
                    "mode": "remote",
                    "recipient": prepared.recipient_input,
                    "recipient_wallet": prepared.recipient_wallet,
                    "faucet_url": prepared.faucet_url,
                    "rpc_url": prepared.rpc_url,
                    "planned_requests": [
                        "POST /v1/airdrop"
                    ],
                }),
            ))?;
        }
    }

    Ok(())
}

fn parse_hog_amount(input: &str) -> Result<u128, CliError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(CliError::Other("airdrop amount cannot be empty".into()));
    }
    if input.starts_with('-') || input.starts_with('+') {
        return Err(CliError::Other("airdrop amount must be positive".into()));
    }
    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fractional = parts.next();
    if parts.next().is_some() || whole.is_empty() || !whole.chars().all(|c| c.is_ascii_digit()) {
        return Err(CliError::Other(format!("invalid HOG amount `{input}`")));
    }
    let whole = whole
        .parse::<u128>()
        .map_err(|error| CliError::Other(format!("invalid HOG amount `{input}`: {error}")))?;
    let fractional = match fractional {
        Some(value) => parse_fractional_hog(input, value)?,
        None => 0,
    };
    let raw = whole
        .checked_mul(hog::SCALE)
        .and_then(|value| value.checked_add(fractional))
        .ok_or_else(|| CliError::Other(format!("HOG amount `{input}` is too large")))?;
    if raw == 0 {
        return Err(CliError::Other(
            "airdrop amount must be greater than zero".into(),
        ));
    }
    Ok(raw)
}

fn parse_fractional_hog(input: &str, fractional: &str) -> Result<u128, CliError> {
    if fractional.is_empty()
        || fractional.len() > hog::DECIMALS as usize
        || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err(CliError::Other(format!(
            "invalid HOG amount `{input}`; at most {} decimal places are supported",
            hog::DECIMALS
        )));
    }
    let mut padded = fractional.to_owned();
    while padded.len() < hog::DECIMALS as usize {
        padded.push('0');
    }
    padded
        .parse::<u128>()
        .map_err(|error| CliError::Other(format!("invalid HOG amount `{input}`: {error}")))
}

pub(crate) fn format_hog_amount(raw_amount: u128) -> String {
    let whole = raw_amount / hog::SCALE;
    let fractional = raw_amount % hog::SCALE;
    if fractional == 0 {
        return whole.to_string();
    }
    let fractional = format!("{fractional:0width$}", width = hog::DECIMALS as usize)
        .trim_end_matches('0')
        .to_owned();
    format!("{whole}.{fractional}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

    use super::{
        DEFAULT_AIRDROP_HOG, PreparedAirdrop, format_hog_amount, parse_hog_amount,
        prepare_remote_airdrop, resolve_airdrop_recipient,
    };
    use crate::config::ResolvedProfile;
    use crate::hog;

    #[test]
    fn parses_hog_amounts_to_raw_units() {
        assert_eq!(parse_hog_amount(DEFAULT_AIRDROP_HOG).unwrap(), 100_000_000);
        assert_eq!(parse_hog_amount("1").unwrap(), 10_000_000);
        assert_eq!(parse_hog_amount("1.5").unwrap(), 15_000_000);
        assert_eq!(parse_hog_amount("0.0000001").unwrap(), 1);
        assert_eq!(parse_hog_amount("100.0000000").unwrap(), 1_000_000_000);
    }

    #[test]
    fn rejects_invalid_hog_amounts() {
        for input in [
            "",
            "0",
            "0.0000000",
            "1.00000001",
            ".1",
            "1.",
            "-1",
            "1.2.3",
        ] {
            assert!(parse_hog_amount(input).is_err(), "{input} should fail");
        }
    }

    #[test]
    fn formats_raw_units_as_hog() {
        assert_eq!(format_hog_amount(10_000_000), "1");
        assert_eq!(format_hog_amount(15_000_000), "1.5");
        assert_eq!(format_hog_amount(1), "0.0000001");
    }

    #[test]
    fn resolves_saved_or_direct_recipient() {
        let mut wallets = BTreeMap::new();
        wallets.insert("alice".into(), format!("0x{}", "ab".repeat(32)));
        let profile = test_profile(wallets);

        assert_eq!(
            resolve_airdrop_recipient(&profile, "alice").unwrap(),
            format!("0x{}", "ab".repeat(32))
        );
        assert_eq!(
            resolve_airdrop_recipient(&profile, &format!("0x{}", "cd".repeat(32))).unwrap(),
            format!("0x{}", "cd".repeat(32))
        );
        assert!(resolve_airdrop_recipient(&profile, "missing").is_err());
    }

    #[test]
    fn malformed_direct_recipient_reports_address_format() {
        let profile = test_profile(BTreeMap::new());

        let err = resolve_airdrop_recipient(&profile, "0xdeadbeef")
            .expect_err("bug: malformed direct address should fail");

        assert!(err.to_string().contains("64-character hex string"));
    }

    #[test]
    fn prepares_remote_airdrop_from_profile_faucet() {
        let mut wallets = BTreeMap::new();
        wallets.insert("alice".into(), format!("0x{}", "ab".repeat(32)));
        let mut profile = test_profile(wallets);
        profile.faucet_url = Some("http://127.0.0.1:8081/".into());

        let prepared = prepare_remote_airdrop(&profile, "alice").unwrap();

        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };
        assert_eq!(prepared.faucet_url, "http://127.0.0.1:8081");
        assert_eq!(prepared.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(prepared.recipient_input, "alice");
        assert_eq!(prepared.recipient_wallet, format!("0x{}", "ab".repeat(32)));
    }

    #[test]
    fn hog_transfer_entrypoint_matches_standard_erc20_interface() {
        let entrypoint = contract_entrypoint_id(
            Some("ERC20"),
            "0.1.3",
            &NormalizedMethodSignature::new("transfer"),
        );

        assert_eq!(entrypoint.as_ref(), hog::TRANSFER_ENTRYPOINT);
    }

    fn test_profile(wallets: BTreeMap<String, String>) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            rpc_url: Some("http://127.0.0.1:9545".into()),
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::default(),
            wallets,
            source: None,
        }
    }
}
