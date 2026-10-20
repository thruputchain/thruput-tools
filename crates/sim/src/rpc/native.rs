//! Native transport with an explicit projection into the simulator's stable report models.
//! Unsupported payloads fail with their raw evidence; no legacy RPC method is dispatched.
use super::{
    ChainTip, ContractAddress, EventPage, EventQuery, HexBytes, SimRpcError, StorageItemRead,
    TransactionReceipt, TxHash, ViewRequest, ViewResponse,
};
use serde_json::json;
use thruput_json_rpc_client::{
    HttpClient,
    native::{NativeClient, NativeClientError},
};
use thruput_json_rpc_types::{
    events::{BlockAnchor, EmittedEvent, IdentifiedEvent, IterationOrder, PaginationCursor},
    receipt::ExecStatus,
    requests as req, responses as res,
    view::{RootInvocationOutput, ViewAuthClaim},
};

pub(crate) fn known<T>(
    method: &'static str,
    result: Result<res::DecodedResponse<T>, NativeClientError>,
) -> Result<T, SimRpcError> {
    match result.map_err(|source| match source {
        NativeClientError::InvalidResponse { raw, source, .. } => SimRpcError::InvalidResponse {
            method,
            message: source.to_string(),
            raw: *raw,
        },
        source => SimRpcError::Native {
            method,
            source: Box::new(source),
        },
    })? {
        res::DecodedResponse::Known(value) => Ok(value),
        res::DecodedResponse::Unsupported { raw } => Err(SimRpcError::InvalidResponse {
            method,
            message: "unsupported native response".into(),
            raw,
        }),
        _ => Err(SimRpcError::Unsupported { method }),
    }
}
fn emitted(event: res::EmittedEvent) -> EmittedEvent {
    EmittedEvent {
        contract_address: event.address,
        topics: event.topics,
        data: event.data,
    }
}
pub(super) async fn chain_tip(client: &HttpClient) -> Result<ChainTip, SimRpcError> {
    let tip = known(
        "getChainTip",
        NativeClient::new(client.clone())
            .get_chain_tip(req::EmptyRequest {})
            .await,
    )?;
    Ok(ChainTip {
        height: tip.block_height,
        block_hash: tip.block_hash,
    })
}
pub(super) async fn send_transaction(
    client: &HttpClient,
    tx_hex: String,
) -> Result<TxHash, SimRpcError> {
    let bytes = hex::decode(tx_hex.strip_prefix("0x").unwrap_or(&tx_hex))
        .map_err(|e| SimRpcError::detail("sendTransaction", e.to_string()))?;
    let signed: thruput_xdr::transaction::SignedTransaction = xdrs::txs::decode(&bytes)
        .map_err(|e| SimRpcError::detail("sendTransaction", e.to_string()))?;
    let expected = TxHash(thruput_json_rpc_types::base58::Base58(
        thruput_signing::transaction_hash(&signed.tx)
            .map_err(|e| SimRpcError::detail("sendTransaction", e.to_string()))?,
    ));
    let response = known(
        "sendTransaction",
        NativeClient::new(client.clone())
            .send_transaction(req::SendTransactionRequest {
                signed_transaction: HexBytes(bytes),
            })
            .await,
    )?;
    if response.transaction_hash != expected {
        return Err(SimRpcError::InvalidResponse {
            method: "sendTransaction",
            message: "acknowledgement hash mismatch".into(),
            raw: json!(response),
        });
    }
    Ok(expected)
}
pub(super) async fn receipt(
    client: &HttpClient,
    transaction_hash: TxHash,
) -> Result<Option<TransactionReceipt>, SimRpcError> {
    let Some(receipt) = known(
        "getTransactionReceipt",
        NativeClient::new(client.clone())
            .get_transaction_receipt(req::TransactionQuery { transaction_hash })
            .await,
    )?
    else {
        return Ok(None);
    };
    if receipt.transaction_hash != transaction_hash {
        return Err(SimRpcError::InvalidResponse {
            method: "getTransactionReceipt",
            message: "receipt hash mismatch".into(),
            raw: json!(receipt),
        });
    }
    let (status, return_data, abort_data) = match receipt.execution {
        res::ReceiptExecution::Return { return_data } => (ExecStatus::Ok, Some(return_data), None),
        res::ReceiptExecution::Abort { abort_data } => (
            ExecStatus::Reverted { message: None },
            None,
            Some(abort_data),
        ),
        res::ReceiptExecution::Fault {} => (
            ExecStatus::DeterministicError {
                kind: "fault".into(),
            },
            None,
            None,
        ),
        res::ReceiptExecution::NonDeterministicError {} => {
            (ExecStatus::NonDeterministicError, None, None)
        }
        res::ReceiptExecution::FailedGasSponsorship {} => {
            (ExecStatus::FailedGasSponsorship, None, None)
        }
        _ => {
            return Err(SimRpcError::InvalidResponse {
                method: "getTransactionReceipt",
                message: "unsupported execution".into(),
                raw: json!(receipt),
            });
        }
    };
    Ok(Some(TransactionReceipt {
        tx_hash: receipt.transaction_hash,
        height: receipt.block_height,
        status,
        events: receipt.events.into_iter().map(emitted).collect(),
        return_data,
        abort_data,
        gas_used: receipt.gas_used,
    }))
}
pub(super) async fn simulate(
    client: &HttpClient,
    request: ViewRequest,
) -> Result<ViewResponse, SimRpcError> {
    let assumed_auth = request
        .auth_claim
        .map(|auth| match auth {
            ViewAuthClaim::SourceAddress {
                allowed_keys,
                required_weight,
            } => Ok(req::AssumedAuth::SourceAddress {
                allowed_keys,
                required_weight,
            }),
            ViewAuthClaim::PubkeySignature { pubkey_hash, nonce } => {
                Ok(req::AssumedAuth::PubkeySignature {
                    pubkey_hash,
                    account_nonce: nonce,
                })
            }
            _ => Err(SimRpcError::Unsupported {
                method: "simulation authorization",
            }),
        })
        .transpose()?;
    let min_height = request.min_height.clone();
    let response = known(
        "simulateInvocation",
        NativeClient::new(client.clone())
            .simulate_invocation(req::SimulateInvocationRequest {
                address: request.contract,
                entrypoint: request.entrypoint,
                calldata: request.calldata,
                caller: request.caller,
                gas_limit: request.gas_limit,
                min_state_block_height: request.min_height,
                assumed_auth,
            })
            .await,
    )?;
    if min_height.is_some_and(|minimum| response.state_block_height < minimum) {
        return Err(SimRpcError::InvalidResponse {
            method: "simulateInvocation",
            message: "state height is below requested minimum".into(),
            raw: json!(response),
        });
    }
    let output = match response.output {
        res::InvocationOutput::Return { return_data } => {
            RootInvocationOutput::Return { return_data }
        }
        res::InvocationOutput::Abort { abort_data } => RootInvocationOutput::Abort { abort_data },
        res::InvocationOutput::Fault {} => RootInvocationOutput::Fault,
        _ => {
            return Err(SimRpcError::InvalidResponse {
                method: "simulateInvocation",
                message: "unsupported output".into(),
                raw: json!(response),
            });
        }
    };
    Ok(ViewResponse {
        output,
        events: response.events.into_iter().map(emitted).collect(),
        height: response.state_block_height,
        gas_used: response.gas_used,
    })
}
pub(super) async fn storage(
    client: &HttpClient,
    address: ContractAddress,
    key: HexBytes,
) -> Result<StorageItemRead, SimRpcError> {
    let response = known(
        "getStorage",
        NativeClient::new(client.clone())
            .get_storage(req::StorageQuery { address, key })
            .await,
    )?;
    // Item variants have the same stable report representation after native validation.
    let item =
        serde_json::from_value(json!(response.item)).map_err(|_| SimRpcError::InvalidResponse {
            method: "getStorage",
            message: "storage report projection failed".into(),
            raw: json!(response),
        })?;
    Ok(StorageItemRead {
        block_height: response.state_block_height,
        item,
    })
}
pub(super) async fn events(
    client: &HttpClient,
    query: EventQuery,
) -> Result<EventPage, SimRpcError> {
    let start = if query.cursor.0.is_empty() {
        Some(match (query.order, query.anchor) {
            (IterationOrder::Ascending, BlockAnchor::AtBeginning) => req::HistoryStart::Ascending {
                anchor: req::AscendingAnchor::AtBeginning {},
            },
            (IterationOrder::Descending, BlockAnchor::AtEnd) => req::HistoryStart::Descending {
                anchor: req::DescendingAnchor::AtEnd {},
            },
            (IterationOrder::Ascending, BlockAnchor::AtHeight { height }) => {
                req::HistoryStart::Ascending {
                    anchor: req::AscendingAnchor::AtHeight {
                        block_height: height,
                    },
                }
            }
            (IterationOrder::Descending, BlockAnchor::AtHeight { height }) => {
                req::HistoryStart::Descending {
                    anchor: req::DescendingAnchor::AtHeight {
                        block_height: height,
                    },
                }
            }
            _ => {
                return Err(SimRpcError::detail(
                    "getEvents",
                    "anchor does not match order",
                ));
            }
        })
    } else {
        None
    };
    let response = known(
        "getEvents",
        NativeClient::new(client.clone())
            .get_events(req::EventQuery {
                filters: query
                    .filters
                    .0
                    .0
                    .into_iter()
                    .map(|filter| req::EventFilter {
                        addresses: filter.contract_addresses.0.0,
                        topics: Some(req::TopicFilter {
                            topic0: Some(filter.topics.topic0.0.0),
                            topic1: Some(filter.topics.topic1.0.0),
                            topic2: Some(filter.topics.topic2.0.0),
                        }),
                    })
                    .collect(),
                start,
                cursor: (!query.cursor.0.is_empty()).then_some(HexBytes(query.cursor.0)),
                limit: Some(query.limit),
            })
            .await,
    )?;
    Ok(EventPage {
        events: response
            .events
            .into_iter()
            .map(|event| IdentifiedEvent {
                tx_hash: event.transaction_hash,
                block_height: event.block_height,
                sequence_number: event.sequence_number,
                event: emitted(event.event),
            })
            .collect(),
        cursor: PaginationCursor(response.next_cursor.map_or_else(Vec::new, |c| c.0)),
        latest_block: response.latest_indexed_block_height,
        oldest_block: response.oldest_indexed_block_height,
    })
}

#[cfg(test)]
mod tests;
