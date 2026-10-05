//! Public API types for committed-snapshot storage state reads.

use groundhog_xdr::storage_item::WireStorageItem;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::{Hex0x, HexBytes};
use crate::{BlockHeight, SHA256Hash};

/// Outcome of reading one storage location at the latest committed
/// snapshot.  `block_height` is always set to the snapshot the read
/// was performed against, even when no item exists at the requested
/// key — that lets callers compare against `chainTip` to gauge
/// freshness.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StorageItemRead {
    /// Committed block height the snapshot was read at.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub block_height: BlockHeight,
    /// The item stored at the requested key, or `null` if the key
    /// has no committed value at this snapshot.
    pub item: Option<StorageItemInfo>,
}

/// Latest-committed value at a single storage location, presented as
/// a tagged union so machine consumers can switch on `kind` without
/// running an XDR codec for the common cases.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StorageItemInfo {
    /// Non-negative int128 (e.g., HOG balance).  Encoded as a
    /// decimal string because i128 exceeds JSON-number precision.
    NonnegativeInt {
        /// Decimal-string-encoded non-negative int128.
        value: String,
    },

    /// Raw byte array stored at this key.
    Bytestring {
        /// The stored bytes.
        bytes: HexBytes,
    },

    /// Authorization data is registered at this address.  Presence
    /// indicator only — querying signer details requires a contract
    /// invocation.
    AuthData,

    /// A retrocausal queue is registered at this address.  Presence
    /// indicator only — RCQ data is not held in a single storage
    /// item, and per-item structure here would be misleading.
    /// Wallets that need to interpret RCQ contents invoke a
    /// contract that exposes them.
    RetrocausalQueue,

    /// Replay-cache entry exists at this key.  No payload — the
    /// presence is the signal.
    ReplayCacheEntry,

    /// Deployed contract metadata.
    DeployedContract {
        /// Hash of the deployed contract bytecode.
        hash: SHA256Hash,
        /// Bytecode size in bytes.
        size: u32,
        /// Contract type tag (raw protocol value).
        #[serde(rename = "contractType")]
        contract_type: i32,
    },

    /// Storage variant unknown to this client.  Indicates the
    /// protocol shipped a new variant that this build was not
    /// regenerated against — clients should treat as
    /// "uninterpretable" and fall back to alternative reads.
    Unknown {
        /// Raw protocol type id of the unrecognized variant.
        #[serde(rename = "typeId")]
        type_id: i32,
    },
}

impl From<&WireStorageItem> for StorageItemInfo {
    fn from(item: &WireStorageItem) -> Self {
        match item {
            WireStorageItem::kItemTypeBytestring { bytestring } => Self::Bytestring {
                bytes: HexBytes(bytestring.0.clone()),
            },
            WireStorageItem::kItemTypeNonnegativeInt { nnint } => {
                let v: i128 = (i128::from(nnint.highbits) << 64) | i128::from(nnint.lowbits);
                Self::NonnegativeInt {
                    value: v.to_string(),
                }
            }
            WireStorageItem::kItemTypeAuthData { .. } => Self::AuthData,
            WireStorageItem::kItemTypeRetrocausalQueue { .. } => Self::RetrocausalQueue,
            WireStorageItem::kItemTypeReplayCacheData => Self::ReplayCacheEntry,
            WireStorageItem::kItemTypeDeployedContractHash {
                deployed_contract_hash,
            } => Self::DeployedContract {
                hash: Hex0x(deployed_contract_hash.hash),
                size: deployed_contract_hash.contract_size,
                contract_type: deployed_contract_hash.contract_type.0,
            },
            other => Self::Unknown {
                type_id: other.r#type().0,
            },
        }
    }
}

impl From<WireStorageItem> for StorageItemInfo {
    fn from(item: WireStorageItem) -> Self {
        Self::from(&item)
    }
}
