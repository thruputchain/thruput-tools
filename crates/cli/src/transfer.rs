use std::time::Duration;

use thruput_client::{NativeClient, ReceiptPollConfig};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::responses::{ReceiptExecution, TransactionReceipt};
use thruput_json_rpc_types::transaction::TxHash;
use thruput_signing::{
    DEFAULT_TX_GAS_LIMIT, SchemeSigner, SignTransactionParams, Signer as _,
    encode_signed_transaction_xdr, sign_transaction, transaction_hash,
};
use thruput_xdr::transaction::Transaction;
use thruput_xdr::types::NetworkHash;

use crate::account_resolution::{
    bind_sender_to_network, resolve_address_target, resolve_sender_account,
};
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::native_token;

use crate::result::{CommandError, CommandResult};
use crate::terminal::terminal_safe;
use crate::transfer_json::{
    TRANSFER_JSON_SCHEMA_VERSION, TransferCommittedReport, TransferDryRunReport,
    TransferFailureReport, TransferReportCommon,
};
use crate::tx_utils::{
    command_error_for_exec_status, format_tx_hash, nonce_from_timestamp, print_receipt_data,
    resolve_block_expiry_height, summarize_exec_status,
};
use crate::{CliError, Print};

#[derive(Debug)]
struct PreparedTransfer {
    profile: String,
    rpc_url: String,
    sender: thruput_xdr::types::ContractAddress,
    sender_wallet: Option<String>,
    signer_public_key: thruput_xdr::types::PublicKey,
    recipient_input: String,
    recipient: thruput_xdr::types::ContractAddress,
    token_address: thruput_xdr::types::ContractAddress,
    amount: String,
    raw_amount: u128,
    tx_hash: TxHash,
    expiry_height: i64,
    signed_transaction: Vec<u8>,
    network_hash: NetworkHash,
}

#[derive(Debug)]
struct TransferPlan {
    profile: String,
    rpc_url: String,
    sender: thruput_xdr::types::ContractAddress,
    sender_wallet: Option<String>,
    signer_public_key: thruput_xdr::types::PublicKey,
    recipient_input: String,
    recipient: thruput_xdr::types::ContractAddress,
    token_address: thruput_xdr::types::ContractAddress,
    amount: String,
    raw_amount: u128,
    tx_hash: TxHash,
    expiry_height: i64,
    transaction: Transaction,
    signer: SchemeSigner,
    network_hash: NetworkHash,
}

/// Prepare a native token transfer from the effective profile wallet.
// The command boundary mirrors its CLI flags plus shared output context.
#[expect(clippy::too_many_arguments)]
pub fn run(
    recipient: &str,
    amount: &str,
    from: Option<&str>,
    dry_run: bool,
    force: bool,
    profile_name: Option<&str>,
    overrides: &ProfileOverrides,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let cwd = std::env::current_dir()?;
    let profile = resolve_profile_with_overrides(&cwd, profile_name, overrides)?;
    if !dry_run {
        print.require_confirmation_available(force, "transfer native tokens")?;
    }
    let plan = prepare_plan(profile, recipient, amount, from)?;
    if !dry_run
        && !print.confirm_review(
            "Review transfer",
            force,
            "transfer native tokens",
            "Submit this transfer?",
            |print| {
                print.detail("recipient", format_contract_address(&plan.recipient));
                print.detail("amount", format_args!("{} native tokens", plan.amount));
                print.detail("from", format_contract_address(&plan.sender));
                print.detail("profile", &plan.profile);
            },
        )?
    {
        print.cancelled();
        return Ok(());
    }
    let prepared = sign_plan(plan)?;
    if dry_run {
        return print_dry_run(&prepared, cmd, print);
    }

    print.detail("prepared transaction", format_tx_hash(&prepared.tx_hash));
    let task = crate::print::progress::Task::start(
        print,
        "Submitting transfer and waiting for receipt",
        true,
    );
    let outcome = submit_and_confirm(&prepared);
    let _ = task.finish();
    match outcome {
        Ok(receipt) => print_committed(&prepared, receipt, cmd, print),
        Err(error) => {
            let evidence = error.details_json().unwrap_or(serde_json::Value::Null);
            let phase = crate::native_rpc::submission_phase(&error)
                .unwrap_or(crate::native_rpc::SubmissionPhase::NotSent);
            print.warn(terminal_safe(&error.to_string()));
            print.detail("transaction", format_tx_hash(&prepared.tx_hash));
            for (label, value) in crate::native_rpc::failure_observations(phase, &evidence) {
                print.detail(label, value);
            }
            if matches!(
                phase,
                crate::native_rpc::SubmissionPhase::Uncertain
                    | crate::native_rpc::SubmissionPhase::Acknowledged
            ) {
                print.note("The transaction may still commit. Inspect its status and receipt before resubmitting.");
            }
            print.json(&CommandResult::failure_with_error(
                cmd,
                TransferFailureReport {
                    common: report_common(
                        &prepared,
                        false,
                        phase != crate::native_rpc::SubmissionPhase::NotSent,
                    ),
                    network_hash: prepared.network_hash.clone().into_inner().into(),
                    submission: phase,
                    signed_transaction: HexBytes(prepared.signed_transaction.clone()),
                    evidence,
                },
                CommandError::from_cli_error(&error),
            ))?;
            Err(CliError::CommandReportedFailure)
        }
    }
}

