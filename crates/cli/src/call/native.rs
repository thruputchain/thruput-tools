//! Native call preparation and reports for explicitly selected qualification runs.

use std::{future::Future, pin::Pin, task::Poll};

use jsonrpsee::core::client::ClientT;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use thruput_client::native::NativeSubmitError;
use thruput_json_rpc_client::native::{
    NativeClient,
    receipt::{ReceiptPollCause, ReceiptPollConfig, ReceiptPollError},
};
use thruput_json_rpc_types::{
    ContractAddress, NetworkHash,
    hex::HexBytes,
    requests::AddressQuery,
    responses::{DecodedResponse, ReceiptExecution, TransactionReceipt},
};

use super::{PreparedCall, format_entrypoint_id, selector_json};
use crate::{
    CliError, Print,
    abort_json::AbortReport,
    account_resolution::ResolvedTransactionSender,
    codec::{decode_abort, decode_return, parse_contract_address},
    native_rpc,
    network_descriptor::{WalletEntrypoints, WalletSigningContext},
    result::{CommandError, CommandResult},
};

const REQUIRED_METHODS: &[&str] = &[
    "getVersionInfo",
    "getNetworkInfo",
    "getChainTip",
    "getBalance",
    "sendTransaction",
    "getTransactionStatus",
    "getTransactionReceipt",
];

pub(super) fn prepare_context(
    sender: &ResolvedTransactionSender,
    url: &str,
    runtime: &tokio::runtime::Runtime,
) -> Result<(WalletSigningContext, i64), CliError> {
    if sender.native_account.is_none() {
        return Err(native_rpc::failure(
            "native RPC qualification currently requires a pubkey wallet account",
            json!({"preflightReason":"unsupportedWallet"}),
        ));
    }
    let expected = sender.network_hash.as_ref().ok_or_else(|| native_rpc::failure(
        "native RPC qualification requires an independent network hash in the profile or runtime manifest",
        json!({"preflightReason":"missingNetworkPin"}),
    ))?;
    let client = NativeClient::new(crate::http_client::rpc_client(url)?);
    let expiry = runtime.block_on(async {
        verify_endpoint(&client, expected).await?;
        thruput_client::native::resolve_block_expiry_height(
            &client,
            crate::tx_utils::DEFAULT_BLOCK_EXPIRY_OFFSET,
        )
        .await
        .map_err(|error| native_rpc::rpc_error(&error))
    })?;
    Ok((
        WalletSigningContext {
            network_hash: expected.clone(),
            entrypoints: WalletEntrypoints {
                deploy: thruput_signing::default_wallet::DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
                do_action: thruput_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
                source: "native_default_wallet",
            },
        },
        expiry,
    ))
}

async fn verify_endpoint<C: ClientT + Sync>(
    client: &NativeClient<C>,
    expected: &thruput_xdr::types::NetworkHash,
) -> Result<(), CliError> {
    thruput_client::native::check_endpoint(client, REQUIRED_METHODS)
        .await
        .map_err(|error| native_rpc::rpc_error(&error))?;
    thruput_client::native::validate_network(client, expected)
        .await
        .map_err(|error| native_rpc::rpc_error(&error))?;
    Ok(())
}

pub(super) fn check_balance(url: &str, wallet: &str, gas_limit: u64) -> Result<(), CliError> {
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let client = NativeClient::new(crate::http_client::rpc_client(url)?);
    runtime.block_on(fetch_balance(&client, wallet, gas_limit))
}

