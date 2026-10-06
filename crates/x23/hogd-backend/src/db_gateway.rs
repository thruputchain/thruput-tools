//! Decode `DbGateway` latest-state reads.
//!
//! A `DbGateway` response nests two failure layers: the outer
//! [`DbGatewayResult`] covers the whole batch, and each item carries its own
//! per-key result. Only one per-item code — `kRPCNotFound` — is an absent
//! key; every other one is a backend failure that must not read as a miss.

use groundhog_db_gateway::{DbGatewayItemResult, DbGatewayPayload, DbGatewayResult};
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::state::{StorageItemInfo, StorageItemRead};
use groundhog_scheduler::RPCError;
use groundhog_xdr::types::{StorageKey, StorageKeyContractData};

/// The storage key for one 32-byte contract-data slot.
pub fn contract_data_key(key: [u8; 32]) -> StorageKey {
    StorageKey::ContractData {
        contract_data_key: StorageKeyContractData::from(key),
    }
}

/// Unwrap the outer batch result.
pub fn payload(service: &str, result: DbGatewayResult) -> Result<DbGatewayPayload, RpcServerError> {
    match result {
        DbGatewayResult::kRPCNone { payload } => Ok(payload),
        DbGatewayResult::Default { error } => Err(RpcServerError::BackendUnavailable(format!(
            "{service}: {}",
            RPCError(error)
        ))),
        unexpected => {
            tracing::warn!(
                ?unexpected,
                %service,
                "DbGateway returned unsupported outer result variant"
            );
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned an unsupported result variant; regenerate the DbGateway \
                 client for this protocol"
            )))
        }
    }
}

/// The error for a batch answer that carries fewer items than the batch
/// asked for, which leaves a requested key unaccounted for.
pub fn missing_item_error(service: &str) -> RpcServerError {
    RpcServerError::BackendUnavailable(format!("{service} response had fewer items than the batch"))
}

/// Fold a one-key batch payload into its single storage read.
pub fn single_item_read(
    service: &str,
    payload: DbGatewayPayload,
) -> Result<StorageItemRead, RpcServerError> {
    let block_height = payload.block_height;
    let item = payload
        .items
        .0
        .into_iter()
        .next()
        .ok_or_else(|| missing_item_error(service))?;
    storage_item_read(service, block_height, item)
}

/// Fold one per-item result and the batch's snapshot height into a public
/// storage read.
///
/// `block_height` is the freshness boundary for the whole batch and is
/// reported whether the key is present or absent, so a caller can tell an
/// absent key at a known height from an unknown one.
pub fn storage_item_read(
    service: &str,
    block_height: groundhog_json_rpc_types::BlockHeight,
    item: DbGatewayItemResult,
) -> Result<StorageItemRead, RpcServerError> {
    match item {
        DbGatewayItemResult::kRPCNone { item } => Ok(StorageItemRead {
            block_height,
            item: Some(StorageItemInfo::from(item)),
        }),
        DbGatewayItemResult::Default { error } => {
            let error = RPCError(error);
            if error == RPCError::kRPCNotFound {
                Ok(StorageItemRead {
                    block_height,
                    item: None,
                })
            } else {
                Err(RpcServerError::BackendUnavailable(format!(
                    "{service} item: {error}"
                )))
            }
        }
        unexpected => {
            tracing::warn!(
                ?unexpected,
                %service,
                "DbGateway returned unsupported per-item result variant"
            );
            Err(RpcServerError::BackendUnavailable(format!(
                "{service} returned an unsupported per-item result variant; regenerate the \
                 DbGateway client for this protocol"
            )))
        }
    }
}
