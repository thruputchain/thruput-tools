//! Off-chain mirror of the bounds hogd checks before executing a
//! transaction.
//!
//! hogd's `SystemCallHandler::perform_initial_validation` rejects a
//! transaction deterministically, before running any of it, when it
//! attaches more contracts than the chain's network config permits, when
//! any attached contract's code exceeds the size the config admits, or
//! when the same contract is attached twice. Applying the same checks
//! before submission turns a silent later failure into an immediate,
//! structured [`TxInvalidDetail`].
//!
//! The chain-configured bounds are read from the network config a db tip
//! carries, flattened once into [`NetworkConfigParams`] so every later
//! access is a field read. A bound is enforced only with a value the
//! protocol defines: the one the config states, or, for the one parameter
//! whose unstated value the protocol itself fixes
//! (`kDefaultMaxContractSizeBytes`), that constant. Nothing is copied from
//! hogd's own fallbacks. None of this is a trust boundary: the executor
//! re-checks everything, and a reading that lags a block is harmless
//! because these parameters change only through explicit config upgrades.

use groundhog_json_rpc_types::error::TxInvalidDetail;
use groundhog_xdr::{
    contract::{Contract, kDefaultMaxContractSizeBytes},
    network_config::{ConfigParam, NetworkConfig},
    transaction::Transaction,
};

/// The execution parameters a chain's network config states, flattened
/// from the list that carries them.
///
/// Resolve once where a tip is read, then each parameter is a field rather
/// than a scan of the list. A field is `None` when the config does not
/// state that parameter; a check that depends on it is then skipped unless
/// the protocol fixes the value in force for that case. `Default` is the
/// reading with nothing stated, and struct update from it is the way to
/// state a subset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetworkConfigParams {
    /// Concurrently open incremental SHA-256 buffers permitted per
    /// invocation frame.
    pub max_sha256_buffers: Option<u32>,
    /// Invocation frames one transaction may have open at once.
    pub max_cross_invoke_depth: Option<u32>,
    /// Contracts one transaction may attach in `contractsToDeploy`.
    pub max_new_contracts_per_tx: Option<u32>,
    /// Bytes one bytestring storage item may hold.
    pub max_bytestring_len: Option<u32>,
    /// Bytes one attached contract's code blob may hold. The value applied
    /// when unstated is [`Self::max_contract_size_bytes_in_force`].
    pub max_contract_size_bytes: Option<u32>,
    /// Storage savepoints one transaction may have open at once.
    pub max_savepoint_depth: Option<u32>,
    /// Bytes one incremental SHA-256 buffer may absorb between resets.
    pub max_sha256_buffer_bytes: Option<u32>,
    /// Levels of node nesting an auth token may carry.
    pub max_auth_token_depth: Option<u32>,
    /// Code bytes an RV32 artifact may carry to be attached.
    pub max_rv32_code_bytes: Option<u32>,
    /// Guest data memory bytes an RV32 artifact may declare to be attached.
    pub max_rv32_memory_bytes: Option<u32>,
}

