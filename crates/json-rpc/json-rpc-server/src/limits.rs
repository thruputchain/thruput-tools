//! Pre-submission bound checks mirrored from the executor.
//!
//! hogd's `SystemCallHandler::perform_initial_validation` rejects a
//! transaction deterministically, before running any of it, when it
//! attaches more contracts than the chain's network config permits, when
//! any attached contract's code exceeds a fixed size cap, or when the same
//! contract is attached twice. Applying the same checks at the JSON-RPC
//! boundary turns a silent later failure into an immediate, structured
//! `TxInvalid` error.
//!
//! None of this is a trust boundary: the executor re-checks everything.
//! The chain-derived bound is read from the tip and may lag a block, which
//! is harmless because these parameters change only through explicit
//! config upgrades.

use groundhog_json_rpc_types::error::TxInvalidDetail;
use groundhog_xdr::{
    contract::Contract,
    network_config::{ConfigParam, NetworkConfig},
    transaction::Transaction,
};

use crate::error::RpcServerError;

/// Largest contract code blob the executor accepts, in bytes.
///
/// Mirrors hogd's `kMaxContractSizeBytes`. It is a protocol constant rather
/// than a network-config parameter today; if it moves into the chain config
/// it belongs on [`TxLimits`] instead.
pub const MAX_CONTRACT_SIZE_BYTES: u32 = 256 * 1024;

/// Value the protocol assumes for `maxNewContractsPerTx` when a chain's
/// network config does not state one. Mirrors hogd's
/// `kDefaultMaxNewContractsPerTx`.
pub const DEFAULT_MAX_NEW_CONTRACTS_PER_TX: u32 = 4;

/// Transaction bounds in force at a chain tip, as read from its network
/// config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxLimits {
    /// Contracts one transaction may attach in `contractsToDeploy`.
    pub max_new_contracts_per_tx: u32,
}

impl TxLimits {
    /// Bounds a chain with an empty parameter list is under.
    pub const PROTOCOL_DEFAULTS: Self = Self {
        max_new_contracts_per_tx: DEFAULT_MAX_NEW_CONTRACTS_PER_TX,
    };

    /// Read the bounds a tip's network config puts in force.
    ///
    /// A parameter the config does not state takes its protocol default.
    /// A config version this build cannot interpret is reported as a
    /// backend error rather than guessed at.
    pub fn from_network_config(config: &NetworkConfig) -> Result<Self, RpcServerError> {
        let params = match config {
            NetworkConfig::_0 { v0 } => &v0.params,
            other => {
                return Err(RpcServerError::BackendUnavailable(format!(
                    "unsupported network config version: {other:?}"
                )));
            }
        };
        let max_new_contracts_per_tx = params
            .iter()
            .find_map(|param| match param {
                ConfigParam::kConfigParamMaxNewContractsPerTx {
                    max_new_contracts_per_tx,
                } => Some(*max_new_contracts_per_tx),
                _ => None,
            })
            .unwrap_or(DEFAULT_MAX_NEW_CONTRACTS_PER_TX);
        Ok(Self {
            max_new_contracts_per_tx,
        })
    }
}

/// Check a decoded transaction against the executor's pre-execution bounds.
///
/// The per-contract size cap and the duplicate-attachment rule are
/// protocol constants and always apply. The attached-contract count is
/// checked only when `limits` carries a reading from the chain, so an
/// unreachable tip degrades to fewer checks rather than to guesses.
pub fn check_transaction(
    tx: &Transaction,
    limits: Option<&TxLimits>,
) -> Result<(), TxInvalidDetail> {
    let contracts = &tx.contracts_to_deploy.0;
    let attached = u32::try_from(contracts.len()).unwrap_or(u32::MAX);

    if let Some(limits) = limits
        && attached > limits.max_new_contracts_per_tx
    {
        return Err(TxInvalidDetail::TooManyContracts {
            attached,
            max: limits.max_new_contracts_per_tx,
        });
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
        precompiles::HogexecPrecompiledNativeContracts,
    };
    use xdr_common::{BytesM, VecM};

    use super::*;
    use crate::tx::test_fixtures::signed_transaction_with_contracts;

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

    fn transaction_with(contracts: Vec<Contract>) -> Transaction {
        signed_transaction_with_contracts(contracts).tx
    }

    #[test]
    fn limits_come_from_the_stated_parameter() {
        let config = network_config(vec![
            ConfigParam::kConfigParamMaxSHA256Buffers {
                max_sha256_buffers: 8,
            },
            ConfigParam::kConfigParamMaxCrossInvokeDepth {
                max_cross_invoke_depth: 256,
            },
            ConfigParam::kConfigParamMaxNewContractsPerTx {
                max_new_contracts_per_tx: 7,
            },
        ]);

        let limits = TxLimits::from_network_config(&config).unwrap();

        assert_eq!(limits.max_new_contracts_per_tx, 7);
    }

    #[test]
    fn unstated_parameter_takes_the_protocol_default() {
        let config = network_config(vec![ConfigParam::kConfigParamMaxSHA256Buffers {
            max_sha256_buffers: 8,
        }]);

        let limits = TxLimits::from_network_config(&config).unwrap();

        assert_eq!(limits, TxLimits::PROTOCOL_DEFAULTS);
        assert_eq!(
            limits.max_new_contracts_per_tx,
            DEFAULT_MAX_NEW_CONTRACTS_PER_TX
        );
    }

    #[test]
    fn transaction_within_bounds_passes() {
        let tx = transaction_with(vec![rv32(16, 1), rv32(MAX_CONTRACT_SIZE_BYTES as usize, 2)]);
        let limits = TxLimits {
            max_new_contracts_per_tx: 2,
        };

        assert_eq!(check_transaction(&tx, Some(&limits)), Ok(()));
        assert_eq!(check_transaction(&tx, None), Ok(()));
    }

    #[test]
    fn too_many_contracts_is_rejected_only_with_a_limits_reading() {
        let tx = transaction_with(vec![rv32(1, 1), rv32(1, 2), rv32(1, 3)]);
        let limits = TxLimits {
            max_new_contracts_per_tx: 2,
        };

        assert_eq!(
            check_transaction(&tx, Some(&limits)),
            Err(TxInvalidDetail::TooManyContracts {
                attached: 3,
                max: 2
            })
        );
        assert_eq!(check_transaction(&tx, None), Ok(()));
    }

    #[test]
    fn oversized_contract_is_rejected_even_without_a_limits_reading() {
        let tx = transaction_with(vec![
            rv32(1, 1),
            rv32(MAX_CONTRACT_SIZE_BYTES as usize + 1, 2),
        ]);

        assert_eq!(
            check_transaction(&tx, None),
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
        let limits = TxLimits {
            max_new_contracts_per_tx: 1,
        };

        assert!(matches!(
            check_transaction(&tx, Some(&limits)),
            Err(TxInvalidDetail::TooManyContracts { .. })
        ));
    }

    #[test]
    fn duplicate_contract_reports_both_positions() {
        let tx = transaction_with(vec![rv32(4, 1), rv32(4, 2), rv32(4, 1)]);

        assert_eq!(
            check_transaction(&tx, None),
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
        let limits = TxLimits {
            max_new_contracts_per_tx: 1,
        };

        assert_eq!(contract_code_size(&native), 0);
        assert_eq!(
            check_transaction(&tx, Some(&limits)),
            Err(TxInvalidDetail::TooManyContracts {
                attached: 2,
                max: 1
            })
        );
    }
}
