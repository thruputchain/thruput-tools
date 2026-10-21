use thruput_json_rpc_types::responses::{ReceiptExecution, TransactionReceipt};
use thruput_json_rpc_types::transaction::TxHash;

use crate::codec::{AbortDecodeError, CallMetadata, DecodedAbort, decode_abort};
use crate::result::CommandError;
use crate::terminal::terminal_safe;
use crate::{CliError, Print};

pub(crate) const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

/// Human-readable one-line summary of an execution status.
pub fn summarize_exec_status(status: &ReceiptExecution) -> String {
    match status {
        ReceiptExecution::Return { .. } => "succeeded".into(),
        ReceiptExecution::Abort { .. } => "aborted".into(),
        ReceiptExecution::Fault {} => "failed with a deterministic execution fault (e.g. out-of-bounds access or out of gas; the node does not report the specific cause)".into(),
        ReceiptExecution::NonDeterministicError {} => "failed with nondeterministic commit error".into(),
        ReceiptExecution::FailedGasSponsorship {} => "failed gas sponsorship".into(),
        _ => "failed with unsupported execution result".into(),
    }
}

/// Stable machine-readable error for an unsuccessful transaction receipt.
pub fn command_error_for_exec_status(status: &ReceiptExecution) -> Option<CommandError> {
    let code = match status {
        ReceiptExecution::Return { .. } => return None,
        ReceiptExecution::Abort { .. } => "transaction_reverted",
        ReceiptExecution::Fault {} => "transaction_deterministic_error",
        ReceiptExecution::NonDeterministicError {} => "transaction_nondeterministic_error",
        ReceiptExecution::FailedGasSponsorship {} => "transaction_failed_gas_sponsorship",
        _ => "transaction_unsupported_error",
    };
    Some(CommandError::new(
        code,
        format!("transaction {}", summarize_exec_status(status)),
        serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
    ))
}

/// Decode a committed abort, preserving empty abort buffers.
pub(crate) fn decode_receipt_abort(
    receipt: &TransactionReceipt,
    metadata: Option<&CallMetadata>,
) -> Option<Result<DecodedAbort, AbortDecodeError>> {
    match &receipt.execution {
        ReceiptExecution::Abort { abort_data } => Some(decode_abort(&abort_data.0, metadata)),
        _ => None,
    }
}

pub(crate) fn print_failed_receipt_data(
    receipt: &TransactionReceipt,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    print_execution_data(&receipt.execution, decoded_abort, print);
    print.detail("gas used", receipt.gas_used);
}

fn print_execution_data(
    execution: &ReceiptExecution,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    match execution {
        ReceiptExecution::Return { return_data } => print.detail(
            "return data",
            format_args!("0x{}", hex::encode(&return_data.0)),
        ),
        ReceiptExecution::Abort { abort_data } => {
            print_abort_context(Some(abort_data), decoded_abort, print);
        }
        _ => {}
    }
}

fn print_abort_context(
    abort_data: Option<&thruput_json_rpc_types::hex::HexBytes>,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    if let Some(Ok(abort)) = decoded_abort {
        if let Some(label) = abort.exception_label() {
            print.detail("abort type", label);
        }
        print.detail("abort value", &abort.value);
        return;
    }
    if let Some(data) = abort_data {
        print.detail("abort data", format_args!("0x{}", hex::encode(&data.0)));
    }
    if let Some(Err(error)) = decoded_abort {
        print.detail("abort decode error", error);
    }
}

/// Insert decoded-abort fields into a command's `data` JSON payload.
///
/// A decoded abort contributes `abortException` and `abortValue`, plus
/// `abortExceptionCrate` / `abortExceptionVersion` when it resolved
/// through the ambient exception registry; a failed decode contributes
/// `abortDecodeError` instead.
pub(crate) fn insert_abort_json(
    data: &mut serde_json::Value,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
) {
    match decoded_abort {
        Some(Ok(abort)) => {
            data["abortException"] = serde_json::json!(abort.exception);
            data["abortValue"] = serde_json::json!(abort.value);
            if let Some(crate_name) = abort.exception_crate {
                data["abortExceptionCrate"] = serde_json::json!(crate_name);
            }
            if let Some(version) = abort.exception_version {
                data["abortExceptionVersion"] = serde_json::json!(version);
            }
        }
        Some(Err(error)) => {
            data["abortDecodeError"] = serde_json::json!(error.to_string());
        }
        None => {}
    }
}

pub fn nonce_from_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(1)
}

pub fn format_tx_hash(tx_hash: &TxHash) -> String {
    serde_json::to_string(tx_hash)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Keep the locally prepared hash in scrollback before a potentially interrupted wait.
/// Presentation is best-effort and never changes submission or validation behavior.
pub(crate) fn print_prepared_transaction(tx_hex: &str, print: &Print) {
    if print.is_json() || print.is_quiet() {
        return;
    }
    if let Ok(bytes) = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex))
        && let Ok(signed) = xdrs::txs::decode::<thruput_xdr::transaction::SignedTransaction>(&bytes)
        && let Ok(hash) = thruput_signing::transaction_hash(&signed.tx)
    {
        print.detail("prepared transaction", format_tx_hash(&TxHash(hash.into())));
    }
}

pub(crate) fn print_receipt_data(
    receipt: &TransactionReceipt,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    for event in &receipt.events {
        if let Ok(event) = serde_json::to_string(event) {
            print.detail("event", terminal_safe(&event));
        }
    }
    print_failed_receipt_data(receipt, decoded_abort, print);
}

pub async fn resolve_block_expiry_height(rpc_url: &str) -> Result<i64, CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    thruput_client::resolve_block_expiry_height(
        &thruput_client::NativeClient::new(client),
        DEFAULT_BLOCK_EXPIRY_OFFSET,
    )
    .await
    .map_err(|error| crate::native_rpc::rpc_error(&error))
}

pub fn ensure_tx_success(
    label: &str,
    receipt: &TransactionReceipt,
    tx_hash: &str,
) -> Result<(), CliError> {
    if matches!(receipt.execution, ReceiptExecution::Return { .. }) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "{label} {}; tx: {tx_hash}",
        summarize_exec_status(&receipt.execution),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_failures_keep_the_native_outcome() {
        use thruput_json_rpc_types::hex::HexBytes;
        for (execution, code) in [
            (
                ReceiptExecution::Abort {
                    abort_data: HexBytes(vec![]),
                },
                "transaction_reverted",
            ),
            (
                ReceiptExecution::Fault {},
                "transaction_deterministic_error",
            ),
            (
                ReceiptExecution::NonDeterministicError {},
                "transaction_nondeterministic_error",
            ),
            (
                ReceiptExecution::FailedGasSponsorship {},
                "transaction_failed_gas_sponsorship",
            ),
        ] {
            let error = command_error_for_exec_status(&execution).unwrap();
            let value = serde_json::to_value(error).unwrap();
            assert_eq!(value["code"], code);
            assert_eq!(value["details"], serde_json::to_value(execution).unwrap());
        }
        assert!(
            command_error_for_exec_status(&ReceiptExecution::Return {
                return_data: HexBytes(vec![])
            })
            .is_none()
        );
    }
}
