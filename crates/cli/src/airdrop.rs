use std::fs::File;
use std::path::{Path, PathBuf};

use getrandom::getrandom;
use groundhog_json_rpc_client::AirdropApiClient;
use groundhog_json_rpc_types::{
    airdrop::{AirdropAttemptResponse, AirdropRequest},
    receipt::ExecStatus,
};
use groundhog_signing::{
    DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams, sign_and_encode_transaction_hex,
};
use groundhog_xdr::transaction::Transaction;
use groundhog_xdr::types::NetworkHash;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::account_resolution::{
    LocalBootstrapRole, resolve_address_target, resolve_address_target_or_default_wallet,
    resolve_local_bootstrap_account,
};
use crate::balance::{
    LocalFundingProjection, fetch_balance_blocking, insufficient_local_faucet_error,
    local_funding_projection, low_local_faucet_balance_warning,
};
use crate::codec::{format_contract_address, parse_contract_address};
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::hog;
use crate::network_descriptor::{RemoteAirdropDescriptor, fetch_remote_airdrop_descriptor};
use crate::profile_backend::{FaucetBackend, resolve_faucet_backend};
use crate::remote_attempt::{
    attempt_path, lock_attempt, read_attempt, remove_attempt, write_attempt,
};
use crate::result::CommandResult;
use crate::runtime_manifest::load_configured_runtime_manifest;
use crate::tx_utils::{
    ensure_tx_success, format_tx_hash, nonce_from_timestamp, resolve_block_expiry_height,
    submit_and_poll_blocking,
};
use crate::{CliError, Print};

/// Schema version of the `airdrop --json` data payload.
pub const AIRDROP_JSON_SCHEMA_VERSION: u32 = 3;
const DEFAULT_AIRDROP_HOG: &str = "10";

#[derive(Debug)]
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
    pub(crate) faucet_wallet: String,
    pub(crate) faucet_balance_height: groundhog_json_rpc_types::BlockHeight,
    pub(crate) funding_projection: LocalFundingProjection,
    pub(crate) tx_hex: String,
}

#[derive(Debug)]
pub(crate) struct PreparedRemoteAirdrop {
    pub(crate) faucet_url: String,
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    client_nonce: String,
    airdrop_policy: groundhog_json_rpc_types::network_descriptor::AirdropDescriptor,
    amount_hog: String,
    attempt_path: PathBuf,
    _attempt_locks: Vec<File>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRemoteAirdropDryRun {
    faucet_url: String,
    rpc_url: String,
    recipient_input: String,
    recipient_wallet: String,
}

#[derive(Debug)]
enum AirdropPlan {
    Local(Box<LocalAirdropPlan>),
    Remote(Box<RemoteAirdropPlan>),
}

#[derive(Debug)]
struct LocalAirdropPlan {
    rpc_url: String,
    profile_name: String,
    recipient_input: String,
    recipient_wallet: String,
    amount_hog: String,
    raw_amount: u128,
    faucet_key_name: String,
    faucet_role: String,
    faucet_wallet: String,
    faucet_balance_height: groundhog_json_rpc_types::BlockHeight,
    funding_projection: LocalFundingProjection,
    transaction: Transaction,
    signer: LocalSigner,
    network_hash: NetworkHash,
}

#[derive(Debug)]
struct RemoteAirdropPlan {
    profile: ResolvedProfile,
    profile_name: String,
    faucet_url: String,
    rpc_url: String,
    recipient_input: String,
    recipient_wallet: String,
    descriptor: RemoteAirdropDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteAirdropAttempt {
    profile: String,
    recipient_wallet: String,
    network_id: String,
    genesis_hash: String,
    client_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoadedRemoteAirdropAttempt {
    attempt: RemoteAirdropAttempt,
    path: PathBuf,
}

#[derive(Debug)]
struct AirdropRecipient {
    input: String,
    wallet: String,
}

#[expect(clippy::too_many_arguments, reason = "CLI command boundary")]
pub fn run(
    recipient: Option<&str>,
    amount: Option<&str>,
    dry_run: bool,
    force: bool,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let resolve_profile = || resolve_profile_with_overrides(&cwd, profile_name, overrides);
    // Only a defaulted recipient needs the profile before the confirmation preflight.
    let (profile, default_recipient) = if recipient.is_some() {
        (None, None)
    } else {
        let profile = resolve_profile()?;
        let (input, address) = resolve_address_target_or_default_wallet(&profile, None)?;
        (
            Some(profile),
            Some(AirdropRecipient {
                input,
                wallet: format_contract_address(&address),
            }),
        )
    };
    if !dry_run {
        print.require_confirmation_available(force, "request an airdrop")?;
    }
    let profile = match profile {
        Some(profile) => profile,
        None => resolve_profile()?,
    };
    let recipient = if let Some(recipient) = default_recipient {
        recipient
    } else {
        let input = recipient.expect("bug: explicit or default recipient must exist");
        AirdropRecipient {
            input: input.to_owned(),
            wallet: resolve_airdrop_recipient(&profile, input)?,
        }
    };
    if dry_run && let Some(prepared) = prepare_remote_airdrop_dry_run(&recipient, amount, &profile)?
    {
        return print_remote_dry_run(&prepared, cmd, print);
    }
    let plan = prepare_airdrop_plan(profile, &recipient, amount)?;
    if !dry_run {
        print_airdrop_plan(&plan, print);
        if !print.confirm(force, "request an airdrop", "Request this airdrop?")? {
            print.note("Cancelled");
            return Ok(());
        }
    }
    let prepared = finalize_airdrop_plan(plan)?;

    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    match prepared {
        PreparedAirdrop::Local(prepared) => run_local_airdrop(&prepared, cmd, print)?,
        PreparedAirdrop::Remote(prepared) => run_remote_airdrop(&prepared, cmd, print)?,
    }

    Ok(())
}

fn prepare_remote_airdrop_dry_run(
    recipient: &AirdropRecipient,
    amount: Option<&str>,
    profile: &ResolvedProfile,
) -> Result<Option<PreparedRemoteAirdropDryRun>, CliError> {
    if !matches!(
        resolve_faucet_backend(profile, "airdrop")?,
        FaucetBackend::RemoteFaucet
    ) {
        return Ok(None);
    }
    if amount.is_some() {
        return Err(CliError::Other(
            "remote airdrop amount is set by the faucet service".into(),
        ));
    }
    let rpc_url = profile
        .rpc_url
        .as_deref()
        .ok_or(CliError::MissingRpcUrl)?
        .to_owned();
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| CliError::Other("remote airdrop requires faucet_url".into()))?
        .trim_end_matches('/')
        .to_owned();
    let recipient_wallet = recipient.wallet.clone();

    Ok(Some(PreparedRemoteAirdropDryRun {
        faucet_url,
        rpc_url,
        recipient_input: recipient.input.clone(),
        recipient_wallet,
    }))
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
    ensure_local_airdrop_success(prepared, &receipt, &tx_hash)?;
    print.status(format_args!("Airdrop sent: {tx_hash}"));
    let observations = post_airdrop_observations(prepared, print);

    let warnings = low_local_faucet_balance_warning(prepared.funding_projection.projected_balance)
        .into_iter()
        .collect();
    print.json(&CommandResult::with_warnings(
        cmd,
        warnings,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "local",
            "recipient": prepared.recipient_input,
            "recipientWallet": prepared.recipient_wallet,
            "amountHog": prepared.amount_hog,
            "rawAmount": prepared.raw_amount.to_string(),
            "faucet": {
                "key": prepared.faucet_key_name,
                "role": prepared.faucet_role,
            },
            "txHash": tx_hash,
            "height": receipt.height,
            "status": receipt.status,
            "observations": observations,
        }),
    ))
}

