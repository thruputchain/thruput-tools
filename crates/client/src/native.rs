//! Native JSON-RPC invocation helpers, separate from wallet account selection.
//!
//! Callers verify endpoint compatibility and a trusted network pin before signing.
//! These helpers never discover configuration, select a wire protocol, or retry writes.

use std::future::Future;

use jsonrpsee::core::client::ClientT;
use thruput_json_rpc_client::native::{
    NativeClient, NativeClientError,
    receipt::{ReceiptPollConfig, ReceiptPollError, poll_receipt},
};
use thruput_json_rpc_types::{
    NetworkHash,
    hex::HexBytes,
    requests::{EmptyRequest, SendTransactionRequest},
    responses::{self, DecodedResponse},
    transaction::TxHash,
};
use thruput_xdr::types::NetworkHash as SigningNetworkHash;

use crate::SignedCall;

/// A native read or endpoint preparation failure. Original application errors
/// and unfamiliar results remain available to the embedding application.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeRpcError {
    #[error("{method} failed: {source}")]
    Call {
        method: &'static str,
        #[source]
        source: Box<NativeClientError>,
    },
    #[error("unsupported {method} result")]
    Unsupported {
        method: &'static str,
        raw: serde_json::Value,
    },
    #[error("unsupported {method} response representation")]
    ResponseRepresentation { method: &'static str },
    /// Missing bootstrap method or malformed discovery, including responses
    /// without Thruput's required `wireFormat` field.
    #[error("endpoint does not provide valid Thruput discovery: {source}")]
    InvalidDiscovery {
        #[source]
        source: Box<NativeClientError>,
    },
    #[error("incompatible native RPC wire format: {actual}; expected major 14")]
    WireFormat { actual: String },
    #[error("native RPC endpoint does not advertise required methods: {methods:?}")]
    MissingMethods { methods: Vec<String> },
    #[error("network identity mismatch: expected {expected:?}, received {actual:?}")]
    NetworkMismatch {
        expected: SigningNetworkHash,
        actual: SigningNetworkHash,
    },
    #[error("invalid transaction expiry: tip {tip}, offset {offset}")]
    InvalidExpiry { tip: i64, offset: i64 },
}

fn known<T>(
    result: Result<DecodedResponse<T>, NativeClientError>,
    method: &'static str,
) -> Result<T, NativeRpcError> {
    match result {
        Ok(DecodedResponse::Known(value)) => Ok(value),
        Ok(DecodedResponse::Unsupported { raw }) => {
            Err(NativeRpcError::Unsupported { method, raw })
        }
        Ok(_) => Err(NativeRpcError::ResponseRepresentation { method }),
        Err(source) => Err(NativeRpcError::Call {
            method,
            source: Box::new(source),
        }),
    }
}

/// Require native wire major 14 and every caller-selected method before use.
/// Discovery is queried once; there is no legacy-name fallback. Missing or
/// malformed discovery fields are incompatible discovery errors retaining the
/// original cause; transport outages remain ordinary call failures.
pub async fn check_endpoint<C: ClientT + Sync>(
    client: &NativeClient<C>,
    required_methods: &[&str],
) -> Result<responses::VersionInfo, NativeRpcError> {
    let version = known(
        client.get_version_info(EmptyRequest {}).await,
        "getVersionInfo",
    )
    .map_err(|error| match error {
        NativeRpcError::Call { source, .. } if invalid_discovery(&source) => {
            NativeRpcError::InvalidDiscovery { source }
        }
        other => other,
    })?;
    if !compatible_wire_format(&version.wire_format) {
        return Err(NativeRpcError::WireFormat {
            actual: version.wire_format,
        });
    }
    let mut missing: Vec<_> = required_methods
        .iter()
        .filter(|required| {
            !version
                .supported_methods
                .iter()
                .any(|name| name == **required)
        })
        .map(|name| (*name).to_owned())
        .collect();
    missing.sort_unstable();
    missing.dedup();
    if !missing.is_empty() {
        return Err(NativeRpcError::MissingMethods { methods: missing });
    }
    Ok(version)
}

fn invalid_discovery(error: &NativeClientError) -> bool {
    match error {
        NativeClientError::Transport(jsonrpsee::core::client::Error::ParseError(_))
        | NativeClientError::InvalidResponse { .. } => true,
        NativeClientError::Application { source, .. } => {
            source.code() == jsonrpsee::types::error::METHOD_NOT_FOUND_CODE
        }
        _ => false,
    }
}

fn compatible_wire_format(value: &str) -> bool {
    let mut parts = value.split('.');
    let mut version = [0_u64; 3];
    for component in &mut version {
        let Some(text) = parts.next() else {
            return false;
        };
        if text.is_empty()
            || (text.len() > 1 && text.starts_with('0'))
            || !text.bytes().all(|b| b.is_ascii_digit())
        {
            return false;
        }
        let Ok(number) = text.parse() else {
            return false;
        };
        *component = number;
    }
    parts.next().is_none() && version[0] == 14
}

