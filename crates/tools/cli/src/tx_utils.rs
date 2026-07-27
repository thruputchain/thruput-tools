use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};

use crate::codec::{AbortDecodeError, CallMetadata, DecodedAbort, decode_abort};
use crate::result::CommandError;
use crate::{CliError, Print};

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

/// Human-readable one-line summary of an execution status.
pub fn summarize_exec_status(status: &ExecStatus) -> String {
    match status {
        ExecStatus::Ok => "succeeded".into(),
        ExecStatus::Reverted {
            message: Some(message),
        } => format!("reverted: {message}"),
        ExecStatus::Reverted { message: None } => "reverted".into(),
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

/// Print return and abort context retained on an unsuccessful receipt.
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
    if let Some(Ok(abort)) = decoded_abort {
        if let Some(exception) = abort.exception.as_deref() {
            print.detail("abort type", exception);
        }
        print.detail("abort value", &abort.value);
        return;
    }
    if let Some(data) = receipt.abort_data.as_ref() {
        print.status(format_args!("  abort data: 0x{}", hex::encode(&data.0)));
    }
    if let Some(Err(error)) = decoded_abort {
        print.detail("abort decode error", error);
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

/// Escape control and bidirectional-formatting characters in terminal output.
pub(crate) fn human_safe(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
        {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

pub async fn resolve_block_expiry_height(rpc_url: &str) -> Result<i64, CliError> {
    use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};

    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;
    let tip = client
        .chain_tip()
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "chainTip",
            message: e.to_string(),
        })?;
    Ok((*tip.height).saturating_add(DEFAULT_BLOCK_EXPIRY_OFFSET))
}

pub async fn submit_and_poll(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<(TxHash, TransactionReceipt), CliError> {
    groundhog_json_rpc_client::submit_and_poll(rpc_url, tx_hex)
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

fn map_submit_and_poll_error(error: groundhog_json_rpc_client::SubmitAndPollError) -> CliError {
    match error {
        groundhog_json_rpc_client::SubmitAndPollError::Connect { rpc_url, source } => {
            CliError::RpcConnect(format!("{rpc_url}: {source}"))
        }
        groundhog_json_rpc_client::SubmitAndPollError::SendRawTransaction(error) => {
            CliError::RpcMethodFailed {
                method: "sendRawTransaction",
                message: error.to_string(),
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
                TransactionStatus::RecentFailure {
                    attempt_height,
                    conflict,
                } => CliError::TransactionRecentFailure {
                    tx_hash: format_tx_hash(&tx_hash),
                    attempt_height: *attempt_height,
                    conflict_address: serde_json::to_value(conflict.address)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("{:?}", conflict.address)),
                    conflict_key: format!("0x{}", hex::encode(conflict.key.0)),
                    conflict_code: serde_json::to_value(conflict.code)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| format!("{:?}", conflict.code)),
                },
                _ => CliError::ReceiptPollingExhausted,
            }
        }
        groundhog_json_rpc_client::SubmitAndPollError::ReceiptTimeout { tx_hash, attempts } => {
            CliError::ReceiptTimeout {
                tx_hash: format_tx_hash(&tx_hash),
                seconds: receipt_poll_duration_seconds(attempts),
            }
        }
        _ => CliError::ReceiptPollingExhausted,
    }
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
                    conflict: ConflictDiagnostic {
                        address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x55; 32])),
                        key: HexBytes(vec![0, 0, 0, 1]),
                        code: ConflictCode::BoundsExceeded,
                    },
                },
            },
        );

        assert_eq!(error.code(), "transaction_recent_failure");
        assert!(error.to_string().contains("boundsExceeded"));
        assert!(error.hint().expect("hint expected").contains("airdrop"));
        let details = error.details_json().expect("details expected");
        assert_eq!(details["attemptHeight"], 17);
        assert_eq!(details["conflict"]["key"], "0x00000001");
        assert_eq!(details["conflict"]["code"], "boundsExceeded");
    }

    #[test]
    fn default_receipt_timeout_stays_thirty_seconds() {
        assert_eq!(
            receipt_poll_duration_seconds(groundhog_json_rpc_client::DEFAULT_RECEIPT_POLL_ATTEMPTS,),
            30
        );
    }
}
