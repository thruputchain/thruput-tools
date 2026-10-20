// Typed JSON-RPC client, generated from the shared json-rpc-api trait.
//
// Re-exports json-rpc-types so consumers (SDK, CLI) only need this crate.
pub use thruput_json_rpc_types as types;
// Native result readers are available before production method migration.
pub use thruput_json_rpc_types::error::native::DecodedError;
pub use thruput_json_rpc_types::responses::DecodedResponse;

pub mod receipt;

use jsonrpsee::core::client::ClientT;
pub use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use std::time::Duration;
pub use thruput_json_rpc_api::{AirdropApiClient, GroundhogApiClient, LocalFundingApiClient};
use thruput_json_rpc_types::receipt::TransactionReceipt;
use thruput_json_rpc_types::transaction::{TransactionStatus, TxHash};

pub use receipt::{
    DEFAULT_RECEIPT_POLL_ATTEMPTS, DEFAULT_RECEIPT_POLL_INTERVAL, ReceiptPollConfig,
    ReceiptPollError, ReceiptSource, poll_receipt,
};

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
#[non_exhaustive]
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
    ReceiptTimeout {
        tx_hash: TxHash,
        attempts: usize,
        /// Last successfully observed status; absent if status was unavailable.
        last_status: Option<TransactionStatus>,
    },
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
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
    ReceiptTimeout {
        tx_hash: TxHash,
        attempts: usize,
        /// Last successfully observed status; absent if status was unavailable.
        last_status: Option<TransactionStatus>,
    },
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
    submit_and_poll_with_client_config(&client, tx_hex, max_attempts, poll_interval).await
}

/// Submit and poll using a caller-provided JSON-RPC client.
pub async fn submit_and_poll_with_client_config<C: ClientT + Sync>(
    client: &C,
    tx_hex: &str,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<(TxHash, TransactionReceipt), SubmitAndPollError> {
    let tx_hash = submit_transaction_with_client(client, tx_hex)
        .await
        .map_err(SubmitAndPollError::SendRawTransaction)?;
    let receipt =
        poll_transaction_receipt_with_client_config(client, tx_hash, max_attempts, poll_interval)
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
                PollTransactionReceiptError::ReceiptTimeout {
                    tx_hash,
                    attempts,
                    last_status,
                } => SubmitAndPollError::ReceiptTimeout {
                    tx_hash,
                    attempts,
                    last_status,
                },
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
    submit_transaction_with_client(&client, tx_hex)
        .await
        .map_err(SubmitTransactionError::SendRawTransaction)
}

/// Submit a signed transaction using a caller-provided JSON-RPC client.
pub async fn submit_transaction_with_client<C: ClientT + Sync>(
    client: &C,
    tx_hex: &str,
) -> Result<TxHash, jsonrpsee::core::client::Error> {
    client.send_raw_transaction(tx_hex.to_owned()).await
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
/// An `Invalid`, `Retry`, or `Dropped` status ends polling early. A best-effort `RecentFailure`
/// diagnostic is retained while polling continues because the same transaction
/// may still commit; if no receipt appears, the diagnostic is returned after
/// polling is exhausted.
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
    poll_transaction_receipt_with_client_config(&client, tx_hash, max_attempts, poll_interval).await
}

/// Poll using a caller-provided JSON-RPC client and explicit retry settings.
pub async fn poll_transaction_receipt_with_client_config<C: ClientT + Sync>(
    client: &C,
    tx_hash: TxHash,
    max_attempts: usize,
    poll_interval: Duration,
) -> Result<TransactionReceipt, PollTransactionReceiptError> {
    poll_receipt(
        client,
        tx_hash,
        ReceiptPollConfig::attempts(max_attempts, poll_interval),
    )
    .await
    .map_err(|error| match error {
        ReceiptPollError::Lookup { tx_hash, source } => {
            PollTransactionReceiptError::GetTransactionReceipt { tx_hash, source }
        }
        ReceiptPollError::Status { tx_hash, status } => {
            PollTransactionReceiptError::TransactionStatus { tx_hash, status }
        }
        ReceiptPollError::Timeout {
            tx_hash,
            attempts,
            last_status,
        } => PollTransactionReceiptError::ReceiptTimeout {
            tx_hash,
            attempts,
            last_status,
        },
    })
}

/// Candidate native transport; production activation is separate.
pub mod native;
