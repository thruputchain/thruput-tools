use groundhog_json_rpc_types::error::{TxInvalidDetail, codes};
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
use jsonrpsee::core::ClientError;
use jsonrpsee::types::error::METHOD_NOT_FOUND_CODE;

use crate::codec::{AbortDecodeError, CallMetadata, DecodedAbort, decode_abort};
use crate::result::CommandError;
use crate::terminal::terminal_safe;
use crate::{CliError, Print};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

/// Submission errors that establish the transaction was not admitted.
/// Backend unavailability can follow dispatch, so it remains uncertain.
pub(crate) fn is_conclusive_send_error(error: &ClientError) -> bool {
    let ClientError::Call(object) = error else {
        return false;
    };
    matches!(
        object.code(),
        codes::INVALID_PARAMS | codes::TX_INVALID | codes::TX_REJECTED | codes::OVERLOADED
    )
}

/// Human-readable one-line summary of an execution status.
pub fn summarize_exec_status(status: &ExecStatus) -> String {
    match status {
        ExecStatus::Ok => "succeeded".into(),
        ExecStatus::Reverted {
            message: Some(message),
        } => format!("reverted: {message}"),
        ExecStatus::Reverted { message: None } => "reverted".into(),
        ExecStatus::DeterministicError { kind }
            if kind == groundhog_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT =>
        {
            "failed with a deterministic execution fault (e.g. out-of-bounds access or \
             out of gas; the node does not report the specific cause)"
                .into()
        }
        ExecStatus::DeterministicError { kind } => {
            format!("failed with deterministic execution error ({kind})")
        }
        ExecStatus::NonDeterministicError => "failed with nondeterministic commit error".into(),
        ExecStatus::FailedGasSponsorship => "failed gas sponsorship".into(),
        ExecStatus::Other => "failed with unknown execution result".into(),
        _ => "failed with unsupported execution result".into(),
    }
}

/// Stable machine-readable error for an unsuccessful transaction receipt.
pub fn command_error_for_exec_status(status: &ExecStatus) -> Option<CommandError> {
    let code = match status {
        ExecStatus::Ok => return None,
        ExecStatus::Reverted { .. } => "transaction_reverted",
        ExecStatus::DeterministicError { .. } => "transaction_deterministic_error",
        ExecStatus::NonDeterministicError => "transaction_nondeterministic_error",
        ExecStatus::FailedGasSponsorship => "transaction_failed_gas_sponsorship",
        ExecStatus::Other => "transaction_unknown_error",
        _ => "transaction_unsupported_error",
    };
    Some(CommandError::new(
        code,
        format!("transaction {}", summarize_exec_status(status)),
        serde_json::to_value(status).unwrap_or(serde_json::Value::Null),
    ))
}

/// Decode the abort buffer retained on a receipt, when present.
pub(crate) fn decode_receipt_abort(
    receipt: &TransactionReceipt,
    metadata: Option<&CallMetadata>,
) -> Option<Result<DecodedAbort, AbortDecodeError>> {
    receipt
        .abort_data
        .as_ref()
        .map(|data| decode_abort(data.0.as_slice(), metadata))
}

/// Print return data, gas, and abort context retained on an unsuccessful
/// receipt.
///
/// A decoded abort prints as its exception type and value; the raw abort
/// buffer prints only when decoding failed (with the reason) or was not
/// attempted, mirroring how decoded return data elides the raw bytes.
pub(crate) fn print_failed_receipt_data(
    receipt: &TransactionReceipt,
    decoded_abort: Option<&Result<DecodedAbort, AbortDecodeError>>,
    print: &Print,
) {
    if let Some(data) = receipt.return_data.as_ref() {
        print.status(format_args!("  return data: 0x{}", hex::encode(&data.0)));
    }
    print.detail("gas used", receipt.gas_used);
    print_abort_context(receipt.abort_data.as_ref(), decoded_abort, print);
}

/// Print a receipt's abort context: the decoded exception when decoding
/// succeeded, otherwise the raw buffer plus the decode failure when one
/// was attempted.
fn print_abort_context(
    abort_data: Option<&groundhog_json_rpc_types::hex::HexBytes>,
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
        && let Ok(signed) =
            xdrs::txs::decode::<groundhog_xdr::transaction::SignedTransaction>(&bytes)
        && let Ok(hash) = groundhog_signing::transaction_hash(&signed.tx)
    {
        print.detail("prepared transaction", format_tx_hash(&TxHash(hash.into())));
    }
}

