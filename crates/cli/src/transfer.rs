use std::time::Duration;

use thruput_json_rpc_client::{
    PollTransactionReceiptError, poll_transaction_receipt_with_client_config,
    submit_transaction_with_client,
};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use thruput_json_rpc_types::transaction::{TransactionStatus, TxHash};
use thruput_signing::{
    DEFAULT_TX_GAS_LIMIT, LocalSigner, SignTransactionParams, encode_signed_transaction_xdr,
    sign_transaction, transaction_hash,
};
use thruput_xdr::transaction::Transaction;
use thruput_xdr::types::NetworkHash;

use crate::account_resolution::{
    bind_sender_to_network, resolve_address_target, resolve_sender_account,
};
use crate::codec::format_contract_address;
use crate::config::{ProfileOverrides, ResolvedProfile, resolve_profile_with_overrides};
use crate::hog;

use crate::result::{CommandError, CommandResult};
use crate::terminal::{render_shell_arg, terminal_safe};
use crate::transfer_json::{
    TRANSFER_JSON_SCHEMA_VERSION, TransferCommittedReport, TransferConfirmation,
    TransferDryRunReport, TransferReportCommon, TransferUnconfirmedReport,
};
use crate::tx_utils::{
    command_error_for_exec_status, format_tx_hash, is_conclusive_send_error,
    is_unsupported_rpc_method_error, nonce_from_timestamp, print_receipt_data,
    resolve_block_expiry_height, rpc_error_message, summarize_exec_status,
};
use crate::{CliError, Print};

#[derive(Debug)]
struct PreparedTransfer {
    profile: String,
    rpc_url: String,
    rpc_url_cli_override: bool,
    sender: thruput_xdr::types::ContractAddress,
    sender_wallet: Option<String>,
    signer_public_key: thruput_xdr::types::Ed25519PublicKey,
    recipient_input: String,
    recipient: thruput_xdr::types::ContractAddress,
    token_address: thruput_xdr::types::ContractAddress,
    amount: String,
    raw_amount: u128,
    tx_hash: TxHash,
    expiry_height: i64,
    signed_transaction: Vec<u8>,
}

#[derive(Debug)]
struct TransferPlan {
    profile: String,
    rpc_url: String,
    rpc_url_cli_override: bool,
    sender: thruput_xdr::types::ContractAddress,
    sender_wallet: Option<String>,
    signer_public_key: thruput_xdr::types::Ed25519PublicKey,
    recipient_input: String,
    recipient: thruput_xdr::types::ContractAddress,
    token_address: thruput_xdr::types::ContractAddress,
    amount: String,
    raw_amount: u128,
    tx_hash: TxHash,
    expiry_height: i64,
    transaction: Transaction,
    signer: LocalSigner,
    network_hash: NetworkHash,
}

