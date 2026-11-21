//! Off-chain mirror of the bounds hogd checks before executing a
//! transaction.
//!
//! hogd's `SystemCallHandler::perform_initial_validation` rejects a
//! transaction deterministically, before running any of it, when its root
//! invocation budgets more gas than the chain's `max_tx_gas`, when it
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
//! `kDefaultMaxRv32MemoryBytes`, and `max_tx_gas`'s 1,000,000,000), that
//! constant. Nothing is copied from
//! hogd's own fallbacks. None of this is a trust boundary: the executor
//! re-checks everything, and a reading that lags a block is harmless
//! because these parameters change only through explicit config upgrades.

use thruput_json_rpc_types::error::TxInvalidDetail;
use thruput_rv32_artifact::{
    DEFAULT_MAX_CODE_BYTES, DEFAULT_MEMORY_BYTES, DeclaredSizes, declared_sizes,
};
use thruput_xdr::{
    contract::{Contract, kDefaultMaxContractSizeBytes},
    transaction::Transaction,
};

/// `kDefaultMaxRv32CodeBytes` from the protocol's `hog_exec_constants.x`: the
/// code bound in force for a configuration that does not state one.
///
/// The Rust protocol bindings do not bind that file yet, so the value is
/// pinned here and held to the artifact crate's restatement of it.
const DEFAULT_MAX_RV32_CODE_BYTES: u32 = 262_144;
/// `kDefaultMaxRv32MemoryBytes` from `hog_exec_constants.x`: the guest memory
/// bound in force for a configuration that does not state one.
const DEFAULT_MAX_RV32_MEMORY_BYTES: u32 = 65_536;
/// The `max_tx_gas` in force for a configuration that does not state one, as
/// the protocol documents it on `kConfigParamMaxTxGas`. The bindings carry no
/// constant for it, so it is pinned here.
const DEFAULT_MAX_TX_GAS: u64 = 1_000_000_000;
const _: () = assert!(DEFAULT_MAX_RV32_CODE_BYTES as usize == DEFAULT_MAX_CODE_BYTES);
const _: () = assert!(DEFAULT_MAX_RV32_MEMORY_BYTES as usize == DEFAULT_MEMORY_BYTES);

pub use thruput_json_rpc_types::network_config::{NetworkConfigParams, UnsupportedNetworkConfig};

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
    /// [`NetworkConfigParams::max_auth_token_depth`].
    MaxAuthTokenDepth,
    /// [`NetworkConfigParams::max_rv32_code_bytes`].
    MaxRv32CodeBytes,
    /// [`NetworkConfigParams::max_rv32_memory_bytes`].
    MaxRv32MemoryBytes,
    /// [`NetworkConfigParams::max_tx_gas`].
    MaxTxGas,
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
        Self::MaxAuthTokenDepth,
        Self::MaxRv32CodeBytes,
        Self::MaxRv32MemoryBytes,
        Self::MaxTxGas,
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
            Self::MaxAuthTokenDepth => "max_auth_token_depth",
            Self::MaxRv32CodeBytes => "max_rv32_code_bytes",
            Self::MaxRv32MemoryBytes => "max_rv32_memory_bytes",
            Self::MaxTxGas => "max_tx_gas",
        }
    }

    /// The value `params` states for this parameter, `None` when unstated.
    ///
    /// This is the binding between a name and the field it reads, so a
    /// parameter added to one has to be added to the other.
    pub fn stated_in(self, params: &NetworkConfigParams) -> Option<u64> {
        match self {
            Self::MaxSha256Buffers => params.max_sha256_buffers.map(u64::from),
            Self::MaxCrossInvokeDepth => params.max_cross_invoke_depth.map(u64::from),
            Self::MaxNewContractsPerTx => params.max_new_contracts_per_tx.map(u64::from),
            Self::MaxBytestringLen => params.max_bytestring_len.map(u64::from),
            Self::MaxContractSizeBytes => params.max_contract_size_bytes.map(u64::from),
            Self::MaxSavepointDepth => params.max_savepoint_depth.map(u64::from),
            Self::MaxAuthTokenDepth => params.max_auth_token_depth.map(u64::from),
            Self::MaxRv32CodeBytes => params.max_rv32_code_bytes.map(u64::from),
            Self::MaxRv32MemoryBytes => params.max_rv32_memory_bytes.map(u64::from),
            Self::MaxTxGas => params.max_tx_gas,
        }
    }

    /// The names this build knows, comma-separated, for diagnostics.
    pub fn known_names() -> String {
        Self::ALL
            .iter()
            .copied()
            .map(Self::as_str)
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

/// Check a decoded transaction against the executor's pre-execution bounds.
///
/// The root invocation's `gas_limit` is checked first, against the
/// `max_tx_gas` in force (1,000,000,000 when unstated), as hogd checks it
/// before any attachment. The duplicate-attachment rule always applies. The attached-contract
/// count is checked only when `params` states a bound for it. Each
/// attached contract's code is checked against the size in force
/// (`kDefaultMaxContractSizeBytes` when unstated), and each attached RV32
/// artifact's declared guest memory and code size against
/// `kDefaultMaxRv32MemoryBytes` and `kDefaultMaxRv32CodeBytes` when
/// unstated, so pass `NetworkConfigParams::default()` when no chain
/// reading is available and the protocol's own values apply.
pub fn check_transaction(
    tx: &Transaction,
    params: &NetworkConfigParams,
) -> Result<(), TxInvalidDetail> {
    let max_gas = params.max_tx_gas.unwrap_or(DEFAULT_MAX_TX_GAS);
    if tx.invocation.gas_limit > max_gas {
        return Err(TxInvalidDetail::GasLimitTooHigh {
            gas_limit: tx.invocation.gas_limit,
            max_gas,
        });
    }

    let contracts = &tx.contracts_to_deploy.0;
    let attached = u32::try_from(contracts.len()).unwrap_or(u32::MAX);

    if let Some(max) = params.max_new_contracts_per_tx
        && attached > max
    {
        return Err(TxInvalidDetail::TooManyContracts { attached, max });
    }

    let max_bytes = params
        .max_contract_size_bytes
        .unwrap_or(kDefaultMaxContractSizeBytes);
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
    let max_memory_bytes = params
        .max_rv32_memory_bytes
        .unwrap_or(DEFAULT_MAX_RV32_MEMORY_BYTES);
    let max_code_bytes = params
        .max_rv32_code_bytes
        .unwrap_or(DEFAULT_MAX_RV32_CODE_BYTES);
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
