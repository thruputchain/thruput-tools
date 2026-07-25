use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_signing::{
    BuildDevWalletDoActionTransaction, DEFAULT_TX_GAS_LIMIT, SignTransactionParams,
    build_dev_wallet_do_action_transaction, encode_signed_transaction_xdr, sign_transaction,
    transaction_hash,
};

use crate::account_resolution::{resolve_address_target, resolve_wallet_address};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{
    ProfileOverrides, resolve_default_key_for_profile, resolve_profile_with_overrides,
};
use crate::hog;
use crate::keys::load_local_signer;
use crate::network_descriptor::resolve_wallet_entrypoints;
use crate::result::CommandResult;
use crate::transfer_json::{TRANSFER_JSON_SCHEMA_VERSION, TransferDryRunReport};
use crate::tx_utils::{format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height};
use crate::{CliError, Print};

#[derive(Debug)]
struct PreparedTransfer {
    profile: String,
    rpc_url: String,
    sender: groundhog_xdr::types::ContractAddress,
    signer_public_key: groundhog_xdr::types::Ed25519PublicKey,
    recipient_input: String,
    recipient: groundhog_xdr::types::ContractAddress,
    token_address: groundhog_xdr::types::ContractAddress,
    amount: String,
    raw_amount: u128,
    tx_hash: TxHash,
    expiry_height: i64,
    signed_transaction: Vec<u8>,
}

/// Prepare a native HOG transfer from the effective profile wallet.
pub fn run(
    recipient: &str,
    amount: &str,
    dry_run: bool,
    profile: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    if !dry_run {
        return Err(CliError::NotImplemented);
    }

    let prepared = prepare(recipient, amount, profile, overrides)?;
    print_dry_run(&prepared, cmd, print)
}

fn prepare(
    recipient_input: &str,
    amount_input: &str,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
) -> Result<PreparedTransfer, CliError> {
    let cwd = std::env::current_dir()?;
    let profile = resolve_profile_with_overrides(&cwd, profile_name, overrides)?;
    let key_name = resolve_default_key_for_profile(&profile)?;
    if profile.rpc_url.is_none() && key_name.is_none() {
        return Err(CliError::MissingRpcUrl);
    }
    let key_name = key_name.ok_or(CliError::MissingSigningKey)?;
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;

    let recipient = resolve_address_target(&profile, recipient_input)?;
    let raw_amount = hog::parse_amount(amount_input)?;
    let amount = hog::format_amount(raw_amount);
    let token_address = parse_contract_address(hog::TOKEN_ADDRESS)?;
    let calldata = hog::encode_transfer_calldata(&recipient, raw_amount)?;

    let signer = load_local_signer(&key_name)?;
    let sender = resolve_wallet_address(&profile, &key_name, &signer)?;
    let wallet_entrypoints = resolve_wallet_entrypoints(&profile, &key_name)?;
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let expiry_height = rt.block_on(resolve_block_expiry_height(rpc_url))?;

    let tx = build_dev_wallet_do_action_transaction(BuildDevWalletDoActionTransaction {
        wallet_address: sender,
        wallet_entrypoint: wallet_entrypoints.do_action,
        target_address: token_address,
        target_entrypoint: hog::transfer_entrypoint(),
        target_calldata: calldata.as_slice(),
        gas_limit: DEFAULT_TX_GAS_LIMIT,
        target_gas_limit: DEFAULT_TX_GAS_LIMIT,
        block_expiry_height: expiry_height,
        nonce: nonce_from_timestamp(),
    })
    .map_err(|error| CliError::Other(error.to_string()))?;
    let tx_hash = TxHash(
        transaction_hash(&tx)
            .map_err(|error| CliError::Other(error.to_string()))?
            .into(),
    );
    let authenticated_tx = sign_transaction(tx, &signer, SignTransactionParams::default())
        .map_err(|error| CliError::Other(error.to_string()))?;
    let signed_transaction = encode_signed_transaction_xdr(&authenticated_tx)
        .map_err(|error| CliError::Other(error.to_string()))?;

    Ok(PreparedTransfer {
        profile: profile.name,
        rpc_url: rpc_url.to_owned(),
        sender,
        signer_public_key: groundhog_xdr::types::Ed25519PublicKey::from(
            signer.verifying_key_bytes(),
        ),
        recipient_input: recipient_input.to_owned(),
        recipient,
        token_address,
        amount,
        raw_amount,
        tx_hash,
        expiry_height,
        signed_transaction,
    })
}

fn print_dry_run(
    prepared: &PreparedTransfer,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let sender = format_contract_address(&prepared.sender);
    let signer_public_key = format!("0x{}", hex::encode(prepared.signer_public_key.as_ref()));
    let recipient = format_contract_address(&prepared.recipient);
    let token_address = format_contract_address(&prepared.token_address);
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    let signed_transaction = format!("0x{}", hex::encode(&prepared.signed_transaction));

    print.status(format_args!(
        "Prepared signed dry-run transfer of {} HOG to {}.",
        prepared.amount, recipient
    ));
    print.detail("sender", &sender);
    print.detail("signer public key", &signer_public_key);
    print.detail("raw amount", prepared.raw_amount);
    print.detail("token", &token_address);
    print.detail("expiry height", prepared.expiry_height);
    print.detail("tx", &tx_hash);
    print.detail("signed transaction", &signed_transaction);
    print.warn("the signed transaction can be submitted until its expiry height");
    print.status("  submitted: no (--dry-run)");

    let report = TransferDryRunReport {
        schema_version: TRANSFER_JSON_SCHEMA_VERSION,
        profile: prepared.profile.clone(),
        rpc_url: prepared.rpc_url.clone(),
        dry_run: true,
        submitted: false,
        sender: prepared.sender.into(),
        signer_public_key: prepared.signer_public_key.into(),
        recipient_input: prepared.recipient_input.clone(),
        recipient: prepared.recipient.into(),
        token_address: prepared.token_address.into(),
        amount: prepared.amount.clone(),
        raw_amount: prepared.raw_amount.to_string(),
        tx_hash: prepared.tx_hash,
        expiry_height: prepared.expiry_height,
        signed_transaction: HexBytes(prepared.signed_transaction.clone()),
    };
    print.json(&CommandResult::new(cmd, report))
}
