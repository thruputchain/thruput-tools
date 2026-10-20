use thruput_db_gateway::{DbGatewayItemResult, DbGatewayPayload};
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::{
    BlockHeight, ContractAddress, base58::Base58,
    error::native::BackendDataInvalidReason as Reason, hex::HexBytes, responses as out,
};
use thruput_xdr::{storage_item::WireStorageItem, types::PublicKey};

use super::{height, invalid};

/// A one-key read must contain exactly one answer, even when the key is absent.
pub fn single_item(
    payload: DbGatewayPayload,
) -> Result<(BlockHeight, DbGatewayItemResult), RpcServerError> {
    height(&payload.block_height)?;
    if payload.items.0.len() != 1 {
        return Err(invalid(
            Reason::InvalidCardinality,
            "one-key read did not return exactly one item",
        ));
    }
    let item = payload
        .items
        .0
        .into_iter()
        .next()
        .expect("bug: checked single item");
    Ok((payload.block_height, item))
}

/// Project an item without treating a failed read as absence.
pub fn storage(
    state_block_height: BlockHeight,
    item: DbGatewayItemResult,
) -> Result<out::StorageResult, RpcServerError> {
    height(&state_block_height)?;
    let item = crate::db_gateway::stored_item("DbGateway", item)?
        .map(storage_item)
        .transpose()?;
    Ok(out::StorageResult {
        state_block_height,
        item,
    })
}

fn storage_item(item: WireStorageItem) -> Result<out::StorageItem, RpcServerError> {
    Ok(match item {
        WireStorageItem::kItemTypeNonnegativeInt { nnint } => {
            let value = (i128::from(nnint.highbits) << 64) | i128::from(nnint.lowbits);
            if value < 0 {
                return Err(invalid(
                    Reason::InconsistentResult,
                    "negative nonnegative storage value",
                ));
            }
            out::StorageItem::NonnegativeInt { value }
        }
        WireStorageItem::kItemTypeBytestring { bytestring } => out::StorageItem::Bytestring {
            bytes: HexBytes(bytestring.0),
        },
        WireStorageItem::kItemTypeAuthData { .. } => out::StorageItem::AuthData {},
        WireStorageItem::kItemTypeRetrocausalQueue { .. } => out::StorageItem::RetrocausalQueue {},
        WireStorageItem::kItemTypeReplayCacheData => out::StorageItem::ReplayCacheEntry {},
        WireStorageItem::kItemTypeDeployedContractHash {
            deployed_contract_hash: value,
        } => out::StorageItem::DeployedContract {
            hash: Base58(value.hash),
            size: value.contract_size,
            contract_type: value.contract_type.0,
        },
        // Unknown wire discriminants fail in the generated client today; this
        // fallback covers variants added to the Rust bindings in future.
        other => out::StorageItem::Unknown {
            type_id: other.r#type().0,
        },
    })
}

/// Project registered signer slots; an absent row is not a missing account.
pub fn registered_auth(
    address: ContractAddress,
    state_block_height: BlockHeight,
    item: DbGatewayItemResult,
) -> Result<out::RegisteredAuthResult, RpcServerError> {
    height(&state_block_height)?;
    let configuration = match crate::db_gateway::stored_item("DbGateway", item)? {
        None => None,
        Some(WireStorageItem::kItemTypeAuthData { auth_data }) => Some(out::AuthConfiguration {
            required_weight: auth_data.required_weight,
            signers: (0u32..)
                .zip(&auth_data.public_keys)
                .filter_map(|(key_id, slot)| {
                    slot.as_ref().map(|slot| out::AuthSigner {
                        key_id,
                        max_weight: slot.max_weight,
                        public_key: match &slot.key {
                            PublicKey::Ed25519 { ed25519 } => out::PublicKey::Ed25519 {
                                key: Base58(*ed25519),
                            },
                            other => out::PublicKey::Unknown {
                                scheme_id: other.scheme().0,
                            },
                        },
                    })
                })
                .collect(),
        }),
        Some(_) => {
            return Err(invalid(
                Reason::InconsistentResult,
                "authorization read returned another storage type",
            ));
        }
    };
    Ok(out::RegisteredAuthResult {
        address,
        state_block_height,
        configuration,
    })
}

#[cfg(test)]
mod tests;
