// Typed JSON-RPC client, generated from the shared json-rpc-api trait.
//
// Re-exports json-rpc-types so consumers (SDK, CLI) only need this crate.
pub use groundhog_json_rpc_types as types;

pub use groundhog_json_rpc_api::GroundhogApiClient;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::TxHash;
pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use std::time::Duration;

pub const DEFAULT_RECEIPT_POLL_ATTEMPTS: usize = 30;
pub const DEFAULT_RECEIPT_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum SubmitAndPollError {
    #[error("failed to connect to JSON-RPC endpoint {rpc_url}: {source}")]
    Connect {
        rpc_url: String,
        #[source]
        source: jsonrpsee::core::client::Error,
    },
    #[error("groundhog_sendRawTransaction failed: {0}")]
    SendRawTransaction(jsonrpsee::core::client::Error),
    #[error("groundhog_getTransactionReceipt failed: {0}")]
    GetTransactionReceipt(jsonrpsee::core::client::Error),
    #[error("timed out waiting for receipt for {tx_hash:?} after {attempts} attempts")]
    ReceiptTimeout { tx_hash: TxHash, attempts: usize },
}

pub async fn submit_and_poll(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<(TxHash, TransactionReceipt), SubmitAndPollError> {
    submit_and_poll_with_config(
        rpc_url,
        tx_hex,
        DEFAULT_RECEIPT_POLL_ATTEMPTS,
        DEFAULT_RECEIPT_POLL_INTERVAL,
    )
    .await
}

pub async fn submit_and_poll_with_config(
    rpc_url: &str,
    tx_hex: &str,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<(TxHash, TransactionReceipt), SubmitAndPollError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|source| SubmitAndPollError::Connect {
            rpc_url: rpc_url.to_owned(),
            source,
        })?;

    let tx_hash = client
        .send_raw_transaction(tx_hex.to_owned())
        .await
        .map_err(SubmitAndPollError::SendRawTransaction)?;

    for attempt in 0..max_attempts {
        match client.get_transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => return Ok((tx_hash, receipt)),
            Ok(None) if attempt < max_attempts.saturating_sub(1) => {
                tokio::time::sleep(poll_interval).await;
            }
            Ok(None) => {
                return Err(SubmitAndPollError::ReceiptTimeout {
                    tx_hash,
                    attempts: max_attempts,
                });
            }
            Err(error) => return Err(SubmitAndPollError::GetTransactionReceipt(error)),
        }
    }

    Err(SubmitAndPollError::ReceiptTimeout {
        tx_hash,
        attempts: max_attempts,
    })
}
