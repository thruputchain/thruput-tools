//! Decode `DbGateway` latest-state reads.
//!
//! A `DbGateway` response nests two failure layers: the outer
//! [`DbGatewayResult`] covers the whole batch, and each item carries its own
//! per-key result. Only one per-item code — `kRPCNotFound` — is an absent
//! key; every other one is a backend failure that must not read as a miss.

use thruput_db_gateway::{DbGatewayItemResult, DbGatewayPayload, DbGatewayResult};
use thruput_json_rpc_server::{RpcServerError, TipReading};
use thruput_json_rpc_types::error::native::BackendDataInvalidReason as DataReason;
use thruput_scheduler::RPCError;
use thruput_xdr::storage_item::WireStorageItem;
use thruput_xdr::types::{StorageKey, StorageKeyContractData};

/// Project one snapshot-bound rules reading without supplying missing defaults.
pub fn tip_reading(
    service: &str,
    result: thruput_db_gateway::DbGatewayConfigResult,
    expected_network: Option<&thruput_json_rpc_types::NetworkHashText>,
) -> Result<TipReading, RpcServerError> {
    use thruput_json_rpc_types::network_config::{ExecutionConfig, NetworkConfigParams};
    let thruput_db_gateway::DbGatewayConfigResult::kRPCNone { config } = result else {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service}: configuration unavailable"
        )));
    };
    if let Some(expected) = expected_network {
        let observed = thruput_json_rpc_types::NetworkHashText::from(
            config
                .tip
                .value
                .proposal_header
                .network_hash
                .clone()
                .into_inner()
                .into_inner(),
        );
        if expected != &observed {
            return Err(RpcServerError::invalid_data(
                DataReason::InconsistentResult,
                format!(
                    "{service}: network hash mismatch: expected {expected}, observed {observed}"
                ),
            ));
        }
    }
    if config.storage_cost_model.ext != thruput_xdr::types::ExtensionPoint::_0 {
        return Err(RpcServerError::invalid_data(
            DataReason::UnsupportedValue,
            format!("{service}: unsupported storage cost model"),
        ));
    }
    let effective =
        NetworkConfigParams::from_network_config(&config.effective_config).map_err(|error| {
            RpcServerError::invalid_data(DataReason::UnsupportedValue, error.to_string())
        })?;
    let tip = crate::scheduler::chain_tip(service, &config.tip.value)?;
    if *tip.height == -1 {
        return Err(RpcServerError::BackendUnavailable(format!(
            "{service}: no committed configuration"
        )));
    }
    if *tip.height < -1 {
        return Err(RpcServerError::invalid_data(
            DataReason::InconsistentResult,
            format!("{service}: no committed configuration"),
        ));
    }
    let execution = ExecutionConfig::new(
        tip.clone(),
        thruput_json_rpc_types::base58::Base58(*config.tip.value.proposal_header.config_hash),
        effective,
        (&config.storage_cost_model).into(),
    );
    Ok(TipReading::new(tip, config.tip.network_config).with_execution(execution))
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
            Err(RpcServerError::invalid_data(
                DataReason::UnsupportedValue,
                format!(
                    "{service} returned an unsupported result variant; regenerate the DbGateway \
                 client for this protocol"
                ),
            ))
        }
    }
}

/// The error for a batch answer that carries fewer items than the batch
/// asked for, which leaves a requested key unaccounted for.
pub fn missing_item_error(service: &str) -> RpcServerError {
    RpcServerError::invalid_data(
        DataReason::InvalidCardinality,
        format!("{service} response had fewer items than the batch"),
    )
}

/// The stored item one per-item result carries, or `None` for an absent key.
pub(crate) fn stored_item(
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
            Err(RpcServerError::invalid_data(
                DataReason::UnsupportedValue,
                format!(
                    "{service} returned an unsupported per-item result variant; regenerate the \
                 DbGateway client for this protocol"
                ),
            ))
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn failed_readings_never_fall_back_to_partial_configuration() {
        use thruput_db_gateway::DbGatewayConfigResult;
        for error in [
            RPCError::kRPCNotFound.0,
            RPCError::kRPCUnrecoverable.0,
            RPCError::kRPCUnimplemented.0,
            999,
        ] {
            assert!(matches!(
                tip_reading("test", DbGatewayConfigResult::Default { error }, None),
                Err(RpcServerError::BackendUnavailable(_))
            ));
        }
    }
}
