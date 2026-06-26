use std::path::Path;

use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, sign_and_encode_transaction_hex,
};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;
use serde_json::json;

use crate::account_resolution::{
    LocalBootstrapRole, lookup_dev_account_address, resolve_local_bootstrap_account,
};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::dev_keys;
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
const HOG_DECIMALS: u32 = 7;
const HOG_SCALE: u128 = 10_000_000;
const HOG_TOKEN_ADDRESS: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";
const HOG_TRANSFER_ENTRYPOINT: [u8; 8] = 0x6328_5b35_23f9_dc3f_u64.to_be_bytes();

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAirdrop {
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    pub(crate) amount_hog: String,
    pub(crate) raw_amount: u128,
    pub(crate) faucet_key_name: String,
    pub(crate) faucet_role: String,
    pub(crate) tx_hex: String,
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
    ))?;

    Ok(())
}

pub(crate) fn prepare_airdrop(
    cwd: &Path,
    recipient: &str,
    amount: Option<&str>,
    profile_name: &str,
    overrides: &ProfileOverrides,
) -> Result<PreparedAirdrop, CliError> {
    let mut profile = resolve_profile_with_overrides(cwd, Some(profile_name), overrides)?;
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

    let hog_address = parse_contract_address(HOG_TOKEN_ADDRESS)?;
    let transfer_entrypoint = ContractEntrypointID::from(HOG_TRANSFER_ENTRYPOINT);
    let transfer_calldata = encode_transfer_calldata(&recipient_address, raw_amount)?;

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

    Ok(PreparedAirdrop {
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        amount_hog,
        raw_amount,
        faucet_key_name: faucet.key_name,
        faucet_role: faucet.role,
        tx_hex,
    })
}

fn resolve_airdrop_recipient(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<String, CliError> {
    if let Some(wallet) = profile.resolve_wallet(recipient) {
        return Ok(wallet);
    }
    if recipient.starts_with("0x") {
        return Err(CliError::Other(
            "wallet address must be a 0x-prefixed 64-character hex string".into(),
        ));
    }
    if let Some(index) = dev_keys::dev_key_index(recipient) {
        let address = lookup_dev_account_address(profile, recipient, index)?;
        return Ok(format_contract_address(&address.0));
    }
    Err(CliError::Other(format!(
        "unknown wallet `{recipient}`; run `groundhog wallet create --key {recipient}` or pass a 0x-prefixed wallet address"
    )))
}

fn encode_transfer_calldata(
    recipient: &ContractAddress,
    raw_amount: u128,
) -> Result<Vec<u8>, CliError> {
    xdrs::cxs::encode(&(recipient, raw_amount)).map_err(|error| {
        CliError::Other(format!("failed to encode HOG transfer calldata: {error}"))
    })
}

fn print_dry_run(
    prepared: &PreparedAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    print.status(format_args!(
        "Prepared dry-run airdrop of {} HOG to {}.",
        prepared.amount_hog, prepared.recipient_wallet
    ));
    print.status(format_args!("  raw amount: {}", prepared.raw_amount));
    print.status(format_args!(
        "  faucet: {} ({})",
        prepared.faucet_key_name, prepared.faucet_role
    ));
    print.status(format_args!("  signed tx: {}", prepared.tx_hex));
    print.status("  submitted: no (--dry-run)");

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "dry_run": true,
            "submitted": false,
            "recipient": prepared.recipient_input,
            "recipient_wallet": prepared.recipient_wallet,
            "amount_hog": prepared.amount_hog,
            "raw_amount": prepared.raw_amount.to_string(),
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "signed_tx": prepared.tx_hex,
        }),
    ))?;

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
        .checked_mul(HOG_SCALE)
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
        || fractional.len() > HOG_DECIMALS as usize
        || !fractional.chars().all(|c| c.is_ascii_digit())
    {
        return Err(CliError::Other(format!(
            "invalid HOG amount `{input}`; at most {HOG_DECIMALS} decimal places are supported"
        )));
    }
    let mut padded = fractional.to_owned();
    while padded.len() < HOG_DECIMALS as usize {
        padded.push('0');
    }
    padded
        .parse::<u128>()
        .map_err(|error| CliError::Other(format!("invalid HOG amount `{input}`: {error}")))
}

pub(crate) fn format_hog_amount(raw_amount: u128) -> String {
    let whole = raw_amount / HOG_SCALE;
    let fractional = raw_amount % HOG_SCALE;
    if fractional == 0 {
        return whole.to_string();
    }
    let fractional = format!("{fractional:0width$}", width = HOG_DECIMALS as usize)
        .trim_end_matches('0')
        .to_owned();
    format!("{whole}.{fractional}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};

    use super::{
        DEFAULT_AIRDROP_HOG, HOG_TRANSFER_ENTRYPOINT, format_hog_amount, parse_hog_amount,
        resolve_airdrop_recipient,
    };
    use crate::config::ResolvedProfile;

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
    fn hog_transfer_entrypoint_matches_standard_erc20_interface() {
        let entrypoint = contract_entrypoint_id(
            Some("ERC20"),
            "0.1.3",
            &NormalizedMethodSignature::new("transfer"),
        );

        assert_eq!(entrypoint.as_ref(), HOG_TRANSFER_ENTRYPOINT);
    }

    fn test_profile(wallets: BTreeMap<String, String>) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            rpc_url: Some("http://127.0.0.1:9545".into()),
            default_key: Some("dev-0".into()),
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            contracts: BTreeMap::default(),
            wallets,
            source: None,
        }
    }
}
