//! Decode `DbGateway` latest-state reads.
//!
//! A `DbGateway` response nests two failure layers: the outer
//! [`DbGatewayResult`] covers the whole batch, and each item carries its own
//! per-key result. Only one per-item code — `kRPCNotFound` — is an absent
//! key; every other one is a backend failure that must not read as a miss.

use groundhog_db_gateway::{DbGatewayItemResult, DbGatewayPayload, DbGatewayResult};
use groundhog_json_rpc_server::RpcServerError;
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    address_auth::{AddressAuthDataInfo, AddressAuthDataResponse},
    state::{StorageItemInfo, StorageItemRead},
};
use groundhog_scheduler::RPCError;
use groundhog_xdr::storage_item::WireStorageItem;
use groundhog_xdr::types::{StorageKey, StorageKeyContractData};

/// Project one snapshot-bound rules reading without supplying missing defaults.
pub fn chain_config(
    service: &str,
    result: groundhog_db_gateway::DbGatewayConfigResult,
) -> Result<Option<groundhog_json_rpc_types::network_config::ChainConfig>, RpcServerError> {
    use groundhog_json_rpc_types::network_config::{
        ChainConfig, ExecutionConfig, NetworkConfigParams,
    };
    if matches!(&result, groundhog_db_gateway::DbGatewayConfigResult::Default { error } if *error == RPCError::kRPCUnimplemented.0)
    {
        return Ok(None);
    }
    let groundhog_db_gateway::DbGatewayConfigResult::kRPCNone { config } = result else {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service}: configuration unavailable"
        )));
    };
    if config.storage_cost_model.ext != groundhog_xdr::types::ExtensionPoint::_0 {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service}: unsupported storage cost model"
        )));
    }
    let stated = NetworkConfigParams::from_network_config(&config.tip.network_config)
        .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
    let effective = NetworkConfigParams::from_network_config(&config.effective_config)
        .map_err(|error| RpcServerError::BackendUnavailable(error.to_string()))?;
    let tip = crate::scheduler::chain_tip(service, &config.tip.value)?;
    if *tip.height < 0 {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service}: no committed configuration"
        )));
    }
    Ok(Some(ChainConfig::with_execution(
        stated,
        ExecutionConfig::new(
            tip,
            groundhog_json_rpc_types::hex::Hex0x(*config.tip.value.proposal_header.config_hash),
            effective,
            (&config.storage_cost_model).into(),
        ),
    )))
}

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

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn only_explicitly_unsupported_readings_allow_stated_only_fallback() {
        use groundhog_db_gateway::DbGatewayConfigResult;
        assert!(
            chain_config(
                "test",
                DbGatewayConfigResult::Default {
                    error: RPCError::kRPCUnimplemented.0,
                }
            )
            .unwrap()
            .is_none()
        );
        for error in [RPCError::kRPCNotFound.0, RPCError::kRPCUnrecoverable.0, 999] {
            assert!(matches!(
                chain_config("test", DbGatewayConfigResult::Default { error }),
                Err(RpcServerError::BackendUnavailable(_))
            ));
        }
    }
}
