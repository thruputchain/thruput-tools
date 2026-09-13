//! Off-chain mirror of the bounds hogd checks before executing a
//! transaction.
//!
//! hogd's `SystemCallHandler::perform_initial_validation` rejects a
//! transaction deterministically, before running any of it, when it
//! attaches more contracts than the chain's network config permits, when
//! any attached contract's code exceeds the size the config admits, when
//! the same contract is attached twice, or when an attached RV32 artifact
//! declares more guest memory or code than the config admits (the
//! attachment fails verification in `NewContractSet::make`). Applying the
//! same checks before submission turns a silent later failure into an
//! immediate, structured [`TxInvalidDetail`].
//!
//! The chain-configured bounds are read from the network config a db tip
//! carries, flattened once into [`NetworkConfigParams`] so every later
//! access is a field read. A bound is enforced only with a value the
//! protocol defines: the one the config states, or, for the parameters
//! whose unstated value the protocol itself fixes
//! (`kDefaultMaxContractSizeBytes`, `kDefaultMaxRv32CodeBytes`,
//! `kDefaultMaxRv32MemoryBytes`), that constant. Nothing is copied from
//! hogd's own fallbacks. None of this is a trust boundary: the executor
//! re-checks everything, and a reading that lags a block is harmless
//! because these parameters change only through explicit config upgrades.

use groundhog_json_rpc_types::error::TxInvalidDetail;
use groundhog_rv32_artifact::{
    DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, DeclaredSizes, declared_sizes,
};
use groundhog_xdr::{
    contract::{Contract, kDefaultMaxContractSizeBytes},
    network_config::{ConfigParam, NetworkConfig},
    transaction::Transaction,
};

/// `kDefaultMaxRv32CodeBytes` from the protocol's `hog_exec_constants.x`: the
/// code bound in force for a configuration that does not state one.
///
/// The Rust protocol bindings do not bind that file yet, so the value is
/// pinned here and held to the artifact crate's restatement of it.
const DEFAULT_MAX_RV32_CODE_BYTES: u32 = 65_536;
/// `kDefaultMaxRv32MemoryBytes` from `hog_exec_constants.x`: the guest memory
/// bound in force for a configuration that does not state one.
const DEFAULT_MAX_RV32_MEMORY_BYTES: u32 = 65_536;
const _: () = assert!(DEFAULT_MAX_RV32_CODE_BYTES as usize == DEFAULT_MAX_CODE_BYTES);
const _: () = assert!(DEFAULT_MAX_RV32_MEMORY_BYTES as usize == DEFAULT_MEMORY_BYTES);

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
    /// Code bytes an RV32 artifact may carry to be attached. The value
    /// applied when unstated is [`Self::max_rv32_code_bytes_in_force`].
    pub max_rv32_code_bytes: Option<u32>,
    /// Guest data memory bytes an RV32 artifact may declare to be attached.
    /// The value applied when unstated is
    /// [`Self::max_rv32_memory_bytes_in_force`].
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

    /// Code bytes an attached RV32 artifact may carry under this config.
    ///
    /// The protocol fixes `kDefaultMaxRv32CodeBytes` as the value in force
    /// for a configuration that does not state the parameter, so, like the
    /// contract size, this bound is always known.
    pub fn max_rv32_code_bytes_in_force(&self) -> u32 {
        self.max_rv32_code_bytes
            .unwrap_or(DEFAULT_MAX_RV32_CODE_BYTES)
    }

    /// Guest data memory bytes an attached RV32 artifact may declare under
    /// this config; `kDefaultMaxRv32MemoryBytes` when unstated.
    pub fn max_rv32_memory_bytes_in_force(&self) -> u32 {
        self.max_rv32_memory_bytes
            .unwrap_or(DEFAULT_MAX_RV32_MEMORY_BYTES)
    }
}

