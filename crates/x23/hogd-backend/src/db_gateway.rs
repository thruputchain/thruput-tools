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
    ContractAddress,
    state::{StorageItemInfo, StorageItemRead},
};
use groundhog_scheduler::RPCError;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_key_is_a_read_at_a_known_height_not_a_failure() {
        let read = storage_item_read(
            "DbGateway",
            7.into(),
            DbGatewayItemResult::Default {
                error: RPCError::kRPCNotFound.0,
            },
        )
        .expect("a missing key is a successful read");

        assert_eq!(read.block_height, 7.into());
        assert!(read.item.is_none());
    }

    #[test]
    fn any_other_item_error_fails_the_read() {
        let error = storage_item_read(
            "hogd-dev DbGateway",
            7.into(),
            DbGatewayItemResult::Default {
                error: RPCError::kRPCStorageWriteFailed.0,
            },
        )
        .unwrap_err();

        assert!(matches!(error, RpcServerError::BackendUnavailable(_)));
        assert!(error.to_string().contains("hogd-dev DbGateway"), "{error}");
    }

    #[test]
    fn an_outer_batch_error_is_named_with_the_service() {
        let error = payload(
            "DbGateway",
            DbGatewayResult::Default {
                error: RPCError::kRPCNotFound.0,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("DbGateway"), "{error}");
    }
}