async fn fetch_balance<C: ClientT + Sync>(
    client: &NativeClient<C>,
    wallet: &str,
    gas_limit: u64,
) -> Result<(), CliError> {
    let address = ContractAddress::from(parse_contract_address(wallet)?);
    let response = client
        .get_balance(AddressQuery { address })
        .await
        .map_err(|error| native_rpc::client_error(&error))?;
    let balance = match response {
        DecodedResponse::Known(balance) => balance,
        DecodedResponse::Unsupported { raw } => {
            return Err(native_rpc::failure(
                "unsupported getBalance result",
                json!({"result":raw}),
            ));
        }
        _ => {
            return Err(native_rpc::failure(
                "unsupported getBalance response representation",
                Value::Null,
            ));
        }
    };
    if balance.address != address || balance.raw_balance < 0 {
        return Err(native_rpc::failure(
            "getBalance returned inconsistent account data",
            json!({"expectedAddress":address,"result":balance}),
        ));
    }
    let available = u128::try_from(balance.raw_balance)
        .map_err(|_| native_rpc::failure("invalid native balance", json!({"result":balance})))?;
    if available < u128::from(gas_limit) {
        return Err(native_rpc::failure(
            format!(
                "wallet {wallet} has {available} free capacity; this call requires {gas_limit}"
            ),
            json!({
                "preflightReason": "insufficientCapacity",
                "wallet": wallet,
                "availableRaw": available.to_string(),
                "requiredRaw": gas_limit.to_string(),
            }),
        ));
    }
    Ok(())
}

/// Local progress through the one submission attempt, independent of server
/// admission evidence. Uncertain means no valid acknowledgement was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum SubmissionPhase {
    NotSent,
    Uncertain,
    Acknowledged,
}

#[derive(Debug)]
struct CallFailure {
    phase: SubmissionPhase,
    error: CliError,
}

impl CallFailure {
    fn cancelled(phase: SubmissionPhase, prepared: &PreparedCall) -> Self {
        Self {
            phase,
            error: native_rpc::failure(
                "contract call cancelled",
                json!({
                    "transactionHash": prepared.signed.tx_hash(),
                    "cause": {"kind": "cancelled"},
                }),
            ),
        }
    }
}

async fn signal_cancellation(signal: impl Future<Output = std::io::Result<()>>) {
    if let Err(error) = signal.await {
        tracing::warn!(%error, "unable to install interrupt handler");
        // Signal registration failure is not a user cancellation.
        std::future::pending::<()>().await;
    }
}

async fn install_interrupt_listener() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    let mut signal: Pin<Box<dyn Future<Output = ()> + Send>> =
        Box::pin(signal_cancellation(tokio::signal::ctrl_c()));
    // ctrl_c installs its process handler on first poll, before any RPC work.
    let ready = std::future::poll_fn(|cx| Poll::Ready(signal.as_mut().poll(cx).is_ready())).await;
    if ready {
        Box::pin(std::future::ready(()))
    } else {
        signal
    }
}

#[tracing::instrument(skip_all)]
pub(super) fn execute(
    prepared: &PreparedCall,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let common = NativeCallCommon::new(prepared)?;
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
    print.detail(
        "prepared transaction",
        crate::tx_utils::format_tx_hash(&prepared.signed.tx_hash()),
    );
    let task = crate::print::progress::Task::start(print, "Submitting contract call", true);
    let result = runtime.block_on(async {
        let cancelled = install_interrupt_listener().await;
        submit_and_wait(&client, prepared, ReceiptPollConfig::default(), cancelled).await
    });
    let warnings = task.finish();
    let receipt = match result {
        Ok(receipt) => receipt,
        Err(CallFailure { phase, error }) => {
            let evidence = match &error {
                CliError::NativeRpcFailure { evidence, .. } => evidence.clone(),
                _ => error.details_json().unwrap_or(Value::Null),
            };
            if !print.is_json() {
                print.warn(crate::terminal::terminal_safe(&error.to_string()));
                print.detail(
                    "transaction",
                    crate::tx_utils::format_tx_hash(&prepared.signed.tx_hash()),
                );
                for (label, value) in failure_observations(phase, &evidence) {
                    print.detail(label, value);
                }
                for warning in warnings {
                    print.warn(warning);
                }
            }
            print.json(&CommandResult::failure_with_error(
                cmd,
                NativeCallFailure {
                    common,
                    transaction_hash: prepared.signed.tx_hash(),
                    submission: phase,
                    evidence,
                },
                CommandError::from_cli_error(&error),
            ))?;
            return Err(CliError::CommandReportedFailure);
        }
    };
    let report = NativeCallReport::new(common, prepared, receipt);
    let failure = report.execution_failure();
    if !print.is_json() {
        print_response(&report, print);
        for warning in warnings {
            print.warn(warning);
        }
    }
    if let Some(error) = failure {
        print.json(&CommandResult::failure_with_error(cmd, report, error))?;
        return Err(CliError::CommandReportedFailure);
    }
    print.json(&CommandResult::new(cmd, report))?;
    Ok(())
}