/// Prepare a native HOG transfer from the effective profile wallet.
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
        print.require_confirmation_available(force, "transfer HOG")?;
    }
    let plan = prepare_plan(profile, recipient, amount, from)?;
    if !dry_run
        && !print.confirm_review(
            "Review transfer",
            force,
            "transfer HOG",
            "Submit this transfer?",
            |print| {
                print.detail("recipient", format_contract_address(&plan.recipient));
                print.detail("amount", format_args!("{} HOG", plan.amount));
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
    let outcome = submit_and_confirm(&prepared)?;
    let _ = task.finish();
    match outcome {
        LiveOutcome::Committed(receipt) => print_committed(&prepared, receipt, cmd, print),
        LiveOutcome::ProcessingFailure(status) => {
            print_processing_failure(&prepared, status, cmd, print)
        }
        LiveOutcome::Unavailable {
            transaction_status,
            reason,
        } => print_unavailable(&prepared, transaction_status, reason, cmd, print),
    }
}

#[derive(Debug)]
enum LiveOutcome {
    Committed(TransactionReceipt),
    ProcessingFailure(TransactionStatus),
    Unavailable {
        transaction_status: Option<TransactionStatus>,
        reason: &'static str,
    },
}

fn prepare_plan(
    profile: ResolvedProfile,
    recipient_input: &str,
    amount_input: &str,
    from: Option<&str>,
) -> Result<TransferPlan, CliError> {
    let rpc_url = profile.rpc_endpoint()?.to_owned();

    let recipient = resolve_address_target(&profile, recipient_input)?;
    let raw_amount = hog::parse_amount(amount_input)?;
    let amount = hog::format_amount(raw_amount);
    let token_address = hog::token_address();

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
    let calldata = hog::encode_transfer_calldata(&recipient, raw_amount)?;
    let wallet_context = resolved_sender.signing_context(&profile, &rt)?;
    let expiry_height = rt.block_on(resolve_block_expiry_height(&rpc_url))?;

    let tx = thruput_client::prepare_call(
        thruput_client::CallRequest {
            wallet: resolved_sender.wallet(wallet_context.entrypoints.do_action),
            target_address: token_address,
            target_entrypoint: hog::transfer_entrypoint(),
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
        rpc_url_cli_override: profile.rpc_url_cli_override,
        sender,
        sender_wallet: resolved_sender.wallet_name,
        signer_public_key: thruput_xdr::types::Ed25519PublicKey::from(
            resolved_sender.signer.verifying_key_bytes(),
        ),
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
        rpc_url_cli_override: plan.rpc_url_cli_override,
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

    print.group(crate::print::Outcome::DryRun, "Transfer", |print| {
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
    });

    let report = TransferDryRunReport {
        common: report_common(prepared, true, false),
        expiry_height: prepared.expiry_height,
        signed_transaction: HexBytes(prepared.signed_transaction.clone()),
    };
    print.json(&CommandResult::new(cmd, report))
}

fn submit_and_confirm(prepared: &PreparedTransfer) -> Result<LiveOutcome, CliError> {
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    rt.block_on(async {
        let client = crate::http_client::rpc_client(&prepared.rpc_url)?;
        let signed_transaction = format!("0x{}", hex::encode(&prepared.signed_transaction));
        match submit_transaction_with_client(&client, &signed_transaction).await {
            Ok(server_hash) if server_hash != prepared.tx_hash => {
                return Ok(LiveOutcome::Unavailable {
                    transaction_status: None,
                    reason: "the RPC returned a transaction hash that did not match the locally computed hash",
                });
            }
            Err(source) if is_unsupported_rpc_method_error(&source) =>
            {
                return Err(CliError::RpcCapabilityUnsupported {
                    method: "sendRawTransaction",
                    message: rpc_error_message(&source),
                });
            }
            Err(source) if is_conclusive_send_error(&source) =>
            {
                return Err(CliError::RpcMethodFailed {
                    method: "sendRawTransaction",
                    message: rpc_error_message(&source),
                });
            }
            Ok(_) | Err(_) => {}
        }

        poll_local_hash(prepared, &client).await
    })
}

async fn poll_local_hash(
    prepared: &PreparedTransfer,
    client: &(impl jsonrpsee::core::client::ClientT + Sync),
) -> Result<LiveOutcome, CliError> {
    let (max_attempts, poll_interval) = receipt_poll_config();
    match poll_transaction_receipt_with_client_config(
        client,
        prepared.tx_hash,
        max_attempts,
        poll_interval,
    )
    .await
    {
        Ok(receipt) if receipt.tx_hash != prepared.tx_hash => Ok(LiveOutcome::Unavailable {
            transaction_status: None,
            reason: "the RPC returned a receipt whose hash did not match the locally computed hash",
        }),
        Ok(receipt) => Ok(LiveOutcome::Committed(receipt)),
        Err(PollTransactionReceiptError::TransactionStatus { status, .. })
            if matches!(
                status,
                TransactionStatus::Invalid { .. }
                    | TransactionStatus::Retry { .. }
                    | TransactionStatus::Dropped { .. }
            ) =>
        {
            Ok(LiveOutcome::ProcessingFailure(status))
        }
        Err(PollTransactionReceiptError::TransactionStatus { status, .. }) => {
            let transaction_status =
                matches!(status, TransactionStatus::RecentFailure { .. }).then_some(status);
            Ok(LiveOutcome::Unavailable {
                transaction_status,
                reason: "the transaction did not produce a receipt before polling ended",
            })
        }
        Err(PollTransactionReceiptError::GetTransactionReceipt { .. }) => {
            Ok(LiveOutcome::Unavailable {
                transaction_status: None,
                reason: "the receipt RPC failed during confirmation polling",
            })
        }
        Err(PollTransactionReceiptError::ReceiptTimeout { .. }) => Ok(LiveOutcome::Unavailable {
            transaction_status: None,
            reason: "the transaction did not produce a receipt before polling ended",
        }),
        Err(_) => Ok(LiveOutcome::Unavailable {
            transaction_status: None,
            reason: "transaction confirmation ended without a receipt",
        }),
    }
}

fn receipt_poll_config() -> (usize, Duration) {
    let mut attempts = thruput_json_rpc_client::DEFAULT_RECEIPT_POLL_ATTEMPTS;
    let mut interval = thruput_json_rpc_client::DEFAULT_RECEIPT_POLL_INTERVAL;
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
    let status_summary = terminal_safe(&summarize_exec_status(&receipt.status));
    let decoded_abort = crate::tx_utils::decode_receipt_abort(&receipt, None);
    print.group(
        if matches!(receipt.status, ExecStatus::Ok) {
            crate::print::Outcome::Success
        } else {
            crate::print::Outcome::Error
        },
        "Transfer",
        |print| {
            if matches!(receipt.status, ExecStatus::Ok) {
                print.success(format_args!("Transferred {} HOG.", prepared.amount));
            } else {
                print.warn(format_args!("Transfer {status_summary}"));
            }
            print.detail("recipient", format_contract_address(&prepared.recipient));
            print.detail("tx", &tx_hash);
            print.detail("height", *receipt.height);
            print_receipt_data(&receipt, decoded_abort.as_ref(), print);
        },
    );

    let command_error = command_error_for_exec_status(&receipt.status);
    let report = TransferCommittedReport {
        common: report_common(prepared, false, true),
        height: receipt.height,
        status: receipt.status,
        events: receipt.events,
        gas_used: receipt.gas_used,
        return_data: receipt.return_data,
        abort_data: receipt.abort_data,
        abort: crate::abort_json::AbortReport::from_decode(decoded_abort.as_ref()),
    };
    if let Some(error) = command_error {
        print.json(&CommandResult::failure_with_error(cmd, report, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, report))
}

fn print_processing_failure(
    prepared: &PreparedTransfer,
    status: TransactionStatus,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    let (confirmation, code, message, next_step) = match &status {
        TransactionStatus::Retry { .. } => (
            TransferConfirmation::Retry,
            "transaction_retry",
            "transaction processing failed and needs resubmission",
            "resubmit the transaction; the mempool will not retry it automatically",
        ),
        TransactionStatus::Dropped { .. } => (
            TransferConfirmation::Dropped,
            "transaction_dropped",
            "transaction processing failed permanently",
            "modify the transaction before resubmitting",
        ),
        _ => (
            TransferConfirmation::Invalid,
            "transaction_invalid",
            "transaction was rejected before commitment",
            "check the transaction arguments and signing wallet",
        ),
    };
    let reason = match &status {
        TransactionStatus::Invalid { reason } => terminal_safe(reason),
        TransactionStatus::Retry { error } | TransactionStatus::Dropped { error } => {
            format!("{message}: {error:?}")
        }
        _ => message.to_owned(),
    };
    let (title, action) = if matches!(status, TransactionStatus::Invalid { .. }) {
        ("Transfer rejected", "was rejected")
    } else {
        ("Transfer failed", "failed")
    };
    print.group(crate::print::Outcome::Error, title, |print| {
        print.warn(format_args!("Transfer {action}: {reason}"));
        print.detail("tx", &tx_hash);
        print.detail("next step", next_step);
    });
    let report = TransferUnconfirmedReport {
        common: report_common(prepared, false, true),
        confirmation,
        transaction_status: Some(status.clone()),
    };
    let error = CommandError::new(
        code,
        message,
        serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
    );
    print.json(&CommandResult::failure_with_error(cmd, report, error))?;
    Err(CliError::CommandReportedFailure)
}

fn print_unavailable(
    prepared: &PreparedTransfer,
    transaction_status: Option<TransactionStatus>,
    reason: &'static str,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let tx_hash = format_tx_hash(&prepared.tx_hash);
    print.group(crate::print::Outcome::Unconfirmed, "Transfer", |print| {
        print.warn(format_args!(
            "Transaction confirmation unavailable: {reason}."
        ));
        print.detail("tx", &tx_hash);
        print.warn("the transaction may still commit; do not blindly resubmit it");
        let mut next = format!(
            "thruput tx show {tx_hash} --profile={}",
            render_shell_arg(&prepared.profile),
        );
        if prepared.rpc_url_cli_override {
            use std::fmt::Write as _;
            let _ = write!(next, " --rpc-url={}", render_shell_arg(&prepared.rpc_url));
        }
        print.status(format_args!("  next: {next}"));
    });

    let report = TransferUnconfirmedReport {
        common: report_common(prepared, false, true),
        confirmation: TransferConfirmation::Unavailable,
        transaction_status,
    };
    let error = CommandError::new(
        "transaction_confirmation_unavailable",
        "transaction confirmation is unavailable",
        serde_json::json!({ "reason": reason }),
    );
    print.json(&CommandResult::failure_with_error(cmd, report, error))?;
    Err(CliError::CommandReportedFailure)
}

fn report_common(
    prepared: &PreparedTransfer,
    dry_run: bool,
    submitted: bool,
) -> TransferReportCommon {
    TransferReportCommon {
        schema_version: TRANSFER_JSON_SCHEMA_VERSION,
        profile: prepared.profile.clone(),
        rpc_url: prepared.rpc_url.clone(),
        dry_run,
        submitted,
        sender: prepared.sender.into(),
        sender_wallet: prepared.sender_wallet.clone(),
        signer_public_key: prepared.signer_public_key.into(),
        recipient_input: prepared.recipient_input.clone(),
        recipient: prepared.recipient.into(),
        token_address: prepared.token_address.into(),
        amount: prepared.amount.clone(),
        raw_amount: prepared.raw_amount.to_string(),
        tx_hash: prepared.tx_hash,
    }
}