pub(crate) fn rpc_error_message(error: &ClientError) -> String {
    match error {
        ClientError::Call(object) => {
            format!(
                "{} (code {})",
                terminal_safe(object.message()),
                object.code()
            )
        }
        _ => terminal_safe(&error.to_string()),
    }
}

pub(crate) fn is_unsupported_rpc_method_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Call(object)
            if matches!(object.code(), METHOD_NOT_FOUND_CODE | codes::NOT_IMPLEMENTED)
    )
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
    if let Some(data) = &receipt.return_data {
        print.detail("return data", format_args!("0x{}", hex::encode(&data.0)));
    }
    print.detail("gas used", receipt.gas_used);
    print_abort_context(receipt.abort_data.as_ref(), decoded_abort, print);
}

pub async fn resolve_block_expiry_height(rpc_url: &str) -> Result<i64, CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    groundhog_client::resolve_block_expiry_height(&client, DEFAULT_BLOCK_EXPIRY_OFFSET)
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "chainTip",
            message: e.to_string(),
        })
}

pub async fn submit_and_poll(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let client = crate::http_client::rpc_client(rpc_url)?;
    groundhog_json_rpc_client::submit_and_poll_with_client_config(
        &client,
        tx_hex,
        groundhog_json_rpc_client::DEFAULT_RECEIPT_POLL_ATTEMPTS,
        groundhog_json_rpc_client::DEFAULT_RECEIPT_POLL_INTERVAL,
    )
    .await
    .map_err(map_submit_and_poll_error)
}

pub fn submit_and_poll_blocking(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    let rt =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    rt.block_on(async { submit_and_poll(rpc_url, tx_hex).await })
}

pub fn ensure_tx_success(
    label: &str,
    receipt: &TransactionReceipt,
    tx_hash: &str,
) -> Result<(), CliError> {
    if matches!(receipt.status, ExecStatus::Ok) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "{label} {}; tx: {tx_hash}",
        summarize_exec_status(&receipt.status),
    )))
}

pub(crate) fn map_submit_call_error(error: groundhog_client::SubmitCallError) -> CliError {
    match error {
        groundhog_client::SubmitCallError::Rpc { source, .. } => map_submit_and_poll_error(
            groundhog_json_rpc_client::SubmitAndPollError::SendRawTransaction(source),
        ),
        mismatch @ groundhog_client::SubmitCallError::HashMismatch { .. } => {
            CliError::Other(mismatch.to_string())
        }
    }
}

pub(crate) fn map_poll_receipt_error(
    error: groundhog_json_rpc_client::PollTransactionReceiptError,
) -> CliError {
    use groundhog_json_rpc_client::{
        PollTransactionReceiptError as Poll, SubmitAndPollError as Combined,
    };
    map_submit_and_poll_error(match error {
        Poll::Connect { rpc_url, source } => Combined::Connect { rpc_url, source },
        Poll::GetTransactionReceipt { source, .. } => Combined::GetTransactionReceipt(source),
        Poll::TransactionStatus { tx_hash, status } => {
            Combined::TransactionStatus { tx_hash, status }
        }
        Poll::ReceiptTimeout {
            tx_hash,
            attempts,
            last_status,
        } => Combined::ReceiptTimeout {
            tx_hash,
            attempts,
            last_status,
        },
        _ => return CliError::ReceiptPollingExhausted,
    })
}