fn ensure_local_airdrop_success(
    prepared: &PreparedLocalAirdrop,
    receipt: &groundhog_json_rpc_types::receipt::TransactionReceipt,
    tx_hash: &str,
) -> Result<(), CliError> {
    let decoded_abort = crate::tx_utils::decode_receipt_abort(receipt, None);
    if matches!(receipt.status, ExecStatus::Reverted { .. })
        && crate::codec::is_erc20_too_large(decoded_abort.as_ref())
    {
        return Err(insufficient_local_faucet_error(
            prepared.raw_amount,
            prepared.funding_projection.estimated_max,
        ));
    }
    ensure_tx_success("airdrop", receipt, tx_hash)
}

fn print_low_faucet_warning(prepared: &LocalAirdropPlan, print: &Print) {
    let Some(projected_balance) = prepared.funding_projection.projected_balance else {
        return;
    };
    if low_local_faucet_balance_warning(Some(projected_balance)).is_none() {
        return;
    }

    print.status(format_args!(
        "Local faucet: {} ({})",
        prepared.faucet_key_name, prepared.faucet_wallet
    ));
    print.status(format_args!(
        "Committed balance: {} HOG at block {}",
        hog::format_amount(prepared.funding_projection.committed_balance),
        *prepared.faucet_balance_height
    ));
    print.status(format_args!(
        "Estimated max now: {} HOG",
        hog::format_amount(prepared.funding_projection.estimated_max)
    ));
    print.status(format_args!("Requested: {} HOG", prepared.amount_hog));
    print.status(format_args!(
        "Projected faucet balance: {} HOG",
        hog::format_amount(projected_balance)
    ));
    print.warn("this would leave the local faucet below 10 HOG");
}

fn print_airdrop_plan(plan: &AirdropPlan, print: &Print) {
    match plan {
        AirdropPlan::Local(plan) => {
            print.detail("recipient", &plan.recipient_wallet);
            print.detail("amount", format_args!("{} HOG", plan.amount_hog));
            print.detail("faucet", &plan.faucet_wallet);
            print.detail("profile", &plan.profile_name);
            print_low_faucet_warning(plan, print);
        }
        AirdropPlan::Remote(plan) => {
            print.detail("recipient", &plan.recipient_wallet);
            print.detail("amount", format_args!("{} HOG", plan.descriptor.amount_hog));
            print.detail("profile", &plan.profile_name);
            print_remote_airdrop_policy(
                &plan.descriptor.policy,
                &plan.descriptor.amount_hog,
                print,
            );
        }
    }
}

fn prepare_local_airdrop_plan(
    profile: &ResolvedProfile,
    recipient: &AirdropRecipient,
    amount: Option<&str>,
) -> Result<LocalAirdropPlan, CliError> {
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let manifest = load_configured_runtime_manifest(profile, "faucet")?
        .ok_or_else(|| CliError::Other("airdrop requires a local runtime manifest".into()))?;
    let recipient_wallet = recipient.wallet.clone();
    let raw_amount = hog::parse_amount(amount.unwrap_or(DEFAULT_AIRDROP_HOG))?;
    let faucet = resolve_local_bootstrap_account(profile, LocalBootstrapRole::Faucet)?;
    let faucet_wallet = format_contract_address(&faucet.wallet_address);
    let faucet_balance = fetch_balance_blocking(
        rpc_url,
        groundhog_json_rpc_types::ContractAddress::from(faucet.wallet_address),
    )?;
    let funding_projection = local_funding_projection(
        &faucet_balance.raw_balance,
        &manifest.bootstrap.storage_cost_model,
        raw_amount,
        1,
    )?;
    if funding_projection.projected_balance.is_none() {
        return Err(insufficient_local_faucet_error(
            raw_amount,
            funding_projection.estimated_max,
        ));
    }
    let recipient_address = parse_contract_address(&recipient_wallet)?;
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let block_expiry_height =
        runtime.block_on(async { resolve_block_expiry_height(rpc_url).await })?;
    let transfer_calldata = hog::encode_transfer_calldata(&recipient_address, raw_amount)?;
    let account = groundhog_signing::PubkeyAccount::from_public_key_bytes(
        faucet.signer.verifying_key_bytes(),
        0,
    );
    let transaction = groundhog_signing::default_wallet::build_default_wallet_call_transaction(
        groundhog_signing::default_wallet::BuildDefaultWalletCallTransaction {
            account: &account,
            target_address: hog::token_address(),
            target_entrypoint: hog::transfer_entrypoint(),
            target_calldata: &transfer_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT * 2,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height,
            nonce: nonce_from_timestamp(),
        },
    )
    .map_err(|error| CliError::Other(error.to_string()))?;
    Ok(LocalAirdropPlan {
        rpc_url: rpc_url.to_owned(),
        profile_name: profile.name.clone(),
        recipient_input: recipient.input.clone(),
        recipient_wallet,
        amount_hog: hog::format_amount(raw_amount),
        raw_amount,
        faucet_key_name: faucet.key_name,
        faucet_role: faucet.role,
        faucet_wallet,
        faucet_balance_height: faucet_balance.height,
        funding_projection,
        transaction,
        signer: faucet.signer,
        network_hash: faucet.network_hash,
    })
}

