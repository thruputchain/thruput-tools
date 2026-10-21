use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thruput_client::{NativeClient, ReceiptPollConfig};
use thruput_json_rpc_client::native::{NativeClientError, receipt::poll_receipt};
use thruput_json_rpc_types::{
    airdrop::{FundAccountRequest, LocalFundingErrorData},
    error::native::{DecodedError, ErrorData},
    requests,
    responses::{AirdropAttempt, AirdropExecution, ReceiptExecution, TransactionReceipt},
};
use thruput_xdr::types::NetworkHash;

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
use crate::tx_utils::{ensure_tx_success, format_tx_hash};
use crate::{CliError, Print};

/// Schema version of the `airdrop --json` data payload.
pub const AIRDROP_JSON_SCHEMA_VERSION: u32 = 5;
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
    pub(crate) faucet_role: String,
    pub(crate) faucet_wallet: String,
    pub(crate) faucet_balance_height: thruput_json_rpc_types::BlockHeight,
    pub(crate) funding_projection: LocalFundingProjection,
    pub(crate) network_hash: NetworkHash,
}

#[derive(Debug)]
pub(crate) struct PreparedRemoteAirdrop {
    pub(crate) faucet_url: String,
    pub(crate) rpc_url: String,
    pub(crate) recipient_input: String,
    pub(crate) recipient_wallet: String,
    client_nonce: String,
    network_hash: thruput_json_rpc_types::NetworkHash,
    airdrop_policy: thruput_json_rpc_types::network_descriptor::AirdropDescriptor,
    amount_hog: String,
    attempt_path: PathBuf,
    _attempt_lock: File,
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
    faucet_role: String,
    faucet_wallet: String,
    faucet_balance_height: thruput_json_rpc_types::BlockHeight,
    funding_projection: LocalFundingProjection,
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
    if !dry_run
        && !print.confirm_review(
            "Review airdrop",
            force,
            "request an airdrop",
            "Request this airdrop?",
            |print| print_airdrop_plan(&plan, print),
        )?
    {
        print.cancelled();
        return Ok(());
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
    let rpc_url = profile.rpc_endpoint()?.to_owned();
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalFundingAttempt {
    rpc_url: String,
    network_hash: String,
    request: FundAccountRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transaction: Option<LocalFundingErrorData>,
}

fn local_funding_attempt_path(
    network_hash: &str,
    faucet: &str,
    recipient: &str,
) -> Result<PathBuf, CliError> {
    attempt_path("local-funding", &[network_hash, faucet, recipient])
}

fn retain_local_funding_request(
    prepared: &PreparedLocalAirdrop,
) -> Result<(PathBuf, File, FundAccountRequest), CliError> {
    let network_hash = thruput_encoding::encode(prepared.network_hash.as_ref());
    let path = local_funding_attempt_path(
        &network_hash,
        &prepared.faucet_wallet,
        &prepared.recipient_wallet,
    )?;
    let lock = lock_attempt(&path, "airdrop", "recipient and local chain")?;
    let recipient = parse_contract_address(&prepared.recipient_wallet)?.into();
    let faucet = parse_contract_address(&prepared.faucet_wallet)?.into();
    if let Some(mut attempt) = read_attempt::<LocalFundingAttempt>(&path)? {
        if attempt.network_hash != network_hash
            || attempt.request.recipient != recipient
            || attempt.request.faucet != faucet
            || attempt.request.raw_amount != prepared.raw_amount.to_string()
        {
            return Err(CliError::LocalFundingAttempt("a retained request has different parameters; reconcile that request before changing the amount".into()));
        }
        if attempt.rpc_url != prepared.rpc_url {
            prepared.rpc_url.clone_into(&mut attempt.rpc_url);
            write_attempt(&path, &attempt)?;
        }
        return Ok((path, lock, attempt.request));
    }
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| CliError::LocalFundingAttempt(error.to_string()))?;
    let request = FundAccountRequest {
        recipient,
        faucet,
        raw_amount: prepared.raw_amount.to_string(),
        client_nonce: thruput_json_rpc_types::airdrop::ClientNonce::new(nonce.to_vec())
            .map_err(|error| CliError::LocalFundingAttempt(error.to_string()))?,
    };
    write_attempt(
        &path,
        &LocalFundingAttempt {
            rpc_url: prepared.rpc_url.clone(),
            network_hash,
            request: request.clone(),
            transaction: None,
        },
    )?;
    Ok((path, lock, request))
}

fn run_local_airdrop(
    prepared: &PreparedLocalAirdrop,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let task = crate::print::progress::Task::start(print, "Requesting airdrop", true);
    task.print().phase(format_args!(
        "Airdropping {} HOG to {}...",
        prepared.amount_hog, prepared.recipient_wallet
    ));
    let (path, _lock, request) = retain_local_funding_request(prepared)?;
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let (tx_hash, receipt) = runtime
        .block_on(request_local_airdrop(prepared, &path, &request))
        .inspect_err(|error| print_funding_recovery(task.print(), error))?;
    // A receipt closes this attempt even when execution failed. Uncertain outcomes keep it.
    remove_attempt(&path)?;
    let tx_hash = format_tx_hash(&tx_hash);
    ensure_local_airdrop_success(prepared, &receipt, &tx_hash)?;
    let _ = task.finish();
    print.group(
        crate::print::Outcome::Success,
        "Airdrop complete",
        |print| {
            print.status(format_args!("Airdrop sent: {tx_hash}"));
            if print.has_human_ui() {
                print.detail("Recipient", &prepared.recipient_wallet);
                print.detail("Amount", format_args!("{} HOG", prepared.amount_hog));
                print.detail("Height", *receipt.block_height);
            }
        },
    );
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
                "address": prepared.faucet_wallet,
                "role": prepared.faucet_role,
            },
            "txHash": tx_hash,
            "height": receipt.block_height.to_string(),
            "execution": receipt.execution,
            "observations": observations,
        }),
    ))
}