impl NetworkConfigParams {
    /// Flatten the parameters a network config states.
    ///
    /// A config version this build cannot interpret is an error rather
    /// than a guess: the protocol requires a node that cannot name a
    /// version to fail, and this crate follows suit.
    pub fn from_network_config(config: &NetworkConfig) -> Result<Self, UnsupportedNetworkConfig> {
        let params = match config {
            NetworkConfig::_0 { v0 } => &v0.params,
            other => {
                return Err(UnsupportedNetworkConfig {
                    config: format!("{other:?}"),
                });
            }
        };
        let mut resolved = Self::default();
        for param in params.iter() {
            match param {
                ConfigParam::kConfigParamMaxSHA256Buffers { max_sha256_buffers } => {
                    resolved.max_sha256_buffers = Some(*max_sha256_buffers);
                }
                ConfigParam::kConfigParamMaxCrossInvokeDepth {
                    max_cross_invoke_depth,
                } => {
                    resolved.max_cross_invoke_depth = Some(*max_cross_invoke_depth);
                }
                ConfigParam::kConfigParamMaxNewContractsPerTx {
                    max_new_contracts_per_tx,
                } => {
                    resolved.max_new_contracts_per_tx = Some(*max_new_contracts_per_tx);
                }
                ConfigParam::kConfigParamMaxBytestringLen { max_bytestring_len } => {
                    resolved.max_bytestring_len = Some(*max_bytestring_len);
                }
                ConfigParam::kConfigParamMaxContractSizeBytes {
                    max_contract_size_bytes,
                } => {
                    resolved.max_contract_size_bytes = Some(*max_contract_size_bytes);
                }
                ConfigParam::kConfigParamMaxSavepointDepth {
                    max_savepoint_depth,
                } => {
                    resolved.max_savepoint_depth = Some(*max_savepoint_depth);
                }
                ConfigParam::kConfigParamMaxSHA256BufferBytes {
                    max_sha256_buffer_bytes,
                } => {
                    resolved.max_sha256_buffer_bytes = Some(*max_sha256_buffer_bytes);
                }
                ConfigParam::kConfigParamMaxAuthTokenDepth {
                    max_auth_token_depth,
                } => {
                    resolved.max_auth_token_depth = Some(*max_auth_token_depth);
                }
                ConfigParam::kConfigParamMaxRv32CodeBytes {
                    max_rv32_code_bytes,
                } => {
                    resolved.max_rv32_code_bytes = Some(*max_rv32_code_bytes);
                }
                ConfigParam::kConfigParamMaxRv32MemoryBytes {
                    max_rv32_memory_bytes,
                } => {
                    resolved.max_rv32_memory_bytes = Some(*max_rv32_memory_bytes);
                }
                // A parameter this build does not name cannot have decoded;
                // the arm exists only because the enum is `#[non_exhaustive]`.
                _ => {}
            }
        }
        Ok(resolved)
    }

    /// Bytes one attached contract's code blob may hold under this config.
    ///
    /// The protocol fixes `kDefaultMaxContractSizeBytes` as the value in
    /// force for a configuration that does not state the parameter, since
    /// every chain predating it ran under that cap. Unlike the other
    /// bounds, this one is therefore always known.
    pub fn max_contract_size_bytes_in_force(&self) -> u32 {
        self.max_contract_size_bytes
            .unwrap_or(kDefaultMaxContractSizeBytes)
    }
}

/// A network config whose version this build cannot interpret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported network config version: {config}")]
pub struct UnsupportedNetworkConfig {
    /// Debug rendering of the unrecognized config.
    pub config: String,
}

/// Check a decoded transaction against the executor's pre-execution bounds.
///
/// The duplicate-attachment rule always applies. The attached-contract
/// count is checked only when `params` states a bound for it. Each
/// attached contract's code is checked against the size in force,
/// [`NetworkConfigParams::max_contract_size_bytes_in_force`], so pass
/// `NetworkConfigParams::default()` when no chain reading is available and
/// the protocol's own values apply.
pub fn check_transaction(
    tx: &Transaction,
    params: &NetworkConfigParams,
) -> Result<(), TxInvalidDetail> {
    let contracts = &tx.contracts_to_deploy.0;
    let attached = u32::try_from(contracts.len()).unwrap_or(u32::MAX);

    if let Some(max) = params.max_new_contracts_per_tx
        && attached > max
    {
        return Err(TxInvalidDetail::TooManyContracts { attached, max });
    }

    let max_bytes = params.max_contract_size_bytes_in_force();
    for (index, contract) in contracts.iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let size_bytes = u32::try_from(contract_code_size(contract)).unwrap_or(u32::MAX);
        if size_bytes > max_bytes {
            return Err(TxInvalidDetail::ContractTooLarge {
                index,
                size_bytes,
                max_bytes,
            });
        }
    }

    // The executor keys attachments by the SHA-256 of their XDR encoding, and
    // TXS encoding is canonical, so value equality is hash equality. The
    // attachment count is small (bounded by `max_new_contracts_per_tx`), so a
    // pairwise scan beats hashing every blob on the submit path.
    for (index, contract) in contracts.iter().enumerate() {
        if let Some(first_index) = contracts[..index].iter().position(|seen| seen == contract) {
            return Err(TxInvalidDetail::DuplicateContract {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                first_index: u32::try_from(first_index).unwrap_or(u32::MAX),
            });
        }
    }

    Ok(())
}