fn post_airdrop_observations(prepared: &PreparedLocalAirdrop, print: &Print) -> Value {
    json!({
        "faucet": observe_post_airdrop_balance(
            "Faucet balance",
            &prepared.rpc_url,
            &prepared.faucet_wallet,
            print,
        ),
        "recipient": observe_post_airdrop_balance(
            "Recipient balance",
            &prepared.rpc_url,
            &prepared.recipient_wallet,
            print,
        ),
    })
}

fn observe_post_airdrop_balance(label: &str, rpc_url: &str, address: &str, print: &Print) -> Value {
    let observation = parse_contract_address(address)
        .map(groundhog_json_rpc_types::ContractAddress::from)
        .and_then(|address| fetch_balance_blocking(rpc_url, address))
        .and_then(|balance| {
            let raw = balance.raw_balance.parse::<u128>().map_err(|error| {
                CliError::Other(format!(
                    "invalid raw balance `{}` from RPC: {error}",
                    balance.raw_balance
                ))
            })?;
            print.status(format_args!(
                "{label}: {} HOG at block {}",
                hog::format_amount(raw),
                *balance.height
            ));
            Ok(json!({
                "status": "observed",
                "rawBalance": balance.raw_balance,
                "balanceHog": hog::format_amount(raw),
                "height": balance.height,
            }))
        });

    observation.unwrap_or_else(|error| {
        print.warn(format_args!(
            "could not observe {label}: {}",
            crate::terminal::terminal_safe(&error.to_string())
        ));
        json!({ "status": "unavailable", "error": error.to_string() })
    })
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
    let request: AirdropRequest = serde_json::from_value(json!({
        "recipient": prepared.recipient_wallet,
        "clientNonce": prepared.client_nonce,
    }))
    .map_err(|error| CliError::Other(format!("invalid retained airdrop request: {error}")))?;
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let response = runtime.block_on(async {
        crate::http_client::rpc_client(&prepared.rpc_url)?
            .request_airdrop(request)
            .await
            .map_err(|error| CliError::RpcMethodFailed {
                method: "requestAirdrop",
                message: error.to_string(),
            })
    })?;
    let recipient = response.recipient();
    if recipient.0 != parse_contract_address(&prepared.recipient_wallet)? {
        return Err(CliError::NetworkDescriptorMismatch {
            field: "airdrop.recipient",
            expected: prepared.recipient_wallet.clone(),
            actual: format_contract_address(&recipient.0),
        });
    }
    validate_remote_airdrop_amount(&prepared.airdrop_policy.raw_amount, response.raw_amount())?;
    let (tx_hash, height, status, raw_amount) = match response {
        AirdropAttemptResponse::Pending {
            retry_after_seconds,
            ..
        } => {
            return Err(CliError::Other(format!(
                "remote airdrop is pending - retry the same command to continue; retry after {retry_after_seconds}s"
            )));
        }
        AirdropAttemptResponse::Succeeded {
            raw_amount,
            tx_hash,
            height,
            status,
            ..
        } => (tx_hash, height, status, raw_amount),
        _ => {
            return Err(CliError::Other(
                "remote faucet returned an unsupported airdrop attempt status".into(),
            ));
        }
    };
    if !matches!(status, ExecStatus::Ok) {
        return Err(CliError::Other(
            "remote airdrop transaction did not commit successfully".into(),
        ));
    }
    let tx_hash = format_tx_hash(&tx_hash);
    remove_remote_airdrop_attempt(prepared)?;
    print.status(format_args!("Airdrop sent: {tx_hash}"));

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipientWallet": prepared.recipient_wallet,
            "faucetUrl": prepared.faucet_url,
            "rpcUrl": prepared.rpc_url,
            "rawAmount": raw_amount,
            "policy": {
                "rawAmount": prepared.airdrop_policy.raw_amount,
                "amountHog": prepared.amount_hog,
                "recipientCooldownSeconds": prepared.airdrop_policy.recipient_cooldown_seconds,
                "clientQuotaMax": prepared.airdrop_policy.client_quota_max,
                "clientQuotaWindowSeconds": prepared.airdrop_policy.client_quota_window_seconds,
                "maxPendingPerRecipient": prepared.airdrop_policy.max_pending_per_recipient,
            },
            "txHash": tx_hash,
            "height": height,
            "status": status,
        }),
    ))
}

fn validate_remote_airdrop_amount(expected: &str, actual: &str) -> Result<(), CliError> {
    let expected_amount = expected.parse::<u128>();
    let actual_amount = actual.parse::<u128>();
    if matches!((expected_amount, actual_amount), (Ok(expected), Ok(actual)) if expected == actual)
    {
        return Ok(());
    }
    Err(CliError::NetworkDescriptorMismatch {
        field: "airdrop.rawAmount",
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    })
}