fn map_submit_and_poll_error(error: groundhog_json_rpc_client::SubmitAndPollError) -> CliError {
    match error {
        groundhog_json_rpc_client::SubmitAndPollError::Connect { rpc_url, source } => {
            CliError::RpcConnect(format!("{rpc_url}: {source}"))
        }
        groundhog_json_rpc_client::SubmitAndPollError::SendRawTransaction(error) => {
            match violated_bound(&error) {
                Some(detail) => CliError::TransactionRejectedBeforeSubmission { detail },
                None => CliError::RpcMethodFailed {
                    method: "sendRawTransaction",
                    message: error.to_string(),
                },
            }
        }
        groundhog_json_rpc_client::SubmitAndPollError::GetTransactionReceipt(error) => {
            CliError::RpcMethodFailed {
                method: "getTransactionReceipt",
                message: error.to_string(),
            }
        }
        groundhog_json_rpc_client::SubmitAndPollError::TransactionStatus { tx_hash, status } => {
            match status {
                TransactionStatus::Invalid { reason } => CliError::TransactionInvalid {
                    tx_hash: format_tx_hash(&tx_hash),
                    reason,
                },
                TransactionStatus::Retry { error } => CliError::TransactionRetry {
                    tx_hash: format_tx_hash(&tx_hash),
                    error,
                },
                TransactionStatus::Dropped { error } => CliError::TransactionDropped {
                    tx_hash: format_tx_hash(&tx_hash),
                    error,
                },
                TransactionStatus::RecentFailure {
                    attempt_height,
                    cause,
                } => CliError::TransactionRecentFailure {
                    tx_hash: format_tx_hash(&tx_hash),
                    attempt_height: *attempt_height,
                    cause,
                },
                _ => CliError::ReceiptPollingExhausted,
            }
        }
        groundhog_json_rpc_client::SubmitAndPollError::ReceiptTimeout {
            tx_hash,
            attempts,
            last_status,
        } => CliError::ReceiptTimeout {
            tx_hash: format_tx_hash(&tx_hash),
            seconds: receipt_poll_duration_seconds(attempts),
            last_status: match last_status {
                Some(TransactionStatus::Unknown) => "unknown",
                Some(TransactionStatus::Pending) => "pending",
                Some(TransactionStatus::Committed { .. }) => "committed",
                Some(TransactionStatus::Invalid { .. }) => "invalid",
                Some(TransactionStatus::RecentFailure { .. }) => "recentFailure",
                Some(TransactionStatus::Retry { .. }) => "retry",
                Some(TransactionStatus::Dropped { .. }) => "dropped",
                None => "unavailable",
                Some(_) => "unrecognized",
            },
        },
        _ => CliError::ReceiptPollingExhausted,
    }
}

/// The bound a `sendRawTransaction` rejection names, when the endpoint's
/// pre-submission checks refused the transaction with a `TxInvalidDetail` in
/// the error data. A `TxInvalid` without readable detail (undecodable XDR, or
/// a reason this build does not know) is `None` and stays a plain method
/// failure.
fn violated_bound(error: &ClientError) -> Option<TxInvalidDetail> {
    let ClientError::Call(object) = error else {
        return None;
    };
    if object.code() != codes::TX_INVALID {
        return None;
    }
    serde_json::from_str(object.data()?.get()).ok()
}