/// Verify signing identity against an independently configured network hash.
/// A matching network hash does not distinguish resets of the same chain.
pub async fn validate_network<C: ClientT + Sync>(
    client: &NativeClient<C>,
    expected: &SigningNetworkHash,
) -> Result<responses::NetworkInfo, NativeRpcError> {
    let info = known(
        client.get_network_info(EmptyRequest {}).await,
        "getNetworkInfo",
    )?;
    let actual = SigningNetworkHash::from(info.network_hash.0);
    if &actual != expected {
        return Err(NativeRpcError::NetworkMismatch {
            expected: expected.clone(),
            actual,
        });
    }
    Ok(info)
}

/// Add a positive expiry offset to a committed tip, rejecting overflow rather
/// than silently changing the caller's requested validity window.
pub async fn resolve_block_expiry_height<C: ClientT + Sync>(
    client: &NativeClient<C>,
    offset: i64,
) -> Result<i64, NativeRpcError> {
    let tip = known(client.get_chain_tip(EmptyRequest {}).await, "getChainTip")?;
    let height = *tip.block_height;
    if height < 0 || offset <= 0 {
        return Err(NativeRpcError::InvalidExpiry {
            tip: height,
            offset,
        });
    }
    height
        .checked_add(offset)
        .ok_or(NativeRpcError::InvalidExpiry {
            tip: height,
            offset,
        })
}

/// Native submission failure retaining the canonical identity prepared locally.
/// Neither an RPC failure nor an unfamiliar acknowledgement proves non-admission.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubmitCallError {
    #[error("sendTransaction failed for {tx_hash:?}: {source}")]
    Rpc {
        tx_hash: TxHash,
        #[source]
        source: Box<NativeClientError>,
    },
    #[error("unsupported sendTransaction result for {tx_hash:?}")]
    Unsupported {
        tx_hash: TxHash,
        raw: serde_json::Value,
    },
    #[error("unsupported sendTransaction response representation for {tx_hash:?}")]
    ResponseRepresentation { tx_hash: TxHash },
    #[error("submitted transaction {tx_hash:?}, but endpoint returned {returned:?}")]
    HashMismatch { tx_hash: TxHash, returned: TxHash },
    #[error("invalid locally signed transaction encoding for {tx_hash:?}: {source}")]
    Encoding {
        tx_hash: TxHash,
        #[source]
        source: hex::FromHexError,
    },
}

impl SignedCall {
    /// Submit these signed bytes exactly once through the native RPC contract.
    /// The caller must verify compatibility and network identity before signing.
    pub async fn submit<C: ClientT + Sync>(
        &self,
        client: &NativeClient<C>,
    ) -> Result<SubmittedCall, SubmitCallError> {
        submit_signed_transaction(client, self.tx_hash(), self.tx_hex()).await
    }
}

/// Submit retained signed bytes once and verify the acknowledgement's transaction hash.
pub async fn submit_signed_transaction<C: ClientT + Sync>(
    client: &NativeClient<C>,
    tx_hash: TxHash,
    tx_hex: &str,
) -> Result<SubmittedCall, SubmitCallError> {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(tx_hex))
        .map_err(|source| SubmitCallError::Encoding { tx_hash, source })?;
    let result = client
        .send_transaction(SendTransactionRequest {
            signed_transaction: HexBytes(bytes),
        })
        .await
        .map_err(|source| SubmitCallError::Rpc {
            tx_hash,
            source: Box::new(source),
        })?;
    let acknowledgement = match result {
        DecodedResponse::Known(value) => value,
        DecodedResponse::Unsupported { raw } => {
            return Err(SubmitCallError::Unsupported { tx_hash, raw });
        }
        _ => return Err(SubmitCallError::ResponseRepresentation { tx_hash }),
    };
    if acknowledgement.transaction_hash != tx_hash {
        return Err(SubmitCallError::HashMismatch {
            tx_hash,
            returned: acknowledgement.transaction_hash,
        });
    }
    Ok(SubmittedCall { tx_hash })
}

/// A transaction hash that can be observed without resubmitting its envelope.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct SubmittedCall {
    pub tx_hash: TxHash,
}

impl SubmittedCall {
    /// Resume observation of a retained hash after an uncertain submission or restart.
    /// Construction does not claim that the transaction was admitted or executed.
    #[must_use]
    pub const fn new(tx_hash: TxHash) -> Self {
        Self { tx_hash }
    }

    /// Wait read-only for this hash on the same caller-verified chain instance.
    /// Failed execution receipts are returned intact. Cancellation and timeout
    /// retain the native waiter's commitment, status and original error evidence.
    pub async fn wait<C, F>(
        &self,
        client: &NativeClient<C>,
        network_hash: NetworkHash,
        config: ReceiptPollConfig,
        cancelled: F,
    ) -> Result<responses::TransactionReceipt, ReceiptPollError>
    where
        C: ClientT + Sync,
        F: Future<Output = ()>,
    {
        poll_receipt(client, network_hash, self.tx_hash, None, config, cancelled).await
    }
}

#[cfg(test)]
mod tests;