async fn request_local_airdrop(
    prepared: &PreparedLocalAirdrop,
    path: &Path,
    request: &FundAccountRequest,
) -> Result<
    (
        thruput_json_rpc_types::transaction::TxHash,
        TransactionReceipt,
    ),
    CliError,
> {
    let mut cancelled = crate::native_rpc::install_interrupt_listener().await;
    let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
    tokio::select! {
        biased;
        () = &mut cancelled => return Err(funding_cancelled(path, "notSent")),
        result = funding_preflight(&client, &prepared.network_hash, "dev_fundAccount") => result?,
    }
    let funding_request = requests::FundAccountRequest {
        recipient: request.recipient,
        faucet: request.faucet,
        raw_amount: prepared.raw_amount,
        client_nonce: request.client_nonce.clone(),
    };
    let response = tokio::select! {
        biased;
        () = &mut cancelled => return Err(funding_cancelled(path, "uncertain")),
        result = client.fund_account(funding_request) => result,
    };
    if let Err(error) = &response {
        retain_native_funding_error(path, error).map_err(|retention_error| {
            let mut evidence = crate::native_rpc::client_evidence(error);
            evidence["retentionError"] = json!(retention_error.to_string());
            funding_recovery_error(
                path,
                &crate::native_rpc::failure(error.to_string(), evidence),
            )
        })?;
    }
    let response = crate::native_rpc::known("dev_fundAccount", response)
        .map_err(|error| funding_recovery_error(path, &error))?;
    let raw_response = json!(response);
    let response_error = |error: CliError| {
        funding_recovery_error(
            path,
            &crate::native_rpc::failure(error.to_string(), json!({"result": raw_response})),
        )
    };
    if response.recipient != request.recipient || response.raw_amount != prepared.raw_amount {
        return Err(response_error(CliError::LocalFundingAttempt(
            "funding response does not match the retained request".into(),
        )));
    }
    // Retain acknowledged evidence before the next await or fallible observation.
    retain_local_funding_evidence(
        path,
        &LocalFundingErrorData::new(response.transaction_hash, response.signed_transaction),
    )
    .map_err(response_error)?;
    let receipt = poll_receipt(
        &client,
        prepared.network_hash.clone().into_inner().into(),
        response.transaction_hash,
        None,
        ReceiptPollConfig::default(),
        cancelled,
    )
    .await
    .map_err(|error| funding_recovery_error(path, &crate::native_rpc::poll_error(&error)))?;
    Ok::<_, CliError>((response.transaction_hash, receipt))
}

fn retain_local_funding_evidence(
    path: &Path,
    evidence: &LocalFundingErrorData,
) -> Result<(), CliError> {
    let signed: thruput_xdr::transaction::SignedTransaction =
        xdrs::txs::decode(&evidence.signed_transaction.0)
            .map_err(|e| CliError::LocalFundingAttempt(e.to_string()))?;
    let hash = thruput_signing::transaction_hash(&signed.tx)
        .map_err(|e| CliError::LocalFundingAttempt(e.to_string()))?;
    if evidence.tx_hash != thruput_json_rpc_types::transaction::TxHash(hash.into()) {
        return Err(CliError::LocalFundingAttempt(
            "retained funding hash does not match signed bytes".into(),
        ));
    }
    let unresolved = |error: CliError| CliError::LocalFundingUnresolved {
        tx_hash: format_tx_hash(&evidence.tx_hash),
        attempt_path: path.display().to_string(),
        message: error.to_string(),
    };
    let mut attempt = read_attempt::<LocalFundingAttempt>(path)
        .map_err(&unresolved)?
        .ok_or_else(|| {
            unresolved(CliError::LocalFundingAttempt(
                "retained request disappeared".into(),
            ))
        })?;
    if attempt
        .transaction
        .as_ref()
        .is_some_and(|original| original != evidence)
    {
        return Err(CliError::LocalFundingAttempt(
            "funding response replaced the retained transaction".into(),
        ));
    }
    attempt.transaction = Some(evidence.clone());
    write_attempt(path, &attempt).map_err(unresolved)
}

