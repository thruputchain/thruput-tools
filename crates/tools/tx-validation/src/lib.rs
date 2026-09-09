//! Off-chain mirror of the bounds hogd checks before executing a
//! transaction.
//!
//! hogd's `SystemCallHandler::perform_initial_validation` rejects a
//! transaction deterministically, before running any of it, when it
//! attaches more contracts than the chain's network config permits, when
//! any attached contract's code exceeds a fixed size cap, or when the same
//! contract is attached twice. Applying the same checks before submission
//! turns a silent later failure into an immediate, structured
//! [`TxInvalidDetail`].
//!
//! The chain-configured side of those bounds is read from the network
//! config a db tip carries, flattened once into [`NetworkConfigParams`] so
//! every later access is a field read. A bound the config does not state
//! is not enforced here: this crate never substitutes a default for a
//! value it did not see. None of this is a trust boundary: the executor
//! re-checks everything, and a reading that lags a block is harmless
//! because these parameters change only through explicit config upgrades.

use groundhog_json_rpc_types::error::TxInvalidDetail;
use groundhog_xdr::{
    contract::Contract,
    network_config::{ConfigParam, NetworkConfig},
    transaction::Transaction,
};

/// Largest contract code blob the executor accepts, in bytes.
///
/// Mirrors hogd's `kMaxContractSizeBytes`. It is a protocol constant rather
/// than a network-config parameter today; if it moves into the chain config
/// it belongs on [`NetworkConfigParams`] instead.
pub const MAX_CONTRACT_SIZE_BYTES: u32 = 256 * 1024;

/// The execution parameters a chain's network config states, flattened
/// from the list that carries them.
///
/// Resolve once where a tip is read, then each parameter is a field rather
/// than a scan of the list. A field is `None` when the config does not
/// state that parameter; checks that depend on it are then skipped rather
/// than run against a guessed value. `Default` is the reading with nothing
/// stated, and struct update from it is the way to state a subset.
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
                // A parameter this build does not name cannot have decoded;
                // the arm exists only because the enum is `#[non_exhaustive]`.
                _ => {}
            }
        }
        Ok(resolved)
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
/// The per-contract size cap and the duplicate-attachment rule are
/// protocol constants and always apply. The attached-contract count is
/// checked only when `params` states a bound for it; pass
/// `NetworkConfigParams::default()` when no chain reading is available.
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

    for (index, contract) in contracts.iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        let size_bytes = u32::try_from(contract_code_size(contract)).unwrap_or(u32::MAX);
        if size_bytes > MAX_CONTRACT_SIZE_BYTES {
            return Err(TxInvalidDetail::ContractTooLarge {
                index,
                size_bytes,
                max_bytes: MAX_CONTRACT_SIZE_BYTES,
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
        ]);

        let params = NetworkConfigParams::from_network_config(&config).unwrap();

        assert_eq!(
            params,
            NetworkConfigParams {
                max_sha256_buffers: Some(3),
                max_cross_invoke_depth: Some(17),
                max_new_contracts_per_tx: Some(7),
                max_bytestring_len: Some(4096),
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
                max_new_contracts_per_tx: None,
                max_bytestring_len: None,
            }
        );
        assert_eq!(
            NetworkConfigParams::from_network_config(&network_config(vec![])).unwrap(),
            NetworkConfigParams::default()
        );
    }

    #[test]
    fn transaction_within_bounds_passes() {
        let tx = transaction_with(vec![rv32(16, 1), rv32(MAX_CONTRACT_SIZE_BYTES as usize, 2)]);

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
    fn oversized_contract_is_rejected_without_any_stated_bound() {
        let tx = transaction_with(vec![
            rv32(1, 1),
            rv32(MAX_CONTRACT_SIZE_BYTES as usize + 1, 2),
        ]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Err(TxInvalidDetail::ContractTooLarge {
                index: 1,
                size_bytes: MAX_CONTRACT_SIZE_BYTES + 1,
                max_bytes: MAX_CONTRACT_SIZE_BYTES,
            })
        );
    }

    #[test]
    fn count_check_runs_before_size_check() {
        // Cheapest check first: the count is knowable without touching any
        // blob, and the executor orders them the same way.
        let tx = transaction_with(vec![rv32(MAX_CONTRACT_SIZE_BYTES as usize + 1, 1); 2]);

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
