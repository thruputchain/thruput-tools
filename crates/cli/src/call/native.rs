//! Native call preparation, submission evidence and execution reports.

use std::future::Future;

use jsonrpsee::core::client::ClientT;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use thruput_json_rpc_client::native::{NativeClient, receipt::ReceiptPollConfig};
use thruput_json_rpc_types::{
    ContractAddress,
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
    native_rpc::{self, failure_observations},
    network_descriptor::WalletSigningContext,
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
    profile: &crate::config::ResolvedProfile,
    url: &str,
    runtime: &tokio::runtime::Runtime,
) -> Result<(WalletSigningContext, i64), CliError> {
    let context = match (&sender.native_account, &sender.network_hash) {
        (Some(_), Some(network_hash)) => WalletSigningContext {
            network_hash: network_hash.clone(),
            entrypoints: crate::network_descriptor::native_entrypoints(),
        },
        _ => sender.signing_context(profile, runtime)?,
    };
    let expected = &context.network_hash;
    let client = NativeClient::new(crate::http_client::rpc_client(url)?);
    let expiry = runtime.block_on(async {
        verify_endpoint(&client, expected).await?;
        thruput_client::resolve_block_expiry_height(
            &client,
            crate::tx_utils::DEFAULT_BLOCK_EXPIRY_OFFSET,
        )
        .await
        .map_err(|error| native_rpc::rpc_error(&error))
    })?;
    Ok((context, expiry))
}

async fn verify_endpoint<C: ClientT + Sync>(
    client: &NativeClient<C>,
    expected: &thruput_xdr::types::NetworkHash,
) -> Result<(), CliError> {
    thruput_client::check_endpoint(client, REQUIRED_METHODS)
        .await
        .map_err(|error| native_rpc::rpc_error(&error))?;
    thruput_client::validate_network(client, expected)
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

use crate::native_rpc::{SubmissionFailure as CallFailure, SubmissionPhase};

#[tracing::instrument(skip_all)]
pub(super) fn execute(
    prepared: &PreparedCall,
    cmd: &'static str,
    print: &Print,
) -> Result<(), CliError> {
    let common = NativeCallCommon::new(prepared)?;
    let signed_transaction = HexBytes(
        hex::decode(prepared.signed.tx_hex().trim_start_matches("0x")).map_err(|error| {
            native_rpc::failure(
                error.to_string(),
                json!({"transactionHash":prepared.signed.tx_hash()}),
            )
        })?,
    );
    let runtime =
        tokio::runtime::Runtime::new().map_err(|e| CliError::RuntimeInit(e.to_string()))?;
    let client = NativeClient::new(crate::http_client::rpc_client(&prepared.rpc_url)?);
    print.detail(
        "prepared transaction",
        crate::tx_utils::format_tx_hash(&prepared.signed.tx_hash()),
    );
    let task = crate::print::progress::Task::start(print, "Submitting contract call", true);
    let result = runtime.block_on(async {
        let cancelled = native_rpc::install_interrupt_listener().await;
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
                    signed_transaction,
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
    native_rpc::submit_and_wait(
        client,
        prepared.signed.tx_hash(),
        prepared.signed.tx_hex(),
        &prepared.network_hash,
        config,
        cancelled,
    )
    .await
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct NativeCallCommon {
    schema_version: u32,
    rpc_url: String,
    address: ContractAddress,
    wallet_address: ContractAddress,
    wallet_entrypoint: String,
    wallet_entrypoint_source: String,
    network_hash: thruput_json_rpc_types::NetworkHash,
    method: String,
    entrypoint: HexBytes,
    calldata: HexBytes,
    selector: Value,
}
impl NativeCallCommon {
    fn new(prepared: &PreparedCall) -> Result<Self, CliError> {
        Ok(Self {
            schema_version: super::CALL_JSON_SCHEMA_VERSION,
            rpc_url: prepared.rpc_url.clone(),
            address: ContractAddress::from(parse_contract_address(&prepared.contract_address_hex)?),
            wallet_address: ContractAddress::from(parse_contract_address(
                &prepared.wallet_address_hex,
            )?),
            wallet_entrypoint: prepared.wallet_entrypoint_id.clone(),
            wallet_entrypoint_source: prepared.wallet_entrypoint_source.to_owned(),
            network_hash: prepared.network_hash.clone().into_inner().into(),
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
    signed_transaction: HexBytes,
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
        ("Wallet".into(), report.common.wallet_address.to_string()),
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