fn print_remote_airdrop_policy(
    policy: &groundhog_json_rpc_types::network_descriptor::AirdropDescriptor,
    amount_hog: &str,
    print: &Print,
) {
    print.status(format_args!("Fixed airdrop amount: {amount_hog} HOG"));
    print.status(format_args!(
        "Recipient cooldown: {} seconds",
        policy.recipient_cooldown_seconds
    ));
    print.status(format_args!(
        "Client/IP quota: {} per {} seconds",
        policy.client_quota_max, policy.client_quota_window_seconds
    ));
    print.status(format_args!(
        "Pending requests per recipient: {}",
        policy.max_pending_per_recipient
    ));
}

fn prepare_airdrop_plan(
    profile: ResolvedProfile,
    recipient: &AirdropRecipient,
    amount: Option<&str>,
) -> Result<AirdropPlan, CliError> {
    match resolve_faucet_backend(&profile, "airdrop")? {
        FaucetBackend::RemoteFaucet => {
            if amount.is_some() {
                return Err(CliError::Other(
                    "remote airdrop amount is set by the faucet service".into(),
                ));
            }
            prepare_remote_airdrop_plan(profile, recipient)
        }
        FaucetBackend::LocalManifest => prepare_local_airdrop_plan(&profile, recipient, amount)
            .map(Box::new)
            .map(AirdropPlan::Local),
    }
}

fn prepare_remote_airdrop_plan(
    profile: ResolvedProfile,
    recipient: &AirdropRecipient,
) -> Result<AirdropPlan, CliError> {
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
    let recipient_wallet = recipient.wallet.clone();
    let descriptor = fetch_remote_airdrop_descriptor(&profile)?;
    Ok(AirdropPlan::Remote(Box::new(RemoteAirdropPlan {
        profile_name: profile.name.clone(),
        profile,
        recipient_input: recipient.input.clone(),
        recipient_wallet,
        faucet_url,
        rpc_url,
        descriptor,
    })))
}

fn finalize_airdrop_plan(plan: AirdropPlan) -> Result<PreparedAirdrop, CliError> {
    match plan {
        AirdropPlan::Local(plan) => {
            let tx_hex = sign_and_encode_transaction_hex(
                plan.transaction,
                &plan.signer,
                SignTransactionParams::default(),
                &plan.network_hash,
            )
            .map_err(|error| CliError::Other(error.to_string()))?;
            Ok(PreparedAirdrop::Local(PreparedLocalAirdrop {
                rpc_url: plan.rpc_url,
                recipient_input: plan.recipient_input,
                recipient_wallet: plan.recipient_wallet,
                amount_hog: plan.amount_hog,
                raw_amount: plan.raw_amount,
                faucet_key_name: plan.faucet_key_name,
                faucet_role: plan.faucet_role,
                faucet_wallet: plan.faucet_wallet,
                faucet_balance_height: plan.faucet_balance_height,
                funding_projection: plan.funding_projection,
                tx_hex,
            }))
        }
        AirdropPlan::Remote(plan) => prepare_remote_airdrop_with_descriptor(
            &plan.profile,
            &plan.recipient_input,
            plan.recipient_wallet,
            plan.faucet_url,
            plan.rpc_url,
            &plan.descriptor,
        ),
    }
}

fn prepare_remote_airdrop_with_descriptor(
    profile: &ResolvedProfile,
    recipient: &str,
    recipient_wallet: String,
    faucet_url: String,
    rpc_url: String,
    descriptor: &RemoteAirdropDescriptor,
) -> Result<PreparedAirdrop, CliError> {
    let (loaded_attempt, attempt_locks) =
        load_or_create_remote_airdrop_attempt(profile, &recipient_wallet, descriptor)?;
    Ok(PreparedAirdrop::Remote(PreparedRemoteAirdrop {
        faucet_url,
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        client_nonce: loaded_attempt.attempt.client_nonce,
        airdrop_policy: descriptor.policy.clone(),
        amount_hog: descriptor.amount_hog.clone(),
        attempt_path: loaded_attempt.path,
        _attempt_locks: attempt_locks,
    }))
}

fn load_or_create_remote_airdrop_attempt(
    profile: &ResolvedProfile,
    recipient_wallet: &str,
    descriptor: &RemoteAirdropDescriptor,
) -> Result<(LoadedRemoteAirdropAttempt, Vec<File>), CliError> {
    let network_id = descriptor.network_id.clone();
    let genesis_hash = descriptor.genesis_hash.clone();
    let path =
        remote_airdrop_attempt_path(&profile.name, recipient_wallet, &network_id, &genesis_hash)?;
    let mut attempt_locks = vec![lock_remote_airdrop_attempt(&path)?];
    let legacy_paths = find_legacy_remote_airdrop_attempt_paths(
        &path,
        &profile.name,
        recipient_wallet,
        &network_id,
        &genesis_hash,
    )?;
    for legacy_path in &legacy_paths {
        attempt_locks.push(lock_remote_airdrop_attempt(legacy_path)?);
    }

    let mut attempts = Vec::new();
    for candidate_path in std::iter::once(&path).chain(legacy_paths.iter()) {
        if let Some(attempt) = read_attempt(candidate_path)?
            && remote_airdrop_attempt_matches(
                &attempt,
                &profile.name,
                recipient_wallet,
                &network_id,
                &genesis_hash,
            )
        {
            attempts.push(attempt);
        }
    }
    if let Some(mut attempt) = attempts.pop() {
        if attempts
            .iter()
            .any(|other| other.client_nonce != attempt.client_nonce)
        {
            return Err(CliError::Other(format!(
                "multiple pending airdrop attempts exist for profile `{}`, recipient `{recipient_wallet}`, and network `{network_id}`; refusing to generate another nonce",
                profile.name
            )));
        }
        recipient_wallet.clone_into(&mut attempt.recipient_wallet);
        write_attempt(&path, &attempt)?;
        for legacy_path in &legacy_paths {
            remove_attempt(legacy_path)?;
        }
        return Ok((LoadedRemoteAirdropAttempt { attempt, path }, attempt_locks));
    }

    let mut client_nonce = [0u8; 16];
    getrandom(&mut client_nonce).map_err(|error| CliError::Other(error.to_string()))?;
    let attempt = RemoteAirdropAttempt {
        profile: profile.name.clone(),
        recipient_wallet: recipient_wallet.to_owned(),
        network_id,
        genesis_hash,
        client_nonce: format!("0x{}", hex::encode(client_nonce)),
    };
    write_attempt(&path, &attempt)?;
    Ok((LoadedRemoteAirdropAttempt { attempt, path }, attempt_locks))
}