/// Code bytes the executor charges an attached contract for.
///
/// Mirrors hogd's `contract_blob_size`: only RV32 blobs carry code, and any
/// contract shape this build does not know is treated as code-free so the
/// executor, not this pre-check, decides its fate.
fn contract_code_size(contract: &Contract) -> usize {
    match contract {
        Contract::kRv32 { rv32_contract } => rv32_contract.data.len(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use groundhog_xdr::{
        contract::ContractBlob, network_config::NetworkConfigV0,
        precompiles::HogexecPrecompiledNativeContracts, transaction::SignedTransaction,
    };
    use xdr_common::{BytesM, VecM};

    use super::*;

    /// Canonical signed-transaction vector shared with `json-rpc-server`'s
    /// tests; only its inner transaction shape matters here.
    const CANONICAL_SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
0000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000000000\
000000000001000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695\
031c9d3fe2554e5ce62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d";

    fn transaction_with(contracts: Vec<Contract>) -> Transaction {
        let canonical = hex::decode(CANONICAL_SIGNED_TX_XDR).unwrap();
        let mut signed: SignedTransaction = xdrs::txs::decode(&canonical).unwrap();
        signed.tx.contracts_to_deploy = VecM(contracts);
        signed.tx
    }

    fn rv32(size: usize, fill: u8) -> Contract {
        Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(vec![fill; size]),
            },
        }
    }

    fn network_config(params: Vec<ConfigParam>) -> NetworkConfig {
        NetworkConfig::_0 {
            v0: NetworkConfigV0 {
                params: VecM(params),
            },
        }
    }

    fn max_contracts(max_new_contracts_per_tx: u32) -> NetworkConfigParams {
        NetworkConfigParams {
            max_new_contracts_per_tx: Some(max_new_contracts_per_tx),
            ..NetworkConfigParams::default()
        }
    }

    fn contract_size_cap(max_contract_size_bytes: u32) -> NetworkConfigParams {
        NetworkConfigParams {
            max_contract_size_bytes: Some(max_contract_size_bytes),
            ..NetworkConfigParams::default()
        }
    }

    #[test]
    fn every_stated_parameter_lands_on_its_field() {
        let config = network_config(vec![
            ConfigParam::kConfigParamMaxSHA256Buffers {
                max_sha256_buffers: 3,
            },
            ConfigParam::kConfigParamMaxCrossInvokeDepth {
                max_cross_invoke_depth: 17,
            },
            ConfigParam::kConfigParamMaxNewContractsPerTx {
                max_new_contracts_per_tx: 7,
            },
            ConfigParam::kConfigParamMaxBytestringLen {
                max_bytestring_len: 4096,
            },
            ConfigParam::kConfigParamMaxContractSizeBytes {
                max_contract_size_bytes: 131_072,
            },
            ConfigParam::kConfigParamMaxSavepointDepth {
                max_savepoint_depth: 9,
            },
            ConfigParam::kConfigParamMaxSHA256BufferBytes {
                max_sha256_buffer_bytes: 8192,
            },
            ConfigParam::kConfigParamMaxAuthTokenDepth {
                max_auth_token_depth: 5,
            },
            ConfigParam::kConfigParamMaxRv32CodeBytes {
                max_rv32_code_bytes: 65536,
            },
            ConfigParam::kConfigParamMaxRv32MemoryBytes {
                max_rv32_memory_bytes: 1_048_576,
            },
        ]);

        let params = NetworkConfigParams::from_network_config(&config).unwrap();

        assert_eq!(
            params,
            NetworkConfigParams {
                max_sha256_buffers: Some(3),
                max_cross_invoke_depth: Some(17),
                max_new_contracts_per_tx: Some(7),
                max_bytestring_len: Some(4096),
                max_contract_size_bytes: Some(131_072),
                max_savepoint_depth: Some(9),
                max_sha256_buffer_bytes: Some(8192),
                max_auth_token_depth: Some(5),
                max_rv32_code_bytes: Some(65536),
                max_rv32_memory_bytes: Some(1_048_576),
            }
        );
    }

    #[test]
    fn unstated_parameters_stay_unset() {
        // A list that stops after the second id is what a chain predating the
        // later parameters states; nothing is invented for the rest.
        let config = network_config(vec![
            ConfigParam::kConfigParamMaxSHA256Buffers {
                max_sha256_buffers: 8,
            },
            ConfigParam::kConfigParamMaxCrossInvokeDepth {
                max_cross_invoke_depth: 64,
            },
        ]);

        let params = NetworkConfigParams::from_network_config(&config).unwrap();

        assert_eq!(
            params,
            NetworkConfigParams {
                max_sha256_buffers: Some(8),
                max_cross_invoke_depth: Some(64),
                ..NetworkConfigParams::default()
            }
        );
        assert_eq!(
            NetworkConfigParams::from_network_config(&network_config(vec![])).unwrap(),
            NetworkConfigParams::default()
        );
    }

    #[test]
    fn contract_size_in_force_is_the_stated_value_or_the_protocol_default() {
        assert_eq!(
            NetworkConfigParams::default().max_contract_size_bytes_in_force(),
            kDefaultMaxContractSizeBytes
        );
        assert_eq!(
            contract_size_cap(1024).max_contract_size_bytes_in_force(),
            1024
        );
    }

    #[test]
    fn transaction_within_bounds_passes() {
        let tx = transaction_with(vec![
            rv32(16, 1),
            rv32(kDefaultMaxContractSizeBytes as usize, 2),
        ]);

        assert_eq!(check_transaction(&tx, &max_contracts(2)), Ok(()));
        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Ok(())
        );
    }

    #[test]
    fn too_many_contracts_is_rejected_only_when_the_bound_is_stated() {
        let tx = transaction_with(vec![rv32(1, 1), rv32(1, 2), rv32(1, 3)]);

        assert_eq!(
            check_transaction(&tx, &max_contracts(2)),
            Err(TxInvalidDetail::TooManyContracts {
                attached: 3,
                max: 2
            })
        );
        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Ok(())
        );
    }

    #[test]
    fn oversized_contract_is_rejected_under_the_protocol_default() {
        let tx = transaction_with(vec![
            rv32(1, 1),
            rv32(kDefaultMaxContractSizeBytes as usize + 1, 2),
        ]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Err(TxInvalidDetail::ContractTooLarge {
                index: 1,
                size_bytes: kDefaultMaxContractSizeBytes + 1,
                max_bytes: kDefaultMaxContractSizeBytes,
            })
        );
    }

    #[test]
    fn stated_contract_size_cap_replaces_the_protocol_default() {
        // A stated cap governs in both directions: below the default it
        // rejects what the default would admit, above it admits what the
        // default would reject.
        let small = transaction_with(vec![rv32(1025, 1)]);
        let large = transaction_with(vec![rv32(kDefaultMaxContractSizeBytes as usize + 1, 1)]);

        assert_eq!(
            check_transaction(&small, &contract_size_cap(1024)),
            Err(TxInvalidDetail::ContractTooLarge {
                index: 0,
                size_bytes: 1025,
                max_bytes: 1024,
            })
        );
        assert_eq!(
            check_transaction(&large, &contract_size_cap(kDefaultMaxContractSizeBytes + 1)),
            Ok(())
        );
    }

    #[test]
    fn count_check_runs_before_size_check() {
        // Cheapest check first: the count is knowable without touching any
        // blob, and the executor orders them the same way.
        let tx = transaction_with(vec![rv32(kDefaultMaxContractSizeBytes as usize + 1, 1); 2]);

        assert!(matches!(
            check_transaction(&tx, &max_contracts(1)),
            Err(TxInvalidDetail::TooManyContracts { .. })
        ));
    }

    #[test]
    fn duplicate_contract_reports_both_positions() {
        let tx = transaction_with(vec![rv32(4, 1), rv32(4, 2), rv32(4, 1)]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Err(TxInvalidDetail::DuplicateContract {
                index: 2,
                first_index: 0,
            })
        );
    }

    #[test]
    fn native_contracts_count_but_carry_no_code() {
        let native = Contract::kNative {
            native_contract_id: HogexecPrecompiledNativeContracts::from(0),
        };
        let tx = transaction_with(vec![native.clone(), rv32(4, 1)]);

        assert_eq!(contract_code_size(&native), 0);
        assert_eq!(
            check_transaction(&tx, &max_contracts(1)),
            Err(TxInvalidDetail::TooManyContracts {
                attached: 2,
                max: 1
            })
        );
    }
}