fn receipt_poll_duration_seconds(attempts: usize) -> u64 {
    let attempts = u32::try_from(attempts).unwrap_or(u32::MAX);
    let duration =
        groundhog_json_rpc_client::DEFAULT_RECEIPT_POLL_INTERVAL.saturating_mul(attempts);
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_timeout_preserves_status_without_echoing_endpoint() {
        use groundhog_json_rpc_types::hex::Hex0x;
        for (status, expected) in [
            (Some(TransactionStatus::Unknown), "unknown"),
            (Some(TransactionStatus::Pending), "pending"),
            (None, "unavailable"),
        ] {
            let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x44; 32])));
            let error = map_submit_and_poll_error(
                groundhog_json_rpc_client::SubmitAndPollError::ReceiptTimeout {
                    tx_hash,
                    attempts: 300,
                    last_status: status,
                },
            );
            assert_eq!(error.code(), "receipt_timeout");
            assert!(
                error
                    .to_string()
                    .contains(&format!("last observed status: {expected}"))
            );
            let hint = error.hint().unwrap();
            assert!(hint.contains(&format!("thruput tx show {}", format_tx_hash(&tx_hash))));
            assert!(!hint.contains("--rpc-url"));
            assert!(hint.contains("same profile and RPC endpoint as the original submission"));
            assert!(hint.contains("sending wallet's HOG balance on the same network"));
            assert!(hint.contains("before submitting the operation again"));
            assert!(!hint.contains("rerun"));
            assert!(hint.contains("node terminal or redirected output"));
            assert!(!hint.contains('\n'));
        }
    }

    #[test]
    fn fault_kind_summarizes_with_bundled_causes() {
        let summary = summarize_exec_status(&ExecStatus::DeterministicError {
            kind: groundhog_json_rpc_types::receipt::DETERMINISTIC_ERROR_KIND_FAULT.into(),
        });
        assert_eq!(
            summary,
            "failed with a deterministic execution fault (e.g. out-of-bounds access or \
             out of gas; the node does not report the specific cause)"
        );
    }

    #[test]
    fn specific_deterministic_error_kind_summarizes_verbatim() {
        let summary = summarize_exec_status(&ExecStatus::DeterministicError {
            kind: "outOfGas".into(),
        });
        assert_eq!(
            summary,
            "failed with deterministic execution error (outOfGas)"
        );
    }

    #[test]
    fn unsuccessful_exec_statuses_have_stable_error_codes() {
        let statuses = [
            (
                ExecStatus::Reverted {
                    message: Some("nope".into()),
                },
                "transaction_reverted",
            ),
            (
                ExecStatus::DeterministicError {
                    kind: "outOfGas".into(),
                },
                "transaction_deterministic_error",
            ),
            (
                ExecStatus::NonDeterministicError,
                "transaction_nondeterministic_error",
            ),
            (
                ExecStatus::FailedGasSponsorship,
                "transaction_failed_gas_sponsorship",
            ),
            (ExecStatus::Other, "transaction_unknown_error"),
        ];

        for (status, expected_code) in statuses {
            let error = command_error_for_exec_status(&status)
                .expect("bug: unsuccessful receipt status must produce an error");
            let value = serde_json::to_value(error).unwrap();
            assert_eq!(value["code"], expected_code);
            assert_eq!(value["details"], serde_json::to_value(status).unwrap());
        }
        assert!(command_error_for_exec_status(&ExecStatus::Ok).is_none());
    }

    #[test]
    fn recent_bounds_failure_maps_to_actionable_cli_error() {
        use groundhog_json_rpc_types::{
            BlockHeight,
            hex::{Hex0x, HexBytes},
            transaction::{ConflictCode, ConflictDiagnostic},
        };

        let tx_hash = TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x44; 32])));
        let error = map_submit_and_poll_error(
            groundhog_json_rpc_client::SubmitAndPollError::TransactionStatus {
                tx_hash,
                status: TransactionStatus::RecentFailure {
                    attempt_height: BlockHeight::from(17),
                    cause:
                        groundhog_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                            conflict: ConflictDiagnostic {
                                address: Hex0x(groundhog_xdr::types::ContractAddress::from(
                                    [0x55; 32],
                                )),
                                key: HexBytes(vec![0, 0, 0, 1]),
                                code: ConflictCode::BoundsExceeded,
                            },
                        },
                },
            },
        );

        assert_eq!(error.code(), "transaction_recent_failure");
        assert!(error.to_string().contains("boundsExceeded"));
        assert!(error.hint().expect("hint expected").contains("airdrop"));
        let details = error.details_json().expect("details expected");
        assert_eq!(details["attemptHeight"], 17);
        assert_eq!(details["cause"]["conflict"]["key"], "0x00000001");
        assert_eq!(details["cause"]["conflict"]["code"], "boundsExceeded");
    }

    #[test]
    fn contract_rejection_preserves_diagnostic_in_cli_error() {
        use groundhog_json_rpc_types::transaction::{ContractRejectCode, TransactionFailureCause};
        let error = map_submit_and_poll_error(
            groundhog_json_rpc_client::SubmitAndPollError::TransactionStatus {
                tx_hash: TxHash(groundhog_json_rpc_types::hex::Hex0x(
                    groundhog_xdr::types::SHA256Hash::from([0x44; 32]),
                )),
                status: TransactionStatus::RecentFailure {
                    attempt_height: 17.into(),
                    cause: TransactionFailureCause::ContractRejected {
                        contract_index: 2,
                        code: ContractRejectCode::GasScheduleMismatch,
                    },
                },
            },
        );
        assert_eq!(error.code(), "transaction_recent_failure");
        assert!(
            error
                .to_string()
                .contains("contract attachment 2 rejected: gasScheduleMismatch")
        );
        assert!(error.hint().unwrap().contains("rebuild artifacts"));
        assert_eq!(
            error.details_json().unwrap()["cause"],
            serde_json::json!({
                "kind": "contractRejected", "contractIndex": 2, "code": "gasScheduleMismatch"
            })
        );
    }

    #[test]
    fn default_receipt_timeout_stays_thirty_seconds() {
        assert_eq!(
            receipt_poll_duration_seconds(groundhog_json_rpc_client::DEFAULT_RECEIPT_POLL_ATTEMPTS,),
            30
        );
    }
}