fn find_legacy_remote_airdrop_attempt_paths(
    canonical_path: &Path,
    profile: &str,
    recipient_wallet: &str,
    network_id: &str,
    genesis_hash: &str,
) -> Result<Vec<PathBuf>, CliError> {
    let parent = canonical_path
        .parent()
        .ok_or_else(|| CliError::Other("airdrop attempt path has no parent".into()))?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let path = entry?.path();
        if path == canonical_path
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(attempt) = serde_json::from_str(&contents) else {
            continue;
        };
        if remote_airdrop_attempt_matches(
            &attempt,
            profile,
            recipient_wallet,
            network_id,
            genesis_hash,
        ) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn remote_airdrop_attempt_matches(
    attempt: &RemoteAirdropAttempt,
    profile: &str,
    recipient_wallet: &str,
    network_id: &str,
    genesis_hash: &str,
) -> bool {
    attempt.profile == profile
        && attempt.network_id == network_id
        && attempt.genesis_hash == genesis_hash
        && parse_contract_address(&attempt.recipient_wallet)
            .is_ok_and(|address| format_contract_address(&address) == recipient_wallet)
}

fn lock_remote_airdrop_attempt(attempt_path: &Path) -> Result<File, CliError> {
    lock_attempt(attempt_path, "airdrop", "profile, recipient, and network")
}

fn remove_remote_airdrop_attempt(prepared: &PreparedRemoteAirdrop) -> Result<(), CliError> {
    remove_attempt(&prepared.attempt_path)
}

fn remote_airdrop_attempt_path(
    profile: &str,
    recipient_wallet: &str,
    network_id: &str,
    genesis_hash: &str,
) -> Result<PathBuf, CliError> {
    attempt_path(
        "airdrop-attempts",
        &[profile, recipient_wallet, network_id, genesis_hash],
    )
}

fn resolve_airdrop_recipient(
    profile: &ResolvedProfile,
    recipient: &str,
) -> Result<String, CliError> {
    match resolve_address_target(profile, recipient) {
        Ok(address) => Ok(format_contract_address(&address)),
        Err(CliError::InvalidAddressTarget { .. }) => Err(CliError::Other(
            "wallet address must be a C StrKey or 64 hex characters, optionally prefixed with 0x"
                .into(),
        )),
        Err(CliError::UnknownAddressTarget { .. }) => Err(CliError::Other(format!(
            "unknown wallet `{}`; run `groundhog wallet create --key={}` or pass a C StrKey or 0x-prefixed wallet address",
            crate::terminal::terminal_safe(recipient),
            crate::terminal::render_shell_arg(recipient),
        ))),
        Err(error) => Err(error),
    }
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
                    "dryRun": true,
                    "submitted": false,
                    "mode": "local",
                    "recipient": prepared.recipient_input,
                    "recipientWallet": prepared.recipient_wallet,
                    "amountHog": prepared.amount_hog,
                    "rawAmount": prepared.raw_amount.to_string(),
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
                    "dryRun": true,
                    "submitted": false,
                    "mode": "remote",
                    "recipient": prepared.recipient_input,
                    "recipientWallet": prepared.recipient_wallet,
                    "faucetUrl": prepared.faucet_url,
                    "rpcUrl": prepared.rpc_url,
                    "plannedRequests": [
                        "groundhog_requestAirdrop"
                    ],
                }),
            ))?;
        }
    }

    Ok(())
}