/// The name a chain execution parameter carries outside its XDR encoding.
///
/// The protocol encodes each parameter as a `ConfigParam` union arm; every
/// surface that has to *say* which parameter it means spells the arm's field
/// name instead. hog-devnode's `--network-config NAME=VALUE` flag keys its
/// genesis overrides by these names and records them under the same names in
/// a data directory's bootstrap metadata. This enum is the one place the
/// Rust side spells them, so a parameter cannot reach hog-devnode under a
/// name no parameter has.
///
/// Naming a parameter says nothing about which values it admits; that stays
/// hog-devnode's call, against the ceilings its build was compiled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ConfigParamName {
    /// [`NetworkConfigParams::max_sha256_buffers`].
    MaxSha256Buffers,
    /// [`NetworkConfigParams::max_cross_invoke_depth`].
    MaxCrossInvokeDepth,
    /// [`NetworkConfigParams::max_new_contracts_per_tx`].
    MaxNewContractsPerTx,
    /// [`NetworkConfigParams::max_bytestring_len`].
    MaxBytestringLen,
    /// [`NetworkConfigParams::max_contract_size_bytes`].
    MaxContractSizeBytes,
    /// [`NetworkConfigParams::max_savepoint_depth`].
    MaxSavepointDepth,
    /// [`NetworkConfigParams::max_sha256_buffer_bytes`].
    MaxSha256BufferBytes,
    /// [`NetworkConfigParams::max_auth_token_depth`].
    MaxAuthTokenDepth,
    /// [`NetworkConfigParams::max_rv32_code_bytes`].
    MaxRv32CodeBytes,
    /// [`NetworkConfigParams::max_rv32_memory_bytes`].
    MaxRv32MemoryBytes,
}

impl ConfigParamName {
    /// Every parameter this build names, in protocol id order.
    pub const ALL: &[Self] = &[
        Self::MaxSha256Buffers,
        Self::MaxCrossInvokeDepth,
        Self::MaxNewContractsPerTx,
        Self::MaxBytestringLen,
        Self::MaxContractSizeBytes,
        Self::MaxSavepointDepth,
        Self::MaxSha256BufferBytes,
        Self::MaxAuthTokenDepth,
        Self::MaxRv32CodeBytes,
        Self::MaxRv32MemoryBytes,
    ];

    /// The protocol's field name for this parameter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MaxSha256Buffers => "max_sha256_buffers",
            Self::MaxCrossInvokeDepth => "max_cross_invoke_depth",
            Self::MaxNewContractsPerTx => "max_new_contracts_per_tx",
            Self::MaxBytestringLen => "max_bytestring_len",
            Self::MaxContractSizeBytes => "max_contract_size_bytes",
            Self::MaxSavepointDepth => "max_savepoint_depth",
            Self::MaxSha256BufferBytes => "max_sha256_buffer_bytes",
            Self::MaxAuthTokenDepth => "max_auth_token_depth",
            Self::MaxRv32CodeBytes => "max_rv32_code_bytes",
            Self::MaxRv32MemoryBytes => "max_rv32_memory_bytes",
        }
    }

    /// The value `params` states for this parameter, `None` when unstated.
    ///
    /// This is the binding between a name and the field it reads, so a
    /// parameter added to one has to be added to the other.
    pub fn stated_in(self, params: &NetworkConfigParams) -> Option<u32> {
        match self {
            Self::MaxSha256Buffers => params.max_sha256_buffers,
            Self::MaxCrossInvokeDepth => params.max_cross_invoke_depth,
            Self::MaxNewContractsPerTx => params.max_new_contracts_per_tx,
            Self::MaxBytestringLen => params.max_bytestring_len,
            Self::MaxContractSizeBytes => params.max_contract_size_bytes,
            Self::MaxSavepointDepth => params.max_savepoint_depth,
            Self::MaxSha256BufferBytes => params.max_sha256_buffer_bytes,
            Self::MaxAuthTokenDepth => params.max_auth_token_depth,
            Self::MaxRv32CodeBytes => params.max_rv32_code_bytes,
            Self::MaxRv32MemoryBytes => params.max_rv32_memory_bytes,
        }
    }

    /// The names this build knows, comma-separated, for diagnostics.
    pub fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|param| param.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for ConfigParamName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ConfigParamName {
    type Err = UnknownConfigParamName;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|param| param.as_str() == name)
            .ok_or_else(|| UnknownConfigParamName {
                name: name.to_string(),
            })
    }
}

/// A name that does not belong to any parameter this build knows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unknown network parameter {name:?}; this build knows {}",
    ConfigParamName::known_names()
)]
pub struct UnknownConfigParamName {
    /// The name as it was written.
    pub name: String,
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
/// [`NetworkConfigParams::max_contract_size_bytes_in_force`], and each
/// attached RV32 artifact's declared guest memory and code size against
/// [`NetworkConfigParams::max_rv32_memory_bytes_in_force`] and
/// [`NetworkConfigParams::max_rv32_code_bytes_in_force`], so pass
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