fn retain_native_funding_error(path: &Path, error: &NativeClientError) -> Result<(), CliError> {
    if let NativeClientError::Application { data, .. } = error
        && let DecodedError::Known { data, .. } = data.as_ref()
        && let ErrorData::LocalFundingUnresolved {
            transaction_hash,
            signed_transaction,
            ..
        } = data.as_ref()
    {
        retain_local_funding_evidence(
            path,
            &LocalFundingErrorData::new(*transaction_hash, signed_transaction.clone()),
        )?;
    }
    Ok(())
}

async fn funding_preflight<C: jsonrpsee::core::client::ClientT + Sync>(
    client: &NativeClient<C>,
    network_hash: &NetworkHash,
    funding_method: &str,
) -> Result<(), CliError> {
    thruput_client::check_endpoint(
        client,
        &[
            funding_method,
            "getNetworkInfo",
            "getTransactionReceipt",
            "getTransactionStatus",
        ],
    )
    .await
    .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    thruput_client::validate_network(client, network_hash)
        .await
        .map_err(|error| crate::native_rpc::rpc_error(&error))?;
    Ok(())
}

fn funding_cancelled(path: &Path, phase: &str) -> CliError {
    funding_recovery_error(
        path,
        &crate::native_rpc::failure(
            "airdrop cancelled",
            json!({"cancelled": true, "fundingRequest": phase}),
        ),
    )
}

fn print_funding_recovery(print: &Print, error: &CliError) {
    let Some(evidence) = error.details_json() else {
        return;
    };
    let Some(path) = evidence["attemptPath"].as_str() else {
        return;
    };
    print.detail("Retained attempt", path);
    if let Some(hash) = evidence["transactionHash"]
        .as_str()
        .or_else(|| evidence["result"]["transactionHash"].as_str())
        .or_else(|| evidence["rpcError"]["data"]["transactionHash"].as_str())
    {
        print.detail("Transaction evidence", hash);
    }
    if let Some(seconds) = evidence["result"]["retryAfterSeconds"].as_str() {
        print.detail("Retry after", format_args!("{seconds} seconds"));
    }
    print.detail(
        "Recovery",
        "Keep this attempt. Rerun the same command with the same amount to reconcile it.",
    );
}

fn funding_recovery_error(path: &Path, error: &CliError) -> CliError {
    let mut evidence = error.details_json().unwrap_or_else(|| json!({}));
    evidence["attemptPath"] = json!(path.display().to_string());
    let message = match error {
        CliError::NativeRpcFailure { message, .. } => message.clone(),
        _ => error.to_string(),
    };
    crate::native_rpc::failure(message, evidence)
}

