// Typed JSON-RPC client, generated from the shared json-rpc-api trait.
//
// Re-exports json-rpc-types so consumers (SDK, CLI) only need this crate.
pub use groundhog_json_rpc_types as types;

pub use groundhog_json_rpc_api::GroundhogApiClient;
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::transaction::{TransactionStatus, TxHash};
pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use std::time::Duration;

pub const DEFAULT_RECEIPT_POLL_ATTEMPTS: usize = 30;
pub const DEFAULT_RECEIPT_POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum SubmitTransactionError {
    #[error("failed to connect to JSON-RPC endpoint {rpc_url}: {source}")]
    Connect {
        rpc_url: String,
        #[source]
        source: jsonrpsee::core::client::Error,
    },
    #[error("groundhog_sendRawTransaction failed: {0}")]
    SendRawTransaction(jsonrpsee::core::client::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum PollTransactionReceiptError {
    #[error("failed to connect to JSON-RPC endpoint {rpc_url}: {source}")]
    Connect {
        rpc_url: String,
        #[source]
        source: jsonrpsee::core::client::Error,
    },
    #[error("groundhog_getTransactionReceipt failed for {tx_hash:?}: {source}")]
    GetTransactionReceipt {
        tx_hash: TxHash,
        #[source]
        source: jsonrpsee::core::client::Error,
    },
    #[error("transaction {tx_hash:?} reported failure status {status:?}")]
    TransactionStatus {
        tx_hash: TxHash,
        status: TransactionStatus,
    },
    #[error("timed out waiting for receipt for {tx_hash:?} after {attempts} attempts")]
    ReceiptTimeout { tx_hash: TxHash, attempts: usize },
}

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
    #[error("transaction {tx_hash:?} reported failure status {status:?}")]
    TransactionStatus {
        tx_hash: TxHash,
        status: TransactionStatus,
    },
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
    let tx_hash = submit_transaction(rpc_url, tx_hex)
        .await
        .map_err(|error| match error {
            SubmitTransactionError::Connect { rpc_url, source } => {
                SubmitAndPollError::Connect { rpc_url, source }
            }
            SubmitTransactionError::SendRawTransaction(source) => {
                SubmitAndPollError::SendRawTransaction(source)
            }
        })?;
    let receipt =
        poll_transaction_receipt_with_config(rpc_url, tx_hash, max_attempts, poll_interval)
            .await
            .map_err(|error| match error {
                PollTransactionReceiptError::Connect { rpc_url, source } => {
                    SubmitAndPollError::Connect { rpc_url, source }
                }
                PollTransactionReceiptError::GetTransactionReceipt { source, .. } => {
                    SubmitAndPollError::GetTransactionReceipt(source)
                }
                PollTransactionReceiptError::TransactionStatus { tx_hash, status } => {
                    SubmitAndPollError::TransactionStatus { tx_hash, status }
                }
                PollTransactionReceiptError::ReceiptTimeout { tx_hash, attempts } => {
                    SubmitAndPollError::ReceiptTimeout { tx_hash, attempts }
                }
            })?;
    Ok((tx_hash, receipt))
}

/// Submit a signed transaction and return once the RPC endpoint accepts it.
pub async fn submit_transaction(
    rpc_url: &str,
    tx_hex: &str,
) -> Result<TxHash, SubmitTransactionError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|source| SubmitTransactionError::Connect {
            rpc_url: rpc_url.to_owned(),
            source,
        })?;
    client
        .send_raw_transaction(tx_hex.to_owned())
        .await
        .map_err(SubmitTransactionError::SendRawTransaction)
}

/// Poll an accepted transaction until its receipt is available or its status
/// reports a rejection or recent commit failure.
pub async fn poll_transaction_receipt(
    rpc_url: &str,
    tx_hash: TxHash,
) -> Result<TransactionReceipt, PollTransactionReceiptError> {
    poll_transaction_receipt_with_config(
        rpc_url,
        tx_hash,
        DEFAULT_RECEIPT_POLL_ATTEMPTS,
        DEFAULT_RECEIPT_POLL_INTERVAL,
    )
    .await
}

/// Poll an accepted transaction with explicit retry settings.
///
/// Receipts are authoritative. When a receipt is not yet available, an
/// `Invalid` or best-effort `RecentFailure` status ends polling early so callers
/// can report the diagnostic before it expires.
pub async fn poll_transaction_receipt_with_config(
    rpc_url: &str,
    tx_hash: TxHash,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<TransactionReceipt, PollTransactionReceiptError> {
    let client = HttpClientBuilder::default()
        .build(rpc_url)
        .map_err(|source| PollTransactionReceiptError::Connect {
            rpc_url: rpc_url.to_owned(),
            source,
        })?;

    for attempt in 0..max_attempts {
        match client.get_transaction_receipt(tx_hash).await {
            Ok(Some(receipt)) => return Ok(receipt),
            Ok(None) => match client.get_transaction_status(tx_hash).await {
                Ok(
                    status @ (TransactionStatus::Invalid { .. }
                    | TransactionStatus::RecentFailure { .. }),
                ) => {
                    return Err(PollTransactionReceiptError::TransactionStatus { tx_hash, status });
                }
                // Receipt polling remains compatible with endpoints that do not
                // implement status, and receipts remain authoritative when status
                // is unknown, pending, or briefly ahead of receipt indexing.
                Ok(_) | Err(_) if attempt < max_attempts.saturating_sub(1) => {
                    tokio::time::sleep(poll_interval).await;
                }
                Ok(_) | Err(_) => {
                    return Err(PollTransactionReceiptError::ReceiptTimeout {
                        tx_hash,
                        attempts: max_attempts,
                    });
                }
            },
            Err(source) => {
                return Err(PollTransactionReceiptError::GetTransactionReceipt { tx_hash, source });
            }
        }
    }

    Err(PollTransactionReceiptError::ReceiptTimeout {
        tx_hash,
        attempts: max_attempts,
    })
}
