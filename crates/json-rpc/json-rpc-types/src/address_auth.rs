//! Public API types for reading an address's authorization configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thruput_xdr::address_auth::AddressAuthData;
use thruput_xdr::types::PublicKey;

use crate::hex::Hex0x;
use crate::{BlockHeight, ContractAddress, Ed25519PublicKey};

/// Authorization configuration registered at one address, read from the
/// latest committed snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddressAuthDataResponse {
    /// Address whose authorization configuration was read.
    pub address: ContractAddress,
    /// Committed block height used for the read, reported whether or not
    /// the address has a registered configuration.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub height: BlockHeight,
    /// The registered configuration, or `null` when the address has none.
    ///
    /// The two cases take different auth claims. An address without auth
    /// data accepts only `kPubkeySignature` claims whose preimage derives
    /// the address. An address with auth data accepts only
    /// `kSourceAddressAuthData` claims, signed by keys listed here.
    pub auth_data: Option<AddressAuthDataInfo>,
}

/// The signer set and weight threshold registered at an address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddressAuthDataInfo {
    /// Occupied key slots in ascending `keyId` order. Unoccupied slots are
    /// omitted, so `keyId` values need not be contiguous.
    pub signers: Vec<AuthSigner>,
    /// Minimum total signer weight required to authorize a transaction
    /// from this address.
    pub required_weight: u32,
}

/// One registered key slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthSigner {
    /// Slot index. A signature by this key names it as `publicKeyId`, and a
    /// source-address claim admits it by setting this bit in `allowedKeys`.
    pub key_id: u32,
    /// The registered public key.
    pub public_key: AuthPublicKey,
    /// Weight this key contributes toward `requiredWeight`.
    pub max_weight: u32,
}

/// A registered public key, tagged by its cryptographic scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "scheme", rename_all = "camelCase")]
#[non_exhaustive]
pub enum AuthPublicKey {
    /// An Ed25519 public key.
    Ed25519 {
        /// The 32-byte public key.
        key: Ed25519PublicKey,
    },
    /// A scheme unknown to this server build. The key is registered, but
    /// this server cannot describe it.
    Unknown {
        /// Raw protocol scheme id of the unrecognized key.
        #[serde(rename = "schemeId")]
        scheme_id: i32,
    },
}

impl From<&AddressAuthData> for AddressAuthDataInfo {
    fn from(auth_data: &AddressAuthData) -> Self {
        let signers = (0u32..)
            .zip(&auth_data.public_keys)
            .filter_map(|(key_id, slot)| {
                slot.as_ref().map(|config| AuthSigner {
                    key_id,
                    public_key: AuthPublicKey::from(&config.key),
                    max_weight: config.max_weight,
                })
            })
            .collect();
        Self {
            signers,
            required_weight: auth_data.required_weight,
        }
    }
}

impl From<&PublicKey> for AuthPublicKey {
    fn from(key: &PublicKey) -> Self {
        match key {
            PublicKey::Ed25519 { ed25519 } => Self::Ed25519 {
                key: Hex0x(*ed25519),
            },
            other => Self::Unknown {
                scheme_id: other.scheme().0,
            },
        }
    }
}
