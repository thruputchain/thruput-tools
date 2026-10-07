use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use groundhog_xdr::{header::ProposalHeader, types::ExtensionPoint};

use crate::hex::{Hex0x, HexBytes};
use crate::{
    BlockHash, BlockHeight, ConsensusValue, NetworkHash, Protocol, SHA256Hash, StorageHash,
    TimePoint, ValidatorID,
};

/// Current chain tip. Returned by chainTip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChainTip {
    /// Signed protocol block height, encoded as a decimal string.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,
    pub block_hash: SHA256Hash,
}

/// Block data returned by getBlockByNumber.
///
/// Maps the XDR `ConsensusValue` to RPC-friendly serialization
/// (camelCase fields, `0x`-prefixed hex). Add RPC-specific fields
/// here without modifying the wire type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BlockInfo {
    /// Canonical SHA-256 hash of the complete XDR-encoded consensus value.
    pub block_hash: BlockHash,
    pub protocol: Protocol,
    pub network_hash: NetworkHash,
    /// Hash of the post-block network configuration committed by consensus.
    pub config_hash: SHA256Hash,
    pub proposer: ValidatorID,
    /// Unix time in nanoseconds since 1970-01-01 UTC, encoded as a decimal string.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<TimePoint>")]
    pub block_timepoint: TimePoint,
    /// Signed protocol block height, encoded as a decimal string.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub block_height: BlockHeight,
    pub txs_hash: SHA256Hash,
    pub storage_hash: StorageHash,
    pub events_hash: SHA256Hash,
    pub prev_block_hash: BlockHash,
    pub proposal_txs_hash: SHA256Hash,
    /// Opaque protocol upgrades selected by consensus for this block.
    pub upgrades: Vec<HexBytes>,
}

/// Errors while projecting consensus-critical protocol data to JSON-RPC types.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockProjectionError {
    /// A future extension cannot be represented by this RPC build.
    #[error("unsupported protocol extension at {field}: {value}")]
    UnsupportedExtension {
        /// Extended protocol field.
        field: &'static str,
        /// Debug representation of the unsupported extension.
        value: String,
    },
    /// The consensus value could not be canonically encoded for hashing.
    #[error("failed to XDR-encode consensus value: {source}")]
    ConsensusValueEncoding {
        /// XDR encoding failure.
        #[source]
        source: xdrs::WriteError,
    },
}

impl TryFrom<ConsensusValue> for BlockInfo {
    type Error = BlockProjectionError;

    fn try_from(value: ConsensusValue) -> Result<Self, Self::Error> {
        let block_hash = consensus_value_hash(&value)
            .map_err(|source| BlockProjectionError::ConsensusValueEncoding { source })?;
        let ConsensusValue {
            protocol,
            ext,
            prev_hash,
            proposal_header,
            proposal_txs_hash,
            upgrades,
        } = value;
        ensure_base_extension("ConsensusValue.ext", ext)?;
        let ProposalHeader {
            ext,
            network_hash,
            config_hash,
            proposer,
            block_timepoint,
            block_height,
            txs_hash,
            storage_hash,
            events_hash,
        } = proposal_header;
        ensure_base_extension("ProposalHeader.ext", ext)?;

        Ok(Self {
            block_hash,
            protocol,
            network_hash: Hex0x(network_hash.into_inner()),
            config_hash: Hex0x(config_hash.into_inner()),
            proposer: Hex0x(proposer),
            block_timepoint,
            block_height,
            txs_hash: Hex0x(txs_hash.into_inner()),
            storage_hash: Hex0x(storage_hash.into_inner()),
            events_hash: Hex0x(events_hash.into_inner()),
            prev_block_hash: Hex0x(prev_hash.into_inner()),
            proposal_txs_hash: Hex0x(proposal_txs_hash.into_inner()),
            upgrades: upgrades
                .0
                .into_iter()
                .map(|upgrade| HexBytes(upgrade.0))
                .collect(),
        })
    }
}

fn ensure_base_extension(
    field: &'static str,
    extension: ExtensionPoint,
) -> Result<(), BlockProjectionError> {
    match extension {
        ExtensionPoint::_0 => Ok(()),
        other => Err(BlockProjectionError::UnsupportedExtension {
            field,
            value: format!("{other:?}"),
        }),
    }
}

/// Return the canonical hash of an XDR-encoded consensus value.
pub fn consensus_value_hash(value: &ConsensusValue) -> Result<BlockHash, xdrs::WriteError> {
    xdrs::txs::encode(value).map(|encoded| {
        Hex0x(groundhog_xdr::types::SHA256Hash::from(<[u8; 32]>::from(
            Sha256::digest(encoded),
        )))
    })
}
