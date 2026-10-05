//! Decode `DbGateway` latest-state reads.
//!
//! A `DbGateway` response nests two failure layers: the outer
//! [`DbGatewayResult`] covers the whole batch, and each item carries its own
//! per-key result. Only one per-item code — `kRPCNotFound` — is an absent
//! key; every other one is a backend failure that must not read as a miss.

use groundhog_db_gateway::{
    AddressAndKeyBatch, DbGatewayItemResult, DbGatewayPayload, DbGatewayResult,
};
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    address_auth::{AddressAuthDataInfo, AddressAuthDataResponse},
    state::{StorageItemInfo, StorageItemRead},
};
use groundhog_scheduler::RPCError;
use groundhog_xdr::storage_item::WireStorageItem;
use groundhog_xdr::types::{AddressAndKey, StorageKey, StorageKeyContractData};
use xdr_common::VecM;

/// The storage key for one 32-byte contract-data slot.
pub fn contract_data_key(key: [u8; 32]) -> StorageKey {
    StorageKey::ContractData {
        contract_data_key: StorageKeyContractData::from(key),
    }
}

/// Build a one-key request batch.
pub fn single_key_batch(address: ContractAddress, key: StorageKey) -> AddressAndKeyBatch {
    VecM(vec![AddressAndKey {
        address: address.0,
        key,
    }])
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

/// Take the single per-item result of a one-key batch payload, together
/// with the batch's snapshot height.
pub fn single_item(
    service: &str,
    payload: DbGatewayPayload,
) -> Result<(BlockHeight, DbGatewayItemResult), RpcServerError> {
    let item = payload
        .items
        .0
        .into_iter()
        .next()
        .ok_or_else(|| missing_item_error(service))?;
    Ok((payload.block_height, item))
}

/// Fold a one-key batch payload into its single storage read.
pub fn single_item_read(
    service: &str,
    payload: DbGatewayPayload,
) -> Result<StorageItemRead, RpcServerError> {
    let (block_height, item) = single_item(service, payload)?;
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
    block_height: BlockHeight,
    item: DbGatewayItemResult,
) -> Result<StorageItemRead, RpcServerError> {
    Ok(StorageItemRead {
        block_height,
        item: stored_item(service, item)?.map(StorageItemInfo::from),
    })
}

/// Fold one per-item result for an address's `AuthData` key, and the batch's
/// snapshot height, into the public auth-data read.
///
/// An absent key is an address with no registered configuration, which is a
/// successful read with `auth_data: None`. hogd stores only auth data at that
/// key, so any other item kind fails the read rather than reading as absent.
pub fn address_auth_data_read(
    service: &str,
    address: ContractAddress,
    block_height: BlockHeight,
    item: DbGatewayItemResult,
) -> Result<AddressAuthDataResponse, RpcServerError> {
    let auth_data = match stored_item(service, item)? {
        None => None,
        Some(WireStorageItem::kItemTypeAuthData { auth_data }) => {
            Some(AddressAuthDataInfo::from(&auth_data))
        }
        Some(other) => {
            return Err(RpcServerError::BackendUnavailable(format!(
                "{service} auth data item had unexpected item type {}",
                other.r#type().0
            )));
        }
    };
    Ok(AddressAuthDataResponse {
        address,
        height: block_height,
        auth_data,
    })
}

/// The stored item one per-item result carries, or `None` for an absent key.
fn stored_item(
    service: &str,
    item: DbGatewayItemResult,
) -> Result<Option<WireStorageItem>, RpcServerError> {
    match item {
        DbGatewayItemResult::kRPCNone { item } => Ok(Some(item)),
        DbGatewayItemResult::Default { error } => {
            let error = RPCError(error);
            if error == RPCError::kRPCNotFound {
                Ok(None)
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