fn prepare_plan(
    profile: ResolvedProfile,
    recipient_input: &str,
    amount_input: &str,
    from: Option<&str>,
) -> Result<TransferPlan, CliError> {
    let rpc_url = profile.rpc_endpoint()?.to_owned();

    let recipient = resolve_address_target(&profile, recipient_input)?;
    let raw_amount = native_token::parse_amount(amount_input)?;
    let amount = native_token::format_amount(raw_amount);
    let token_address = native_token::token_address();

    let sender_account = resolve_sender_account(&profile, from)?;
    let sender = sender_account.wallet_address;
    if sender == recipient {
        return Err(CliError::TransferToSelf {
            address: format_contract_address(&recipient),
        });
    }

    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let resolved_sender = bind_sender_to_network(&profile, sender_account)?;
    let calldata = native_token::encode_transfer_calldata(&recipient, raw_amount)?;
    let wallet_context = resolved_sender.signing_context(&profile, &rt)?;
    let expiry_height = rt.block_on(resolve_block_expiry_height(&rpc_url))?;

    let tx = thruput_client::prepare_call(
        thruput_client::CallRequest {
            wallet: resolved_sender.wallet(wallet_context.entrypoints.do_action),
            target_address: token_address,
            target_entrypoint: native_token::transfer_entrypoint(),
            target_calldata: calldata.as_slice(),
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            target_gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: expiry_height,
            nonce: nonce_from_timestamp(),
        },
        wallet_context.network_hash.clone(),
    )
    .map_err(|error| CliError::Other(error.to_string()))?
    .transaction()
    .clone();
    let tx_hash = TxHash(
        transaction_hash(&tx)
            .map_err(|error| CliError::Other(error.to_string()))?
            .into(),
    );
    Ok(TransferPlan {
        profile: profile.name,
        rpc_url,
        sender,
        sender_wallet: resolved_sender.wallet_name,
        signer_public_key: resolved_sender.signer.public_key(),
        recipient_input: recipient_input.to_owned(),
        recipient,
        token_address,
        amount,
        raw_amount,
        tx_hash,
        expiry_height,
        transaction: tx,
        signer: resolved_sender.signer,
        network_hash: wallet_context.network_hash,
    })
}

fn sign_plan(plan: TransferPlan) -> Result<PreparedTransfer, CliError> {
    let authenticated_tx = sign_transaction(
        plan.transaction,
        &plan.signer,
        SignTransactionParams::default(),
        &plan.network_hash,
    )
    .map_err(|error| CliError::Other(error.to_string()))?;
    let signed_transaction = encode_signed_transaction_xdr(&authenticated_tx)
        .map_err(|error| CliError::Other(error.to_string()))?;
    Ok(PreparedTransfer {
        profile: plan.profile,
        rpc_url: plan.rpc_url,
        sender: plan.sender,
        sender_wallet: plan.sender_wallet,
        signer_public_key: plan.signer_public_key,
        recipient_input: plan.recipient_input,
        recipient: plan.recipient,
        token_address: plan.token_address,
        amount: plan.amount,
        raw_amount: plan.raw_amount,
        tx_hash: plan.tx_hash,
        expiry_height: plan.expiry_height,
        signed_transaction,
        network_hash: plan.network_hash,
    })
}

fn print_dry_run(
    prepared: &PreparedTransfer,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let sender = format_contract_address(&prepared.sender);
    let (signer_scheme, signer_public_key) =
        crate::keys::public_key_scheme_and_bytes(&prepared.signer_public_key);
    let signer_public_key = thruput_encoding::encode(signer_public_key);
    let recipient = format_contract_address(&prepared.recipient);
    let token_address = format_contract_address(&prepared.token_address);
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    let signed_transaction = format!("0x{}", hex::encode(&prepared.signed_transaction));

    print.group(crate::print::Outcome::DryRun, "Transfer", |print| {
        print.status(format_args!(
            "Prepared signed dry-run transfer of {} native tokens to {}.",
            prepared.amount, recipient
        ));
        print.detail("sender", &sender);
        print.detail("signer scheme", signer_scheme);
        print.detail("signer public key", &signer_public_key);
        print.detail("raw amount", prepared.raw_amount);
        print.detail("token", &token_address);
        print.detail("expiry height", prepared.expiry_height);
        print.detail("tx", &tx_hash);
        print.detail("signed transaction", &signed_transaction);
        print.warn("the signed transaction can be submitted until its expiry height");
        print.status("  submitted: no (--dry-run)");
    });

    let report = TransferDryRunReport {
        common: report_common(prepared, true, false),
        expiry_height: prepared.expiry_height,
        signed_transaction: HexBytes(prepared.signed_transaction.clone()),
    };
    print.json(&CommandResult::new(cmd, report))
}