async fn submit_and_wait<C, F>(
    client: &NativeClient<C>,
    prepared: &PreparedCall,
    config: ReceiptPollConfig,
    cancelled: F,
) -> Result<TransactionReceipt, CallFailure>
where
    C: ClientT + Sync,
    F: Future<Output = ()>,
{
    tokio::pin!(cancelled);
    tokio::select! {
        biased;
        () = &mut cancelled => return Err(CallFailure::cancelled(SubmissionPhase::NotSent, prepared)),
        result = verify_endpoint(client, &prepared.network_hash) => result.map_err(|error| CallFailure {
            phase: SubmissionPhase::NotSent,
            error,
        })?,
    }
    let mut started = false;
    let submission = {
        let send = async {
            started = true;
            prepared.signed.submit_native(client).await
        };
        tokio::select! {
            biased;
            () = &mut cancelled => None,
            result = send => Some(result),
        }
    };
    let submitted = match submission {
        Some(result) => result.map_err(|error| CallFailure {
            phase: if matches!(error, NativeSubmitError::Encoding { .. }) {
                SubmissionPhase::NotSent
            } else {
                SubmissionPhase::Uncertain
            },
            error: submit_error(&error),
        })?,
        None => {
            return Err(CallFailure::cancelled(
                if started {
                    SubmissionPhase::Uncertain
                } else {
                    SubmissionPhase::NotSent
                },
                prepared,
            ));
        }
    };
    submitted
        .wait(
            client,
            NetworkHash::from(prepared.network_hash.clone().into_inner()),
            config,
            &mut cancelled,
        )
        .await
        .map_err(|error| CallFailure {
            phase: SubmissionPhase::Acknowledged,
            error: poll_error(&error),
        })
}

fn failure_observations(phase: SubmissionPhase, evidence: &Value) -> Vec<(&'static str, String)> {
    let phase = match phase {
        SubmissionPhase::NotSent => "not sent",
        SubmissionPhase::Uncertain => "outcome uncertain",
        SubmissionPhase::Acknowledged => "admission acknowledged",
    };
    let mut fields = vec![("Submission", phase.to_owned())];
    if let Some(height) = evidence
        .get("commitment")
        .and_then(|value| value.get("blockHeight"))
        .and_then(Value::as_str)
    {
        fields.push((
            "Committed block height",
            crate::terminal::terminal_safe(height),
        ));
    }
    if let Some(status) = evidence
        .get("lastStatus")
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
    {
        fields.push(("Last status", crate::terminal::terminal_safe(status)));
    }
    fields
}

fn submit_error(error: &NativeSubmitError) -> CliError {
    let evidence = match error {
        NativeSubmitError::Rpc { tx_hash, source } => {
            json!({"transactionHash":tx_hash,"cause":native_rpc::client_evidence(source)})
        }
        NativeSubmitError::Unsupported { tx_hash, raw } => {
            json!({"transactionHash":tx_hash,"result":raw})
        }
        NativeSubmitError::HashMismatch { tx_hash, returned } => {
            json!({"transactionHash":tx_hash,"returnedTransactionHash":returned})
        }
        NativeSubmitError::Encoding { tx_hash, .. }
        | NativeSubmitError::ResponseRepresentation { tx_hash } => {
            json!({"transactionHash":tx_hash})
        }
        _ => json!({"unsupportedSubmissionError":error.to_string()}),
    };
    native_rpc::failure(error.to_string(), evidence)
}