fn print_remote_dry_run(
    prepared: &PreparedRemoteAirdropDryRun,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
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
            "dryRun": true,
            "submitted": false,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipientWallet": prepared.recipient_wallet,
            "faucetUrl": prepared.faucet_url,
            "rpcUrl": prepared.rpc_url,
            "plannedRequests": [
                "GET /v1/network",
                "groundhog_requestAirdrop"
            ],
            "idempotencyKey": "profile + recipient + networkId + genesisHash + clientNonce",
        }),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::env;
    use std::ffi::OsString;
    use std::path::Path;

    use groundhog_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
    use serial_test::serial;

    use super::{
        PreparedAirdrop, PreparedLocalAirdrop, RemoteAirdropAttempt, ensure_local_airdrop_success,
        lock_remote_airdrop_attempt, observe_post_airdrop_balance,
        prepare_remote_airdrop_with_descriptor, remote_airdrop_attempt_path,
        resolve_airdrop_recipient, validate_remote_airdrop_amount,
    };
    use crate::config::{Network, ResolvedProfile, SavedWalletBinding};
    use crate::hog;
    use crate::profile_backend::{FaucetBackend, resolve_faucet_backend};
    use crate::remote_attempt::write_attempt;

    const ATTEMPT_LOCK_TEST_PATH: &str = "GROUNDHOG_ATTEMPT_LOCK_TEST_PATH";
    const ATTEMPT_LOCK_TEST_MODE: &str = "GROUNDHOG_ATTEMPT_LOCK_TEST_MODE";

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set_str(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                unsafe { std::env::set_var(self.key, previous) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    #[test]
    fn resolves_saved_or_direct_recipient() {
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: format!("0x{}", "aB".repeat(32)),
                owner_public_key: format!("0x{}", "ef".repeat(32)),
            },
        );
        let profile = test_profile(wallets);

        assert_eq!(
            resolve_airdrop_recipient(&profile, "alice").unwrap(),
            format!("0x{}", "ab".repeat(32))
        );
        assert_eq!(
            resolve_airdrop_recipient(&profile, &format!("0x{}", "cD".repeat(32))).unwrap(),
            format!("0x{}", "cd".repeat(32))
        );
        assert!(resolve_airdrop_recipient(&profile, "missing").is_err());
    }

    #[test]
    #[serial]
    fn recipient_hex_casing_selects_the_same_attempt_path() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let lower = resolve_airdrop_recipient(&profile, &format!("0x{}", "ab".repeat(32)))
            .expect("bug: lowercase recipient should resolve");
        let mixed = resolve_airdrop_recipient(&profile, &format!("0x{}", "aB".repeat(32)))
            .expect("bug: mixed-case recipient should resolve");

        assert_eq!(lower, mixed);
        assert_eq!(
            remote_airdrop_attempt_path(&profile.name, &lower, "devnet", "0xgenesis").unwrap(),
            remote_airdrop_attempt_path(&profile.name, &mixed, "devnet", "0xgenesis").unwrap()
        );
    }

    #[test]
    #[serial]
    fn migrates_mixed_case_attempt_without_changing_nonce() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let descriptor = test_network_descriptor();
        let legacy_recipient = format!("0x{}", "aB".repeat(32));
        let current_recipient = format!("0x{}", "Ab".repeat(32));
        let recipient_wallet = resolve_airdrop_recipient(&profile, &current_recipient)
            .expect("bug: mixed-case recipient should resolve");
        let legacy_path = remote_airdrop_attempt_path(
            &profile.name,
            &legacy_recipient,
            &descriptor.network_id,
            &descriptor.genesis_hash,
        )
        .unwrap();
        let client_nonce = format!("0x{}", "11".repeat(16));
        write_attempt(
            &legacy_path,
            &RemoteAirdropAttempt {
                profile: profile.name.clone(),
                recipient_wallet: legacy_recipient.clone(),
                network_id: descriptor.network_id.clone(),
                genesis_hash: descriptor.genesis_hash.clone(),
                client_nonce: client_nonce.clone(),
            },
        )
        .unwrap();

        let prepared = prepare_remote_airdrop_with_descriptor(
            &profile,
            &current_recipient,
            recipient_wallet.clone(),
            "http://127.0.0.1:8081".into(),
            "http://127.0.0.1:9545".into(),
            &descriptor,
        )
        .expect("bug: legacy attempt should migrate");
        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };

        assert_eq!(prepared.client_nonce, client_nonce);
        assert_eq!(prepared.recipient_wallet, recipient_wallet);
        assert!(!legacy_path.exists());
        assert!(prepared.attempt_path.exists());
    }

    #[test]
    #[serial]
    fn cleans_legacy_copy_left_after_attempt_migration() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let descriptor = test_network_descriptor();
        let legacy_recipient = format!("0x{}", "aB".repeat(32));
        let recipient_wallet = resolve_airdrop_recipient(&profile, &legacy_recipient).unwrap();
        let canonical_path = remote_airdrop_attempt_path(
            &profile.name,
            &recipient_wallet,
            &descriptor.network_id,
            &descriptor.genesis_hash,
        )
        .unwrap();
        let legacy_path = remote_airdrop_attempt_path(
            &profile.name,
            &legacy_recipient,
            &descriptor.network_id,
            &descriptor.genesis_hash,
        )
        .unwrap();
        let client_nonce = format!("0x{}", "11".repeat(16));
        for (path, stored_recipient) in [
            (&canonical_path, &recipient_wallet),
            (&legacy_path, &legacy_recipient),
        ] {
            write_attempt(
                path,
                &RemoteAirdropAttempt {
                    profile: profile.name.clone(),
                    recipient_wallet: stored_recipient.clone(),
                    network_id: descriptor.network_id.clone(),
                    genesis_hash: descriptor.genesis_hash.clone(),
                    client_nonce: client_nonce.clone(),
                },
            )
            .unwrap();
        }

        let prepared = prepare_remote_airdrop_with_descriptor(
            &profile,
            &recipient_wallet,
            recipient_wallet.clone(),
            "http://127.0.0.1:8081".into(),
            "http://127.0.0.1:9545".into(),
            &descriptor,
        )
        .expect("bug: duplicate copy with the same nonce should be cleaned up");
        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };

        assert_eq!(prepared.client_nonce, client_nonce);
        assert!(canonical_path.exists());
        assert!(!legacy_path.exists());
    }

    #[test]
    fn malformed_direct_recipient_reports_address_format() {
        let profile = test_profile(BTreeMap::new());

        let err = resolve_airdrop_recipient(&profile, "0xdeadbeef")
            .expect_err("bug: malformed direct address should fail");

        assert!(
            err.to_string()
                .contains("C StrKey or 64 hex characters, optionally prefixed with 0x")
        );
    }

    #[test]
    fn unknown_recipient_is_terminal_safe_and_shell_quoted_in_guidance() {
        let profile = test_profile(BTreeMap::new());
        let err = resolve_airdrop_recipient(&profile, "alice; touch /tmp/pwned\n\u{1b}[31m")
            .expect_err("bug: unknown recipient should fail");
        let message = err.to_string();

        assert!(!message.contains('\n'));
        assert!(!message.contains('\u{1b}'));
        assert!(
            message
                .contains(r"groundhog wallet create --key='alice; touch /tmp/pwned\n\u{1b}[31m'")
        );
    }

    #[test]
    #[serial]
    fn prepares_remote_airdrop_from_profile_faucet() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        unsafe { env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let mut wallets = BTreeMap::new();
        wallets.insert(
            "alice".into(),
            SavedWalletBinding {
                kind: crate::config::WalletKind::DevWallet,
                account_nonce: None,
                address: format!("0x{}", "ab".repeat(32)),
                owner_public_key: format!("0x{}", "ef".repeat(32)),
            },
        );
        let mut profile = test_profile(wallets);
        profile.faucet_url = Some("http://127.0.0.1:8081/".into());
        let recipient_wallet = resolve_airdrop_recipient(&profile, "alice").unwrap();

        let prepared = prepare_remote_airdrop_with_descriptor(
            &profile,
            "alice",
            recipient_wallet,
            "http://127.0.0.1:8081".into(),
            "http://127.0.0.1:9545".into(),
            &test_network_descriptor(),
        )
        .unwrap();

        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };
        assert_eq!(prepared.faucet_url, "http://127.0.0.1:8081");
        assert_eq!(prepared.rpc_url, "http://127.0.0.1:9545");
        assert_eq!(prepared.recipient_input, "alice");
        assert_eq!(prepared.recipient_wallet, format!("0x{}", "ab".repeat(32)));
        assert_eq!(prepared.client_nonce.len(), 34);
        unsafe { env::remove_var("XDG_CACHE_HOME") };
    }

    #[test]
    #[serial]
    fn concurrent_remote_airdrop_lifecycle_is_rejected_across_processes() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let attempt_path = tmp.path().join("attempt.json");
        let first = lock_remote_airdrop_attempt(&attempt_path)
            .expect("bug: first process should acquire attempt lock");
        let blocked = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "airdrop::tests::remote_airdrop_lock_subprocess"])
            .env(ATTEMPT_LOCK_TEST_PATH, &attempt_path)
            .env(ATTEMPT_LOCK_TEST_MODE, "blocked")
            .status()
            .expect("bug: lock test subprocess should start");
        assert!(blocked.success());

        drop(first);
        let available = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "airdrop::tests::remote_airdrop_lock_subprocess"])
            .env(ATTEMPT_LOCK_TEST_PATH, &attempt_path)
            .env(ATTEMPT_LOCK_TEST_MODE, "available")
            .status()
            .expect("bug: lock test subprocess should start");
        assert!(available.success());
    }

    #[test]
    fn remote_airdrop_lock_subprocess() {
        let Some(path) = std::env::var_os(ATTEMPT_LOCK_TEST_PATH) else {
            return;
        };
        let result = lock_remote_airdrop_attempt(Path::new(&path));
        match std::env::var(ATTEMPT_LOCK_TEST_MODE).as_deref() {
            Ok("blocked") => assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("another airdrop command")
            ),
            Ok("available") => {
                result.expect("lock should be released when the first process exits");
            }
            mode => panic!("unexpected attempt lock test mode: {mode:?}"),
        }
    }

    #[test]
    #[serial]
    fn airdrop_devnet_missing_faucet_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);

        let error = resolve_faucet_backend(&profile, "airdrop")
            .expect_err("bug: missing devnet faucet should fail");

        assert_eq!(
            error.to_string(),
            "profile `devnet` targets devnet, but airdrop requires `faucet_url`"
        );
    }

    #[test]
    #[serial]
    fn airdrop_devnet_missing_rpc_reports_devnet_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);
        profile.rpc_url = None;
        profile.faucet_url = Some("http://127.0.0.1:8081".into());

        let error = resolve_faucet_backend(&profile, "airdrop")
            .expect_err("bug: missing devnet rpc should fail");

        assert_eq!(
            error.to_string(),
            "profile `devnet` targets devnet, but airdrop requires `rpc_url`"
        );
    }

    #[test]
    #[serial]
    fn airdrop_devnet_with_urls_uses_remote_faucet() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "devnet".into();
        profile.network = Some(Network::Devnet);
        profile.faucet_url = Some("http://127.0.0.1:8081".into());

        let backend = resolve_faucet_backend(&profile, "airdrop")
            .expect("bug: configured devnet profile should resolve");

        assert_eq!(backend, FaucetBackend::RemoteFaucet);
    }

    #[test]
    #[serial]
    fn airdrop_local_missing_manifest_reports_local_error() {
        let _manifest = EnvGuard::set_str("GROUNDHOG_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "local".into();
        profile.network = Some(Network::Local);

        let error = resolve_faucet_backend(&profile, "airdrop")
            .expect_err("bug: missing local manifest should fail");

        assert_eq!(
            error.to_string(),
            "profile `local` targets local, but airdrop requires a local runtime manifest; run `groundhog node` or configure `runtime_manifest`"
        );
    }

    #[test]
    fn hog_transfer_entrypoint_matches_standard_erc20_interface() {
        let entrypoint = contract_entrypoint_id(
            Some("ERC20"),
            "0.1.3",
            &NormalizedMethodSignature::new("transfer"),
        );

        assert_eq!(entrypoint.as_ref(), hog::transfer_entrypoint().as_ref());
    }

    #[test]
    #[serial]
    fn remote_airdrop_requires_additive_policy_at_airdrop_boundary() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let mut value = test_network_descriptor_value();
        value.as_object_mut().unwrap().remove("airdrop");
        let error = crate::network_descriptor::parse_remote_airdrop_descriptor(
            "http://127.0.0.1:9545",
            value,
        )
        .expect_err("bug: remote airdrop needs advertised policy");

        assert!(matches!(
            error,
            crate::CliError::NetworkDescriptorInvalid {
                field: "airdrop",
                ..
            }
        ));
    }

    #[test]
    fn failed_post_airdrop_observation_is_structured_and_nonfatal() {
        let observation = observe_post_airdrop_balance(
            "Recipient balance",
            "http://127.0.0.1:9545",
            "invalid",
            &crate::Print::new(false, true),
        );

        assert_eq!(observation["status"], "unavailable");
        assert!(observation["error"].as_str().is_some());
    }

    #[test]
    fn committed_too_large_airdrop_reports_actionable_local_faucet_error() {
        let prepared = prepared_local_airdrop();
        let receipt = reverted_too_large_receipt();

        let error = ensure_local_airdrop_success(&prepared, &receipt, "0x44")
            .expect_err("bug: committed TooLarge should map to faucet funding guidance");

        assert!(
            error
                .to_string()
                .contains("insufficient actionable balance")
        );
        assert!(error.to_string().contains("start a fresh local chain"));
    }

    #[test]
    fn non_reverted_too_large_airdrop_preserves_transaction_status() {
        let prepared = prepared_local_airdrop();
        let mut receipt = reverted_too_large_receipt();
        receipt.status = groundhog_json_rpc_types::receipt::ExecStatus::DeterministicError {
            kind: "outOfGas".into(),
        };

        let error = ensure_local_airdrop_success(&prepared, &receipt, "0x44")
            .expect_err("bug: deterministic error should remain a transaction failure");

        assert!(
            !error
                .to_string()
                .contains("insufficient actionable balance")
        );
        assert!(error.to_string().contains("deterministic execution error"));
    }

    #[test]
    fn remote_airdrop_rejects_amount_different_from_advertised_policy() {
        let error = validate_remote_airdrop_amount("100000000", "200000000")
            .expect_err("bug: mismatched service amount should fail");

        assert_eq!(error.code(), "network_descriptor_mismatch");
        assert!(error.to_string().contains("airdrop.rawAmount"));
    }

    #[test]
    fn remote_airdrop_compares_raw_amounts_numerically() {
        validate_remote_airdrop_amount("0100000000", "100000000")
            .expect("bug: equivalent raw amounts should match");
    }

    #[test]
    fn remote_airdrop_rejects_malformed_response_amount() {
        let error = validate_remote_airdrop_amount("100000000", "invalid")
            .expect_err("bug: malformed service amount should fail");

        assert_eq!(error.code(), "network_descriptor_mismatch");
    }

    fn prepared_local_airdrop() -> PreparedLocalAirdrop {
        PreparedLocalAirdrop {
            rpc_url: String::new(),
            recipient_input: "alice".into(),
            recipient_wallet: format!("0x{}", "ab".repeat(32)),
            amount_hog: "10".into(),
            raw_amount: 100_000_000,
            faucet_key_name: "dev-0".into(),
            faucet_role: "faucet".into(),
            faucet_wallet: format!("0x{}", "cd".repeat(32)),
            faucet_balance_height: groundhog_json_rpc_types::BlockHeight::from(42),
            funding_projection: crate::balance::LocalFundingProjection {
                committed_balance: 200_000_000,
                transaction_storage_cost: 12_007,
                estimated_max: 199_987_993,
                projected_balance: Some(99_987_993),
            },
            tx_hex: String::new(),
        }
    }

    fn reverted_too_large_receipt() -> groundhog_json_rpc_types::receipt::TransactionReceipt {
        groundhog_json_rpc_types::receipt::TransactionReceipt {
            tx_hash: groundhog_json_rpc_types::transaction::TxHash(
                groundhog_json_rpc_types::hex::Hex0x(groundhog_xdr::types::SHA256Hash::from(
                    [0x44; 32],
                )),
            ),
            height: groundhog_json_rpc_types::BlockHeight::from(43),
            status: groundhog_json_rpc_types::receipt::ExecStatus::Reverted { message: None },
            return_data: None,
            abort_data: Some(groundhog_json_rpc_types::hex::HexBytes(
                erc20_too_large_abort(),
            )),
            events: Vec::new(),
            gas_used: 1_500,
        }
    }

    fn erc20_too_large_abort() -> Vec<u8> {
        let selector = groundhog_contract_util::exception_selector(
            "0.1.9",
            "groundhog-erc20-if",
            "ERC20Error",
        );
        let mut bytes = xdrs::cxs::encode(&selector).expect("bug: selector should encode");
        bytes.extend_from_slice(
            &xdrs::cxs::encode(&groundhog_erc20_if::ERC20Error::TooLarge)
                .expect("bug: ERC20 error should encode"),
        );
        bytes
    }

    fn test_profile(wallets: BTreeMap<String, SavedWalletBinding>) -> ResolvedProfile {
        ResolvedProfile {
            name: "default".into(),
            network: None,
            rpc_url: Some("http://127.0.0.1:9545".into()),
            network_hash: None,
            rpc_url_cli_override: false,
            faucet_url: None,
            default_key: Some("dev-0".into()),
            default_wallet: None,
            runtime_manifest: None,
            runtime_manifest_cli_override: false,
            runtime_artifacts_dir: None,
            contracts: BTreeMap::default(),
            wallets,
            source: None,
        }
    }

    fn test_network_descriptor() -> crate::network_descriptor::RemoteAirdropDescriptor {
        crate::network_descriptor::parse_remote_airdrop_descriptor(
            "http://127.0.0.1:9545",
            test_network_descriptor_value(),
        )
        .expect("bug: test network descriptor should validate")
    }

    fn test_network_descriptor_value() -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": groundhog_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet-preview",
            "networkHash": format!("0x{}", "42".repeat(32)),
            "protocol": "x23",
            "genesisHash": format!("0x{}", "01".repeat(32)),
            "endpoints": { "rpcUrl": "http://127.0.0.1:9545" },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "02".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": { "walletAddress": format!("0x{}", "04".repeat(32)) },
            "airdrop": {
                "rawAmount": "100000000",
                "recipientCooldownSeconds": 43200,
                "clientQuotaMax": 10,
                "clientQuotaWindowSeconds": 3600,
                "maxPendingPerRecipient": 1
            },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "03".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0xf969542572bd6860",
                "doActionEntrypoint": "0xa0fff94c64fbaf3c"
            }
        })
    }
}
