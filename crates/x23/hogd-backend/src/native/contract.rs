use sha2::{Digest, Sha256};
use thruput_db_gateway::{DbGatewayContractResult, DbGatewayItemResult};
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::{
    BlockHeight, ContractAddress, SHA256Hash, base58::Base58,
    error::native::BackendDataInvalidReason as Reason, hex::HexBytes, responses as out,
};
use thruput_node_rpc::RPCError;
use thruput_xdr::{contract::Contract, storage_item::WireStorageItem};

use super::{height, invalid};

/// Project only the reserved deployment row, never arbitrary contract storage.
pub fn contract_deployment(
    address: ContractAddress,
    state_block_height: BlockHeight,
    item: DbGatewayItemResult,
) -> Result<out::ContractDeploymentResult, RpcServerError> {
    height(&state_block_height)?;
    let deployment = match crate::db_gateway::stored_item("DbGateway", item)? {
        None => None,
        Some(WireStorageItem::kItemTypeDeployedContractHash {
            deployed_contract_hash: value,
        }) => Some(out::ContractDeployment {
            hash: Base58(value.hash),
            size: value.contract_size,
            contract_type: value.contract_type.0,
        }),
        Some(_) => {
            return Err(invalid(
                Reason::InconsistentResult,
                "deployment read returned another storage type",
            ));
        }
    };
    Ok(out::ContractDeploymentResult {
        address,
        state_block_height,
        deployment,
    })
}

/// Validate complete canonical Contract XDR before exposing content-addressed bytes.
pub fn contract_artifact(
    hash: SHA256Hash,
    result: DbGatewayContractResult,
) -> Result<out::ContractArtifactResult, RpcServerError> {
    let bytes = match result {
        DbGatewayContractResult::kRPCNone { contract_xdr } => contract_xdr.0,
        DbGatewayContractResult::Default { error } if RPCError(error) == RPCError::kRPCNotFound => {
            return Err(RpcServerError::ContractArtifactUnavailable(hash));
        }
        DbGatewayContractResult::Default { error } => {
            return Err(RpcServerError::BackendUnavailable(format!(
                "DbGateway: {}",
                RPCError(error)
            )));
        }
        _ => {
            return Err(invalid(
                Reason::UnsupportedValue,
                "unsupported artifact response",
            ));
        }
    };
    if Sha256::digest(&bytes).as_slice() != hash.0.as_ref() {
        return Err(invalid(
            Reason::InconsistentResult,
            "contract artifact hash mismatch",
        ));
    }
    let contract: Contract = xdrs::txs::decode(&bytes)
        .map_err(|_| invalid(Reason::InvalidEncoding, "invalid Contract XDR"))?;
    let canonical = xdrs::txs::encode(&contract)
        .map_err(|_| invalid(Reason::InvalidEncoding, "cannot encode Contract XDR"))?;
    if canonical != bytes {
        return Err(invalid(
            Reason::InvalidEncoding,
            "noncanonical Contract XDR",
        ));
    }
    Ok(out::ContractArtifactResult {
        hash,
        contract_xdr: HexBytes(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use thruput_xdr::contract::ContractBlob;
    use xdr_common::BytesM;

    fn response(bytes: Vec<u8>) -> DbGatewayContractResult {
        DbGatewayContractResult::kRPCNone {
            contract_xdr: BytesM(bytes),
        }
    }
    fn hash(bytes: &[u8]) -> SHA256Hash {
        Base58(thruput_xdr::types::SHA256Hash::from(<[u8; 32]>::from(
            Sha256::digest(bytes),
        )))
    }

    #[test]
    fn complete_contract_bytes_are_hash_checked_and_preserved() {
        let contract = Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(vec![1, 2, 3]),
            },
        };
        let bytes = xdrs::txs::encode(&contract).unwrap();
        let result = contract_artifact(hash(&bytes), response(bytes.clone())).unwrap();
        assert_eq!(result.contract_xdr.0, bytes);
        assert!(matches!(
            contract_artifact(hash(b"other"), response(bytes)),
            Err(RpcServerError::BackendDataInvalid {
                reason: Reason::InconsistentResult,
                ..
            })
        ));
    }

    #[test]
    fn matching_hash_does_not_make_invalid_xdr_valid() {
        let valid = xdrs::txs::encode(&Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(vec![1, 2, 3]),
            },
        })
        .unwrap();
        let mut trailing = valid.clone();
        trailing.extend_from_slice(&[0; 4]);
        let mut padding = valid.clone();
        *padding.last_mut().unwrap() = 1;
        for bytes in [vec![], valid[..7].to_vec(), trailing, padding] {
            assert!(matches!(
                contract_artifact(hash(&bytes), response(bytes)),
                Err(RpcServerError::BackendDataInvalid {
                    reason: Reason::InvalidEncoding,
                    ..
                })
            ));
        }
    }

    #[test]
    fn unavailable_content_is_distinct_from_failed_reads() {
        let wanted = hash(b"missing");
        assert!(
            matches!(contract_artifact(wanted, DbGatewayContractResult::Default {
            error: RPCError::kRPCNotFound.0,
        }), Err(RpcServerError::ContractArtifactUnavailable(observed)) if observed == wanted)
        );
        assert!(matches!(
            contract_artifact(
                wanted,
                DbGatewayContractResult::Default {
                    error: RPCError::kRPCUnrecoverable.0,
                }
            ),
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }

    #[test]
    fn deployment_absence_requires_an_authoritative_missing_row() {
        let address = Base58(thruput_xdr::types::ContractAddress::from([7; 32]));
        let missing = DbGatewayItemResult::Default {
            error: RPCError::kRPCNotFound.0,
        };
        let result = contract_deployment(address, 42.into(), missing).unwrap();
        assert_eq!(result.address, address);
        assert_eq!(*result.state_block_height, 42);
        assert!(result.deployment.is_none());
        assert!(
            contract_deployment(
                address,
                42.into(),
                DbGatewayItemResult::kRPCNone {
                    item: WireStorageItem::kItemTypeBytestring {
                        bytestring: BytesM(vec![])
                    },
                }
            )
            .is_err()
        );
        assert!(
            contract_deployment(
                address,
                42.into(),
                DbGatewayItemResult::Default {
                    error: RPCError::kRPCUnrecoverable.0,
                }
            )
            .is_err()
        );
    }
}