fn submit_and_confirm(prepared: &PreparedTransfer) -> Result<TransactionReceipt, CliError> {
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    rt.block_on(async {
        let cancelled = crate::native_rpc::install_interrupt_listener().await;
        let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
        let signed_transaction = format!("0x{}", hex::encode(&prepared.signed_transaction));
        let (attempts, interval) = receipt_poll_config();
        crate::native_rpc::submit_and_wait(
            &client,
            prepared.tx_hash,
            &signed_transaction,
            &prepared.network_hash,
            ReceiptPollConfig {
                timeout: interval.saturating_mul(u32::try_from(attempts).unwrap_or(u32::MAX)),
                interval,
            },
            cancelled,
        )
        .await
        .map_err(|failure| failure.error)
    })
}

fn receipt_poll_config() -> (usize, Duration) {
    let mut attempts = thruput_json_rpc_client::receipt::DEFAULT_RECEIPT_POLL_ATTEMPTS;
    let mut interval = thruput_json_rpc_client::receipt::DEFAULT_RECEIPT_POLL_INTERVAL;
    if cfg!(debug_assertions) {
        if let Ok(value) = std::env::var("THRUPUT_TEST_RECEIPT_POLL_ATTEMPTS")
            && let Ok(value) = value.parse::<usize>()
        {
            attempts = value.max(1);
        }
        if let Ok(value) = std::env::var("THRUPUT_TEST_RECEIPT_POLL_INTERVAL_MS")
            && let Ok(value) = value.parse::<u64>()
        {
            interval = Duration::from_millis(value);
        }
    }
    (attempts, interval)
}

fn print_committed(
    prepared: &PreparedTransfer,
    receipt: TransactionReceipt,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    let status_summary = terminal_safe(&summarize_exec_status(&receipt.execution));
    let decoded_abort = crate::tx_utils::decode_receipt_abort(&receipt, None);
    print.group(
        if matches!(receipt.execution, ReceiptExecution::Return { .. }) {
            crate::print::Outcome::Success
        } else {
            crate::print::Outcome::Error
        },
        "Transfer",
        |print| {
            if matches!(receipt.execution, ReceiptExecution::Return { .. }) {
                print.success(format_args!(
                    "Transferred {} native tokens.",
                    prepared.amount
                ));
            } else {
                print.warn(format_args!("Transfer {status_summary}"));
            }
            print.detail("recipient", format_contract_address(&prepared.recipient));
            print.detail("tx", &tx_hash);
            print.detail("height", *receipt.block_height);
            print_receipt_data(&receipt, decoded_abort.as_ref(), print);
        },
    );

    let command_error = command_error_for_exec_status(&receipt.execution);
    let report = TransferCommittedReport {
        common: report_common(prepared, false, true),
        block_height: receipt.block_height,
        execution: receipt.execution,
        events: receipt.events,
        gas_used: receipt.gas_used,
        abort: crate::abort_json::AbortReport::from_decode(decoded_abort.as_ref()),
    };
    if let Some(error) = command_error {
        print.json(&CommandResult::failure_with_error(cmd, report, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, report))
}

fn report_common(
    prepared: &PreparedTransfer,
    dry_run: bool,
    submitted: bool,
) -> TransferReportCommon {
    let (signer_scheme, signer_public_key) =
        crate::keys::public_key_scheme_and_bytes(&prepared.signer_public_key);
    TransferReportCommon {
        schema_version: TRANSFER_JSON_SCHEMA_VERSION,
        profile: prepared.profile.clone(),
        rpc_url: prepared.rpc_url.clone(),
        dry_run,
        submitted,
        sender: prepared.sender.into(),
        sender_wallet: prepared.sender_wallet.clone(),
        signer_scheme: signer_scheme.to_owned(),
        signer_public_key: thruput_encoding::encode(signer_public_key),
        recipient_input: prepared.recipient_input.clone(),
        recipient: prepared.recipient.into(),
        token_address: prepared.token_address.into(),
        amount: prepared.amount.clone(),
        raw_amount: prepared.raw_amount.to_string(),
        tx_hash: prepared.tx_hash,
    }
}