fn ensure_local_airdrop_success(
    prepared: &PreparedLocalAirdrop,
    receipt: &TransactionReceipt,
    tx_hash: &str,
) -> Result<(), CliError> {
    let decoded_abort = crate::tx_utils::decode_receipt_abort(receipt, None);
    if matches!(receipt.execution, ReceiptExecution::Abort { .. })
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

    print.status(format_args!("Local faucet: {}", prepared.faucet_wallet));
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
    let rpc_url = profile.rpc_endpoint()?;
    let manifest = load_configured_runtime_manifest(profile, "faucet")?
        .ok_or_else(|| CliError::Other("airdrop requires a local runtime manifest".into()))?;
    let recipient_wallet = recipient.wallet.clone();
    let raw_amount = hog::parse_amount(amount.unwrap_or(DEFAULT_AIRDROP_HOG))?;
    if raw_amount == 0 {
        return Err(CliError::InvalidHogAmount {
            input: amount.unwrap_or(DEFAULT_AIRDROP_HOG).into(),
            message: "amount must be positive".into(),
        });
    }
    let faucet = resolve_local_bootstrap_account(profile, LocalBootstrapRole::Faucet)?;
    let faucet_wallet = format_contract_address(&faucet.wallet_address);
    let faucet_balance = fetch_balance_blocking(
        rpc_url,
        thruput_json_rpc_types::ContractAddress::from(faucet.wallet_address),
    )?;
    let funding_projection = local_funding_projection(
        &faucet_balance.raw_balance.to_string(),
        &manifest.bootstrap.storage_cost_model,
        raw_amount,
        1,
    )?;
    // A retained request may already have spent this balance. Let the service
    // reconcile it instead of rejecting a retry against the post-transfer balance.
    let pending_path = local_funding_attempt_path(
        &thruput_encoding::encode(faucet.network_hash.as_ref()),
        &faucet_wallet,
        &recipient_wallet,
    )?;
    let retained = read_attempt::<LocalFundingAttempt>(&pending_path)?.is_some();
    if funding_projection.projected_balance.is_none() && !retained {
        return Err(insufficient_local_faucet_error(
            raw_amount,
            funding_projection.estimated_max,
        ));
    }
    Ok(LocalAirdropPlan {
        rpc_url: rpc_url.to_owned(),
        profile_name: profile.name.clone(),
        recipient_input: recipient.input.clone(),
        recipient_wallet,
        amount_hog: hog::format_amount(raw_amount),
        raw_amount,
        faucet_role: faucet.role,
        faucet_wallet,
        faucet_balance_height: faucet_balance.state_block_height,
        funding_projection,
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
        .map(thruput_json_rpc_types::ContractAddress::from)
        .and_then(|address| fetch_balance_blocking(rpc_url, address))
        .and_then(|balance| {
            let raw = u128::try_from(balance.raw_balance).map_err(|error| {
                CliError::Other(format!(
                    "invalid raw balance `{}` from RPC: {error}",
                    balance.raw_balance
                ))
            })?;
            print.status(format_args!(
                "{label}: {} HOG at block {}",
                hog::format_amount(raw),
                *balance.state_block_height
            ));
            Ok(json!({
                "status": "observed",
                "rawBalance": balance.raw_balance.to_string(),
                "balanceHog": hog::format_amount(raw),
                "height": balance.state_block_height.to_string(),
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
    let task = crate::print::progress::Task::start(print, "Requesting remote airdrop", true);
    task.print().phase(format_args!(
        "Requesting remote airdrop to {}...",
        prepared.recipient_wallet
    ));
    let request: requests::AirdropRequest = serde_json::from_value(json!({
        "recipient": prepared.recipient_wallet,
        "clientNonce": prepared.client_nonce,
    }))
    .map_err(|error| CliError::Other(format!("invalid retained airdrop request: {error}")))?;
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let (tx_hash, height, execution, raw_amount) = runtime
        .block_on(request_remote_airdrop(prepared, &request))
        .inspect_err(|error| print_funding_recovery(task.print(), error))?;
    let tx_hash = format_tx_hash(&tx_hash);
    let _ = task.finish();
    print.group(
        crate::print::Outcome::Success,
        "Airdrop complete",
        |print| {
            print.status(format_args!("Airdrop sent: {tx_hash}"));
            if print.has_human_ui() {
                print.detail("Recipient", &prepared.recipient_wallet);
                print.detail("Raw amount", raw_amount);
                print.detail("Height", *height);
            }
        },
    );

    print.json(&CommandResult::new(
        cmd,
        json!({
            "schemaVersion": AIRDROP_JSON_SCHEMA_VERSION,
            "mode": "remote",
            "recipient": prepared.recipient_input,
            "recipientWallet": prepared.recipient_wallet,
            "faucetUrl": prepared.faucet_url,
            "rpcUrl": prepared.rpc_url,
            "rawAmount": raw_amount.to_string(),
            "policy": {
                "rawAmount": prepared.airdrop_policy.raw_amount,
                "amountHog": prepared.amount_hog,
                "recipientCooldownSeconds": prepared.airdrop_policy.recipient_cooldown_seconds.to_string(),
                "clientQuotaMax": prepared.airdrop_policy.client_quota_max,
                "clientQuotaWindowSeconds": prepared.airdrop_policy.client_quota_window_seconds.to_string(),
                "maxPendingPerRecipient": prepared.airdrop_policy.max_pending_per_recipient,
            },
            "txHash": tx_hash,
            "height": height.to_string(),
            "execution": execution,
        }),
    ))
}

async fn request_remote_airdrop(
    prepared: &PreparedRemoteAirdrop,
    request: &requests::AirdropRequest,
) -> Result<
    (
        thruput_json_rpc_types::transaction::TxHash,
        thruput_json_rpc_types::BlockHeight,
        ReceiptExecution,
        u128,
    ),
    CliError,
> {
    let mut cancelled = crate::native_rpc::install_interrupt_listener().await;
    let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
    let signing_network = NetworkHash::from(prepared.network_hash.0);
    tokio::select! {
        biased;
        () = &mut cancelled => return Err(funding_cancelled(&prepared.attempt_path, "notSent")),
        result = funding_preflight(&client, &signing_network, "faucet_requestAirdrop") => result?,
    }
    let response = tokio::select! {
        biased;
        () = &mut cancelled => return Err(funding_cancelled(&prepared.attempt_path, "uncertain")),
        result = client.request_airdrop(request.clone()) => crate::native_rpc::known("faucet_requestAirdrop", result)
            .map_err(|error| funding_recovery_error(&prepared.attempt_path, &error))?,
    };
    let response_error = |error: CliError| {
        funding_recovery_error(
            &prepared.attempt_path,
            &crate::native_rpc::failure(error.to_string(), json!({"result":response})),
        )
    };
    let (recipient, raw_amount) = match &response {
        AirdropAttempt::Pending {
            recipient,
            raw_amount,
            ..
        }
        | AirdropAttempt::Succeeded {
            recipient,
            raw_amount,
            ..
        } => (recipient, *raw_amount),
        _ => {
            return Err(crate::native_rpc::failure(
                "unsupported airdrop outcome",
                json!({"result": response}),
            ));
        }
    };
    if *recipient != request.recipient {
        return Err(response_error(CliError::NetworkDescriptorMismatch {
            field: "airdrop.recipient",
            expected: prepared.recipient_wallet.clone(),
            actual: format_contract_address(&recipient.0),
        }));
    }
    validate_remote_airdrop_amount(&prepared.airdrop_policy.raw_amount, &raw_amount.to_string())
        .map_err(response_error)?;
    match response {
        AirdropAttempt::Pending {
            transaction_hash: Some(hash),
            ..
        } => {
            // The funding call is never repeated; only read the supplied transaction.
            let receipt = poll_receipt(
                &client,
                prepared.network_hash,
                hash,
                None,
                ReceiptPollConfig::default(),
                cancelled,
            )
            .await
            .map_err(|error| {
                funding_recovery_error(
                    &prepared.attempt_path,
                    &crate::native_rpc::poll_error(&error),
                )
            })?;
            remove_remote_airdrop_attempt(prepared)?;
            ensure_tx_success("airdrop", &receipt, &format_tx_hash(&hash))?;
            Ok((hash, receipt.block_height, receipt.execution, raw_amount))
        }
        AirdropAttempt::Pending { .. } => Err(funding_recovery_error(
            &prepared.attempt_path,
            &crate::native_rpc::failure(
                "remote airdrop is pending without a transaction hash",
                json!({"result":response}),
            ),
        )),
        AirdropAttempt::Succeeded {
            transaction_hash,
            block_height,
            execution: AirdropExecution::Return { return_data },
            ..
        } => {
            remove_remote_airdrop_attempt(prepared)?;
            Ok((
                transaction_hash,
                block_height,
                ReceiptExecution::Return { return_data },
                raw_amount,
            ))
        }
        _ => Err(crate::native_rpc::failure(
            "unsupported airdrop outcome",
            json!({"result":response}),
        )),
    }
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
    policy: &thruput_json_rpc_types::network_descriptor::AirdropDescriptor,
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
    let rpc_url = profile.rpc_endpoint()?.to_owned();
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
        AirdropPlan::Local(plan) => Ok(PreparedAirdrop::Local(PreparedLocalAirdrop {
            rpc_url: plan.rpc_url,
            recipient_input: plan.recipient_input,
            recipient_wallet: plan.recipient_wallet,
            amount_hog: plan.amount_hog,
            raw_amount: plan.raw_amount,
            faucet_role: plan.faucet_role,
            faucet_wallet: plan.faucet_wallet,
            faucet_balance_height: plan.faucet_balance_height,
            funding_projection: plan.funding_projection,
            network_hash: plan.network_hash,
        })),
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
    let (loaded_attempt, attempt_lock) =
        load_or_create_remote_airdrop_attempt(profile, &recipient_wallet, descriptor)?;
    Ok(PreparedAirdrop::Remote(PreparedRemoteAirdrop {
        faucet_url,
        rpc_url,
        recipient_input: recipient.to_owned(),
        recipient_wallet,
        client_nonce: loaded_attempt.attempt.client_nonce,
        network_hash: descriptor.network_hash,
        airdrop_policy: descriptor.policy.clone(),
        amount_hog: descriptor.amount_hog.clone(),
        attempt_path: loaded_attempt.path,
        _attempt_lock: attempt_lock,
    }))
}

fn load_or_create_remote_airdrop_attempt(
    profile: &ResolvedProfile,
    recipient_wallet: &str,
    descriptor: &RemoteAirdropDescriptor,
) -> Result<(LoadedRemoteAirdropAttempt, File), CliError> {
    let network_id = descriptor.network_id.clone();
    let genesis_hash = descriptor.genesis_hash.clone();
    let path =
        remote_airdrop_attempt_path(&profile.name, recipient_wallet, &network_id, &genesis_hash)?;
    let attempt_lock = lock_remote_airdrop_attempt(&path)?;
    if let Some(attempt) = read_attempt::<RemoteAirdropAttempt>(&path)? {
        if attempt.profile != profile.name
            || attempt.recipient_wallet != recipient_wallet
            || attempt.network_id != network_id
            || attempt.genesis_hash != genesis_hash
        {
            return Err(CliError::Other(
                "saved airdrop attempt does not match the recipient and network".into(),
            ));
        }
        return Ok((LoadedRemoteAirdropAttempt { attempt, path }, attempt_lock));
    }

    let mut client_nonce = [0u8; 16];
    getrandom::fill(&mut client_nonce).map_err(|error| CliError::Other(error.to_string()))?;
    let attempt = RemoteAirdropAttempt {
        profile: profile.name.clone(),
        recipient_wallet: recipient_wallet.to_owned(),
        network_id,
        genesis_hash,
        client_nonce: format!("0x{}", hex::encode(client_nonce)),
    };
    write_attempt(&path, &attempt)?;
    Ok((LoadedRemoteAirdropAttempt { attempt, path }, attempt_lock))
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
            "wallet address must be a C StrKey or base58 encoding of exactly 32 bytes".into(),
        )),
        Err(CliError::UnknownAddressTarget { .. }) => Err(CliError::Other(format!(
            "unknown wallet `{}`; run `thruput wallet create --key={}` or pass a C StrKey or base58 wallet address",
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
            print.group(crate::print::Outcome::DryRun, "Airdrop", |print| {
                print.status(format_args!(
                    "Prepared dry-run airdrop of {} HOG to {}.",
                    prepared.amount_hog, prepared.recipient_wallet
                ));
                print.status(format_args!("  raw amount: {}", prepared.raw_amount));
                print.status(format_args!(
                    "  faucet: {} ({})",
                    prepared.faucet_wallet, prepared.faucet_role
                ));
                print.status("  submitted: no (--dry-run)");
            });

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
                        "address": prepared.faucet_wallet,
                        "role": prepared.faucet_role,
                    },
                    "plannedRequests": ["dev_fundAccount"],
                }),
            ))?;
        }
        PreparedAirdrop::Remote(prepared) => {
            print.group(crate::print::Outcome::DryRun, "Remote airdrop", |print| {
                print.status(format_args!(
                    "Prepared dry-run remote airdrop to {}.",
                    prepared.recipient_wallet
                ));
                print.status(format_args!("  faucet: {}", prepared.faucet_url));
                print.status("  submitted: no (--dry-run)");
            });

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
                        "faucet_requestAirdrop"
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
    print.group(crate::print::Outcome::DryRun, "Remote airdrop", |print| {
        print.status(format_args!(
            "Prepared dry-run remote airdrop to {}.",
            prepared.recipient_wallet
        ));
        print.status(format_args!("  faucet: {}", prepared.faucet_url));
        print.status("  submitted: no (--dry-run)");
    });

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
                "faucet_requestAirdrop"
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

    use serde_json::json;
    use serial_test::serial;
    use thruput_contract_util::{NormalizedMethodSignature, contract_entrypoint_id};
    use thruput_json_rpc_types::responses::{ReceiptExecution, TransactionReceipt};

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

    const ATTEMPT_LOCK_TEST_PATH: &str = "THRUPUT_ATTEMPT_LOCK_TEST_PATH";
    const ATTEMPT_LOCK_TEST_MODE: &str = "THRUPUT_ATTEMPT_LOCK_TEST_MODE";

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
    fn funding_recovery_retains_evidence_without_repeating_error_prefix() {
        let error =
            crate::native_rpc::failure("receipt unavailable", json!({"transactionHash":"saved"}));
        let error =
            super::funding_recovery_error(std::path::Path::new("/tmp/attempt.json"), &error);
        assert_eq!(error.to_string(), "native RPC: receipt unavailable");
        let evidence = error.details_json().unwrap();
        assert_eq!(evidence["transactionHash"], "saved");
        assert_eq!(evidence["attemptPath"], "/tmp/attempt.json");
    }

    #[test]
    #[serial]
    fn local_funding_reuses_retained_nonce_and_refuses_changed_amount() {
        let tmp = tempfile::tempdir().unwrap();
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let mut prepared = prepared_local_airdrop();
        let (path, lock, first) = super::retain_local_funding_request(&prepared).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["networkHash"],
            thruput_encoding::encode(prepared.network_hash.as_ref())
        );
        drop(lock);
        let (_, lock, retry) = super::retain_local_funding_request(&prepared).unwrap();
        assert_eq!(first, retry);
        drop(lock);
        prepared.rpc_url = "http://127.0.0.1:4444".into();
        let (routed_path, lock, routed) = super::retain_local_funding_request(&prepared).unwrap();
        assert_eq!(path, routed_path);
        assert_eq!(first, routed);
        drop(lock);
        prepared.raw_amount += 1;
        assert!(super::retain_local_funding_request(&prepared).is_err());
        prepared.raw_amount -= 1;
        prepared.faucet_wallet = thruput_encoding::encode(&[0x55; 32]);
        let (other_path, _, other) = super::retain_local_funding_request(&prepared).unwrap();
        assert_ne!(path, other_path);
        assert_ne!(first.client_nonce, other.client_nonce);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
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
                address: thruput_encoding::encode(&[0xab; 32]),
                owner_public_key: thruput_encoding::encode(&[0xef; 32]),
            },
        );
        let profile = test_profile(wallets);

        assert_eq!(
            resolve_airdrop_recipient(&profile, "alice").unwrap(),
            thruput_encoding::encode(&[0xab; 32])
        );
        assert_eq!(
            resolve_airdrop_recipient(&profile, &thruput_encoding::encode(&[0xcd; 32])).unwrap(),
            thruput_encoding::encode(&[0xcd; 32])
        );
        assert!(resolve_airdrop_recipient(&profile, "missing").is_err());
    }

    #[test]
    #[serial]
    fn recipient_base58_and_strkey_select_the_same_attempt_path() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let lower = resolve_airdrop_recipient(&profile, &thruput_encoding::encode(&[0xab; 32]))
            .expect("bug: base58 recipient should resolve");
        let mixed =
            resolve_airdrop_recipient(&profile, &thruput_encoding::address_strkey(&[0xab; 32]))
                .expect("bug: StrKey recipient should resolve");

        assert_eq!(lower, mixed);
        assert_eq!(
            remote_airdrop_attempt_path(&profile.name, &lower, "devnet", "0xgenesis").unwrap(),
            remote_airdrop_attempt_path(&profile.name, &mixed, "devnet", "0xgenesis").unwrap()
        );
    }

    #[test]
    #[serial]
    fn reuses_pending_attempt_without_changing_nonce() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let descriptor = test_network_descriptor();
        let current_recipient = thruput_encoding::encode(&[0xab; 32]);
        let recipient_wallet = resolve_airdrop_recipient(&profile, &current_recipient)
            .expect("bug: base58 recipient should resolve");
        let path = remote_airdrop_attempt_path(
            &profile.name,
            &recipient_wallet,
            &descriptor.network_id,
            &descriptor.genesis_hash,
        )
        .unwrap();
        let client_nonce = format!("0x{}", "11".repeat(16));
        write_attempt(
            &path,
            &RemoteAirdropAttempt {
                profile: profile.name.clone(),
                recipient_wallet: recipient_wallet.clone(),
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
        .expect("bug: pending attempt should be reused");
        let PreparedAirdrop::Remote(prepared) = prepared else {
            panic!("bug: expected remote airdrop");
        };

        assert_eq!(prepared.client_nonce, client_nonce);
        assert_eq!(prepared.recipient_wallet, recipient_wallet);
        assert_eq!(prepared.attempt_path, path);
        assert!(path.exists());
    }

    #[test]
    #[serial]
    fn mismatched_pending_attempt_is_not_overwritten() {
        let tmp = tempfile::tempdir().expect("bug: tempdir should be created");
        let _cache = EnvGuard::set_str("XDG_CACHE_HOME", tmp.path().to_str().unwrap());
        let profile = test_profile(BTreeMap::new());
        let descriptor = test_network_descriptor();
        let recipient_wallet = thruput_encoding::encode(&[0xab; 32]);
        let path = remote_airdrop_attempt_path(
            &profile.name,
            &recipient_wallet,
            &descriptor.network_id,
            &descriptor.genesis_hash,
        )
        .unwrap();
        write_attempt(
            &path,
            &RemoteAirdropAttempt {
                profile: profile.name.clone(),
                recipient_wallet: thruput_encoding::encode(&[0xcd; 32]),
                network_id: descriptor.network_id.clone(),
                genesis_hash: descriptor.genesis_hash.clone(),
                client_nonce: format!("0x{}", "11".repeat(16)),
            },
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();

        let error = prepare_remote_airdrop_with_descriptor(
            &profile,
            &recipient_wallet,
            recipient_wallet.clone(),
            "http://127.0.0.1:8081".into(),
            "http://127.0.0.1:9545".into(),
            &descriptor,
        )
        .unwrap_err();
        assert!(error.to_string().contains("does not match"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn malformed_direct_recipient_reports_address_format() {
        let profile = test_profile(BTreeMap::new());

        let err = resolve_airdrop_recipient(&profile, "Cinvalid")
            .expect_err("bug: malformed direct address should fail");

        assert!(
            err.to_string()
                .contains("C StrKey or base58 encoding of exactly 32 bytes")
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
            message.contains(r"thruput wallet create --key='alice; touch /tmp/pwned\n\u{1b}[31m'")
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
                address: thruput_encoding::encode(&[0xab; 32]),
                owner_public_key: thruput_encoding::encode(&[0xef; 32]),
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
        assert_eq!(
            prepared.recipient_wallet,
            thruput_encoding::encode(&[0xab; 32])
        );
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
    fn airdrop_hosted_missing_faucet_reports_network_error() {
        let _manifest = EnvGuard::set_str("THRUPUT_RUNTIME_MANIFEST", "");
        for network in [Network::Futurenet, Network::Devnet] {
            let name = network.as_str();
            let mut profile = test_profile(BTreeMap::new());
            profile.name = name.into();
            profile.network = Some(network);

            let error = resolve_faucet_backend(&profile, "airdrop")
                .expect_err("bug: missing hosted faucet should fail");

            assert_eq!(
                error.to_string(),
                format!("profile `{name}` targets {name}, but airdrop requires `faucet_url`")
            );
        }
    }

    #[test]
    #[serial]
    fn airdrop_hosted_missing_rpc_reports_network_error() {
        let _manifest = EnvGuard::set_str("THRUPUT_RUNTIME_MANIFEST", "");
        for network in [Network::Futurenet, Network::Devnet] {
            let name = network.as_str();
            let mut profile = test_profile(BTreeMap::new());
            profile.name = name.into();
            profile.network = Some(network);
            profile.rpc_url = None;
            profile.faucet_url = Some("http://127.0.0.1:8081".into());

            let error = resolve_faucet_backend(&profile, "airdrop")
                .expect_err("bug: missing hosted rpc should fail");

            assert_eq!(
                error.to_string(),
                format!("profile `{name}` targets {name}, but airdrop requires `rpc_url`")
            );
        }
    }

    #[test]
    #[serial]
    fn airdrop_hosted_with_urls_uses_remote_faucet() {
        let _manifest = EnvGuard::set_str("THRUPUT_RUNTIME_MANIFEST", "");
        for network in [Network::Futurenet, Network::Devnet] {
            let name = network.as_str();
            let mut profile = test_profile(BTreeMap::new());
            profile.name = name.into();
            profile.network = Some(network);
            profile.faucet_url = Some("http://127.0.0.1:8081".into());

            let backend = resolve_faucet_backend(&profile, "airdrop")
                .expect("bug: configured hosted profile should resolve");

            assert_eq!(backend, FaucetBackend::RemoteFaucet);
        }
    }

    #[test]
    #[serial]
    fn airdrop_local_missing_manifest_reports_local_error() {
        let _manifest = EnvGuard::set_str("THRUPUT_RUNTIME_MANIFEST", "");
        let mut profile = test_profile(BTreeMap::new());
        profile.name = "local".into();
        profile.network = Some(Network::Local);

        let error = resolve_faucet_backend(&profile, "airdrop")
            .expect_err("bug: missing local manifest should fail");

        assert_eq!(
            error.to_string(),
            "profile `local` targets local, but airdrop requires a local runtime manifest; run `thruput node` or configure `runtime_manifest`"
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
        receipt.execution = ReceiptExecution::Fault {};

        let error = ensure_local_airdrop_success(&prepared, &receipt, "0x44")
            .expect_err("bug: deterministic error should remain a transaction failure");

        assert!(
            !error
                .to_string()
                .contains("insufficient actionable balance")
        );
        assert!(error.to_string().contains("fault"));
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
            recipient_wallet: thruput_encoding::encode(&[0xab; 32]),
            amount_hog: "10".into(),
            raw_amount: 100_000_000,
            faucet_role: "faucet".into(),
            faucet_wallet: thruput_encoding::encode(&[0xcd; 32]),
            faucet_balance_height: thruput_json_rpc_types::BlockHeight::from(42),
            funding_projection: crate::balance::LocalFundingProjection {
                committed_balance: 200_000_000,
                transaction_storage_cost: 12_007,
                estimated_max: 199_987_993,
                projected_balance: Some(99_987_993),
            },
            network_hash: thruput_xdr::types::NetworkHash::new(
                thruput_xdr::types::SHA256Hash::from([0; 32]),
            ),
        }
    }

    fn reverted_too_large_receipt() -> TransactionReceipt {
        serde_json::from_value(json!({
            "transactionHash": thruput_encoding::encode(&[0x44;32]),
            "blockHeight": "43", "execution": { "result":"abort",
                "abortData": format!("0x{}",hex::encode(erc20_too_large_abort())) },
            "events":[], "gasUsed":"1500",
        }))
        .unwrap()
    }

    fn erc20_too_large_abort() -> Vec<u8> {
        let selector = <thruput_erc20_if::ERC20Error as thruput_sdk::exn::Exn>::SELECTOR;
        let mut bytes = xdrs::cxs::encode(&selector).expect("bug: selector should encode");
        bytes.extend_from_slice(
            &xdrs::cxs::encode(&thruput_erc20_if::ERC20Error::TooLarge)
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
            "schemaVersion": thruput_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": "groundhog-network",
            "networkId": "devnet-preview",
            "networkHash": thruput_json_rpc_types::NetworkHashText::from([0x42; 32]),
            "protocol": "x23",
            "genesisHash": thruput_json_rpc_types::NetworkHashText::from([0x01; 32]),
            "endpoints": { "rpcUrl": "http://127.0.0.1:9545" },
            "nativeHog": {
                "tokenAddress": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x02; 32])),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": { "walletAddress": thruput_json_rpc_types::ContractAddress::from(thruput_xdr::types::ContractAddress::from([0x04; 32])) },
            "airdrop": {
                "rawAmount": "100000000",
                "recipientCooldownSeconds": "43200",
                "clientQuotaMax": 10,
                "clientQuotaWindowSeconds": "3600",
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
