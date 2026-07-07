use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TxHash;

use crate::CliError;

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

/// Human-readable one-line summary of an execution status.
pub fn summarize_exec_status(status: &ExecStatus) -> String {
    match status {
        ExecStatus::Ok => "succeeded".into(),
        ExecStatus::Reverted {
            message: Some(message),
        } => format!("reverted: {message}"),
        ExecStatus::Reverted { message: None } => "reverted without abort data".into(),
        ExecStatus::DeterministicError { kind } => {
            format!("failed with deterministic execution error ({kind})")
        }
        ExecStatus::NonDeterministicError => "failed with nondeterministic commit error".into(),
        ExecStatus::FailedGasSponsorship => "failed gas sponsorship".into(),
        ExecStatus::Other => "failed with unknown execution result".into(),
        _ => "failed with unsupported execution result".into(),
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
        groundhog_json_rpc_client::SubmitAndPollError::ReceiptTimeout { tx_hash, attempts } => {
            CliError::ReceiptTimeout {
                tx_hash: format_tx_hash(&tx_hash),
                seconds: u64::try_from(attempts).unwrap_or(u64::MAX),
            }
        }
    }
}