    // hogd verifies each attachment after the gates above, and its artifact
    // parser checks the declared memory before the code size; the same order
    // here, so both name the same first violation.
    let max_memory_bytes = params.max_rv32_memory_bytes_in_force();
    let max_code_bytes = params.max_rv32_code_bytes_in_force();
    for (index, contract) in contracts.iter().enumerate() {
        let Some(sizes) = rv32_declared_sizes(contract) else {
            continue;
        };
        let index = u32::try_from(index).unwrap_or(u32::MAX);
        if sizes.memory_bytes > max_memory_bytes {
            return Err(TxInvalidDetail::Rv32MemoryTooLarge {
                index,
                memory_bytes: sizes.memory_bytes,
                max_bytes: max_memory_bytes,
            });
        }
        if sizes.code_bytes > max_code_bytes {
            return Err(TxInvalidDetail::Rv32CodeTooLarge {
                index,
                code_bytes: sizes.code_bytes,
                max_bytes: max_code_bytes,
            });
        }
    }

    Ok(())
}

/// The sizes an attached RV32 artifact's header declares, when it carries one.
///
/// A blob without a readable v5 header is left to the executor, which rejects
/// it as unverifiable: this pre-check speaks only for headers it can read, and
/// a contract shape this build does not know carries none.
fn rv32_declared_sizes(contract: &Contract) -> Option<DeclaredSizes> {
    match contract {
        Contract::kRv32 { rv32_contract } => declared_sizes(&rv32_contract.data.0),
        _ => None,
    }
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

    /// The `ConfigParam` that states `value` for `name`.
    ///
    /// Exhaustive on purpose: a parameter added to [`ConfigParamName`] does
    /// not compile here until it is given its union arm, and the test below
    /// then fails until `from_network_config` lands it on its field.
    fn stated(name: ConfigParamName, value: u32) -> ConfigParam {
        match name {
            ConfigParamName::MaxSha256Buffers => ConfigParam::kConfigParamMaxSHA256Buffers {
                max_sha256_buffers: value,
            },
            ConfigParamName::MaxCrossInvokeDepth => ConfigParam::kConfigParamMaxCrossInvokeDepth {
                max_cross_invoke_depth: value,
            },
            ConfigParamName::MaxNewContractsPerTx => {
                ConfigParam::kConfigParamMaxNewContractsPerTx {
                    max_new_contracts_per_tx: value,
                }
            }
            ConfigParamName::MaxBytestringLen => ConfigParam::kConfigParamMaxBytestringLen {
                max_bytestring_len: value,
            },
            ConfigParamName::MaxContractSizeBytes => {
                ConfigParam::kConfigParamMaxContractSizeBytes {
                    max_contract_size_bytes: value,
                }
            }
            ConfigParamName::MaxSavepointDepth => ConfigParam::kConfigParamMaxSavepointDepth {
                max_savepoint_depth: value,
            },
            ConfigParamName::MaxSha256BufferBytes => {
                ConfigParam::kConfigParamMaxSHA256BufferBytes {
                    max_sha256_buffer_bytes: value,
                }
            }
            ConfigParamName::MaxAuthTokenDepth => ConfigParam::kConfigParamMaxAuthTokenDepth {
                max_auth_token_depth: value,
            },
            ConfigParamName::MaxRv32CodeBytes => ConfigParam::kConfigParamMaxRv32CodeBytes {
                max_rv32_code_bytes: value,
            },
            ConfigParamName::MaxRv32MemoryBytes => ConfigParam::kConfigParamMaxRv32MemoryBytes {
                max_rv32_memory_bytes: value,
            },
        }
    }

    #[test]
    fn every_named_parameter_flattens_onto_the_field_it_names() {
        for (index, name) in ConfigParamName::ALL.iter().copied().enumerate() {
            let value = 1_000 + index as u32;

            let params = NetworkConfigParams::from_network_config(&network_config(vec![stated(
                name, value,
            )]))
            .unwrap();

            assert_eq!(name.stated_in(&params), Some(value), "{name}");
            for other in ConfigParamName::ALL.iter().copied() {
                if other != name {
                    assert_eq!(other.stated_in(&params), None, "{name} also stated {other}");
                }
            }
        }
    }

    #[test]
    fn names_round_trip_and_an_unknown_one_lists_the_known_names() {
        for name in ConfigParamName::ALL.iter().copied() {
            assert_eq!(name.as_str().parse::<ConfigParamName>(), Ok(name));
            assert_eq!(name.to_string(), name.as_str());
        }

        let error = "max_rv32_memmory_bytes"
            .parse::<ConfigParamName>()
            .unwrap_err();

        assert_eq!(error.name, "max_rv32_memmory_bytes");
        assert!(
            error.to_string().contains("max_rv32_memory_bytes"),
            "{error}"
        );
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

    fn rv32_caps(
        max_rv32_code_bytes: Option<u32>,
        max_rv32_memory_bytes: Option<u32>,
    ) -> NetworkConfigParams {
        NetworkConfigParams {
            max_rv32_code_bytes,
            max_rv32_memory_bytes,
            ..NetworkConfigParams::default()
        }
    }

    /// A well-formed v5 artifact carrying `code_bytes` of code and declaring
    /// `memory_bytes` of guest memory, as an attached contract.
    fn artifact(code_bytes: usize, memory_bytes: usize) -> Contract {
        let artifact =
            groundhog_rv32_artifact::build_artifact(&vec![0u8; code_bytes], &[], 0, memory_bytes)
                .unwrap();
        Contract::kRv32 {
            rv32_contract: ContractBlob {
                data: BytesM(artifact),
            },
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
    fn rv32_bounds_in_force_are_the_stated_values_or_the_protocol_defaults() {
        let unstated = NetworkConfigParams::default();
        assert_eq!(unstated.max_rv32_code_bytes_in_force(), 65_536);
        assert_eq!(unstated.max_rv32_memory_bytes_in_force(), 65_536);

        let stated = rv32_caps(Some(4096), Some(1 << 20));
        assert_eq!(stated.max_rv32_code_bytes_in_force(), 4096);
        assert_eq!(stated.max_rv32_memory_bytes_in_force(), 1 << 20);
    }

    #[test]
    fn rv32_sizes_at_the_protocol_defaults_pass() {
        let tx = transaction_with(vec![artifact(DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES)]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Ok(())
        );
    }

    #[test]
    fn rv32_memory_over_the_bound_is_rejected_and_checked_before_code() {
        // Both fields over: memory is reported, as hogd's artifact parser
        // orders the two checks.
        let tx = transaction_with(vec![
            artifact(16, 1),
            artifact(DEFAULT_MAX_CODE_BYTES + 4, DEFAULT_MEMORY_BYTES + 1),
        ]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Err(TxInvalidDetail::Rv32MemoryTooLarge {
                index: 1,
                memory_bytes: 65_537,
                max_bytes: 65_536,
            })
        );
    }

    #[test]
    fn rv32_code_over_the_bound_is_rejected() {
        let tx = transaction_with(vec![artifact(
            DEFAULT_MAX_CODE_BYTES + 4,
            DEFAULT_MEMORY_BYTES,
        )]);

        assert_eq!(
            check_transaction(&tx, &NetworkConfigParams::default()),
            Err(TxInvalidDetail::Rv32CodeTooLarge {
                index: 0,
                code_bytes: 65_540,
                max_bytes: 65_536,
            })
        );
    }

    #[test]
    fn stated_rv32_bounds_replace_the_protocol_defaults() {
        // Above the defaults, a chain configured for it admits the artifact.
        let large = transaction_with(vec![artifact(
            DEFAULT_MAX_CODE_BYTES + 4,
            DEFAULT_MEMORY_BYTES + 1,
        )]);
        assert_eq!(
            check_transaction(&large, &rv32_caps(Some(65_540), Some(65_537))),
            Ok(())
        );

        // Below the defaults, a stated cap rejects what the default admits.
        let small = transaction_with(vec![artifact(1024, 4096)]);
        assert_eq!(
            check_transaction(&small, &rv32_caps(Some(1020), None)),
            Err(TxInvalidDetail::Rv32CodeTooLarge {
                index: 0,
                code_bytes: 1024,
                max_bytes: 1020,
            })
        );
        assert_eq!(
            check_transaction(&small, &rv32_caps(None, Some(4095))),
            Err(TxInvalidDetail::Rv32MemoryTooLarge {
                index: 0,
                memory_bytes: 4096,
                max_bytes: 4095,
            })
        );
    }

    #[test]
    fn a_blob_without_an_artifact_header_is_left_to_the_executor() {
        // Not a v5 artifact: the executor rejects it as unverifiable, and this
        // pre-check has no header to read a declared size from.
        let blob = rv32(DEFAULT_MAX_CODE_BYTES + 1024, 0x5a);
        assert_eq!(rv32_declared_sizes(&blob), None);

        let tx = transaction_with(vec![blob]);
        assert_eq!(
            check_transaction(&tx, &rv32_caps(Some(16), Some(16))),
            Ok(())
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
