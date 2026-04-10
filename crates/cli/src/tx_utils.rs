use groundhog_rpc_api_types::receipt::TransactionReceipt;
use groundhog_rpc_api_types::transaction::TxHash;

use crate::CliError;

const DEFAULT_BLOCK_EXPIRY_OFFSET: i64 = 1_000;

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

pub fn poll_exhausted_error() -> CliError {
    CliError::ReceiptPollingExhausted
}

pub async fn resolve_block_expiry_height(rpc_url: &str) -> Result<i64, CliError> {
    use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};

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
    use groundhog_rpc_client::{GroundhogApiClient, HttpClientBuilder};

    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|e| CliError::RpcConnect(format!("{rpc_url}: {e}")))?;

    let tx_hash = client
        .send_raw_transaction(tx_hex.to_string())
        .await
        .map_err(|e| CliError::RpcMethodFailed {
            method: "sendRawTransaction",
            message: e.to_string(),
        })?;

    let max_attempts = 30;
    let poll_interval = std::time::Duration::from_secs(1);
    for attempt in 0..max_attempts {
        match client.get_transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => return Ok((tx_hash, receipt)),
            Ok(None) if attempt < max_attempts - 1 => {
                tokio::time::sleep(poll_interval).await;
            }
            Ok(None) => {
                return Err(CliError::ReceiptTimeout {
                    tx_hash: format_tx_hash(&tx_hash),
                    seconds: max_attempts,
                });
            }
            Err(e) => {
                return Err(CliError::RpcMethodFailed {
                    method: "getTransactionReceipt",
                    message: e.to_string(),
                });
            }
        }
    }

    Err(poll_exhausted_error())
}