fn poll_error(error: &ReceiptPollError) -> CliError {
    let cause = match &error.cause {
        ReceiptPollCause::Timeout => json!({"kind":"timeout"}),
        ReceiptPollCause::Cancelled => json!({"kind":"cancelled"}),
        ReceiptPollCause::Lookup(source) => {
            json!({"kind":"lookup","error":native_rpc::client_evidence(source)})
        }
        ReceiptPollCause::Unsupported { method, raw } => {
            json!({"kind":"unsupported","method":method,"result":raw})
        }
        ReceiptPollCause::Invalid { message, raw } => {
            json!({"kind":"invalid","message":message,"result":raw})
        }
        _ => json!({"kind":"unsupported","message":error.cause.to_string()}),
    };
    let commitment = error.evidence.commitment.as_ref().map(|known| {
        json!({
            "networkHash":known.network_hash,
            "transactionHash":known.transaction_hash,
            "blockHeight":known.block_height.to_string(),
        })
    });
    let evidence = json!({
        "transactionHash":error.evidence.transaction_hash,
        "networkHash":error.evidence.network_hash,
        "commitment":commitment,
        "lastStatus":error.evidence.last_status,
        "lastLookupError":error.evidence.last_lookup_error.as_ref().map(native_rpc::client_evidence),
        "cause":cause,
    });
    native_rpc::failure(error.to_string(), evidence)
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeCallCommon {
    schema_version: u32,
    rpc_url: String,
    address: ContractAddress,
    wallet_address: ContractAddress,
    method: String,
    entrypoint: HexBytes,
    calldata: HexBytes,
    selector: Value,
}
impl NativeCallCommon {
    fn new(prepared: &PreparedCall) -> Result<Self, CliError> {
        Ok(Self {
            schema_version: 4,
            rpc_url: prepared.rpc_url.clone(),
            address: ContractAddress::from(parse_contract_address(&prepared.contract_address_hex)?),
            wallet_address: ContractAddress::from(parse_contract_address(
                &prepared.wallet_address_hex,
            )?),
            method: prepared.method.clone(),
            entrypoint: HexBytes(prepared.resolved.entrypoint.into_inner().to_vec()),
            calldata: HexBytes(prepared.calldata.clone()),
            selector: selector_json(&prepared.resolved),
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeCallReport {
    #[serde(flatten)]
    common: NativeCallCommon,
    #[serde(flatten)]
    receipt: TransactionReceipt,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_return_data: Option<HexBytes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    return_decode_error: Option<String>,
    #[serde(flatten)]
    abort: AbortReport,
}
impl NativeCallReport {
    fn new(common: NativeCallCommon, prepared: &PreparedCall, receipt: TransactionReceipt) -> Self {
        let mut child_return_data = None;
        let mut return_value = None;
        let mut return_decode_error = None;
        if let ReceiptExecution::Return { return_data } = &receipt.execution {
            match prepared.signed.decode_return(&return_data.0) {
                Ok(bytes) => {
                    return_value = decode_return(&bytes, prepared.resolved.metadata.as_ref());
                    child_return_data = Some(HexBytes(bytes));
                }
                Err(error) => return_decode_error = Some(error.to_string()),
            }
        }
        let decoded_abort = match &receipt.execution {
            ReceiptExecution::Abort { abort_data } => Some(decode_abort(
                &abort_data.0,
                prepared.resolved.metadata.as_ref(),
            )),
            _ => None,
        };
        Self {
            common,
            receipt,
            child_return_data,
            return_value,
            return_decode_error,
            abort: AbortReport::from_decode(decoded_abort.as_ref()),
        }
    }

    fn execution_failure(&self) -> Option<CommandError> {
        if let Some(message) = &self.return_decode_error {
            return Some(CommandError::new(
                "native_wallet_return_invalid",
                message,
                json!({"transactionHash":self.receipt.transaction_hash,"execution":self.receipt.execution}),
            ));
        }
        let (code, message) = match &self.receipt.execution {
            ReceiptExecution::Return { .. } => return None,
            ReceiptExecution::Abort { .. } => {
                ("transaction_reverted", "Transaction execution aborted")
            }
            ReceiptExecution::Fault {} => (
                "transaction_deterministic_error",
                "Transaction execution faulted",
            ),
            ReceiptExecution::NonDeterministicError {} => (
                "transaction_nondeterministic_error",
                "Transaction failed during commit",
            ),
            ReceiptExecution::FailedGasSponsorship {} => (
                "transaction_failed_gas_sponsorship",
                "Transaction failed gas sponsorship",
            ),
            _ => ("native_rpc_failure", "Unsupported transaction execution"),
        };
        Some(CommandError::new(
            code,
            message,
            json!({"transactionHash":self.receipt.transaction_hash,"execution":self.receipt.execution}),
        ))
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeCallFailure {
    #[serde(flatten)]
    common: NativeCallCommon,
    transaction_hash: thruput_json_rpc_types::transaction::TxHash,
    submission: SubmissionPhase,
    evidence: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeCallDryRun {
    #[serde(flatten)]
    common: NativeCallCommon,
    transaction_hash: thruput_json_rpc_types::transaction::TxHash,
    dry_run: bool,
    submitted: bool,
    signed_transaction: HexBytes,
}

pub(super) fn print_dry_run(
    prepared: &PreparedCall,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let signed_transaction = hex::decode(
        prepared
            .tx_hex
            .strip_prefix("0x")
            .unwrap_or(&prepared.tx_hex),
    )
    .map_err(|error| {
        native_rpc::failure(
            error.to_string(),
            json!({"transactionHash":prepared.signed.tx_hash()}),
        )
    })?;
    let report = NativeCallDryRun {
        common: NativeCallCommon::new(prepared)?,
        transaction_hash: prepared.signed.tx_hash(),
        dry_run: true,
        submitted: false,
        signed_transaction: HexBytes(signed_transaction),
    };
    if !print.is_json() {
        print.outcome_report(
            crate::print::Outcome::DryRun,
            "Contract call prepared",
            &[
                ("Address".into(), prepared.contract_address_hex.clone()),
                ("Method".into(), prepared.method.clone()),
                (
                    "Entrypoint".into(),
                    format_entrypoint_id(prepared.resolved.entrypoint),
                ),
                (
                    "Transaction".into(),
                    crate::tx_utils::format_tx_hash(&prepared.signed.tx_hash()),
                ),
                (
                    "Signed transaction".into(),
                    thruput_json_rpc_types::hex::prefixed_hex(&report.signed_transaction.0),
                ),
                ("Submitted".into(), "no (--dry-run)".into()),
            ],
            &[],
        );
    }
    print.json(&CommandResult::new(cmd, report))?;
    Ok(())
}

fn print_response(report: &NativeCallReport, print: &Print) {
    let (outcome, title) = if report.execution_failure().is_none() {
        (crate::print::Outcome::Success, "Call succeeded")
    } else {
        (crate::print::Outcome::Error, "Call failed")
    };
    let mut fields = vec![
        ("Address".into(), report.common.address.to_string()),
        ("Method".into(), report.common.method.clone()),
        (
            "Transaction".into(),
            crate::tx_utils::format_tx_hash(&report.receipt.transaction_hash),
        ),
        (
            "Block height".into(),
            report.receipt.block_height.to_string(),
        ),
        ("Gas used".into(), report.receipt.gas_used.to_string()),
        (
            "Execution".into(),
            serde_json::to_string(&report.receipt.execution)
                .unwrap_or_else(|_| "unavailable".into()),
        ),
    ];
    if let Some(value) = &report.return_value {
        fields.push(("Return".into(), value.to_string()));
    } else if let Some(bytes) = &report.child_return_data {
        fields.push((
            "Child return".into(),
            thruput_json_rpc_types::hex::prefixed_hex(&bytes.0),
        ));
    }
    if let Some(value) = &report.abort.abort_value {
        fields.push(("Abort".into(), value.to_string()));
    }
    for (index, event) in report.receipt.events.iter().enumerate() {
        let mut parts = vec![event.address.to_string()];
        parts.extend(event.topics.iter().enumerate().map(|(index, topic)| {
            format!(
                "topic{index}={}",
                thruput_json_rpc_types::hex::prefixed_hex(topic.0)
            )
        }));
        parts.push(format!(
            "data={}",
            thruput_json_rpc_types::hex::prefixed_hex(&event.data.0)
        ));
        fields.push((format!("Event {index}"), parts.join(" ")));
    }
    let warnings: Vec<_> = report
        .abort
        .abort_decode_error
        .iter()
        .chain(report.return_decode_error.iter())
        .cloned()
        .collect();
    print.outcome_report(outcome, title, &fields, &warnings);
}

#[cfg(test)]
mod tests;
