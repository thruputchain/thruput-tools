use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::HexBytes;
use crate::{BlockHeight, ContractAddress};

/// Authorization assumed during sandboxed execution. Signatures are not checked;
/// contracts still inspect and validate this claim through their normal code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ViewAuthClaim {
    SourceAddress {
        /// Bitmap of source-address authorization keys allowed by this claim.
        allowed_keys: u32,
        /// Required source-address authorization weight assumed by this claim.
        required_weight: u32,
    },
    PubkeySignature {
        /// Hash naming the account's public key, scheme included:
        /// `sha256(cxs(DomainSeparator("PublicKey")) || cxs(PublicKey))`.
        pubkey_hash: crate::PublicKeyHash,
        #[serde(with = "crate::decimal")]
        #[schemars(with = "crate::decimal::Decimal<u64>")]
        nonce: u64,
    },
}

impl From<ViewAuthClaim> for thruput_xdr::auth_claim::AuthClaim {
    fn from(claim: ViewAuthClaim) -> Self {
        use thruput_xdr::auth_claim::{AuthClaim_source_address_claim, PubkeyAccountPreimage};
        match claim {
            ViewAuthClaim::SourceAddress {
                allowed_keys,
                required_weight,
            } => Self::kSourceAddressAuthData {
                source_address_claim: AuthClaim_source_address_claim {
                    allowed_keys,
                    required_weight,
                },
            },
            ViewAuthClaim::PubkeySignature { pubkey_hash, nonce } => Self::kPubkeySignature {
                pubkey_preimage: PubkeyAccountPreimage {
                    pubkey_hash: pubkey_hash.0,
                    nonce,
                },
            },
        }
    }
}

/// Independent simulations against one committed snapshot, in request order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewBatchRequest {
    /// Nonempty calls bounded by the endpoint's advertised batch limits.
    pub calls: Vec<ViewCall>,
    /// Freshness floor on the selected snapshot. Does not wait or request a refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::decimal::option")]
    #[schemars(with = "Option<crate::decimal::Decimal<BlockHeight>>")]
    pub min_height: Option<BlockHeight>,
}

/// Requested minimum height and the committed height of the selected snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewHeightUnavailable {
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub requested: BlockHeight,
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<BlockHeight>")]
    pub observed: BlockHeight,
}

/// Request for sandboxed contract simulation against the latest committed state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewRequest {
    /// Optional assumed authorization; omission preserves the unauthenticated default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_claim: Option<ViewAuthClaim>,
    /// Contract address to execute.
    pub contract: ContractAddress,

    /// Contract entrypoint id. Must be exactly 8 bytes.
    pub entrypoint: HexBytes,

    /// Opaque call input bytes.
    pub calldata: HexBytes,

    /// Optional simulated caller address observed by `get_msg_sender`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<ContractAddress>,

    /// Optional gas budget for the root invocation. Absent selects the
    /// server's `maxSimulationGas` (see `getServerLimits`); a value above it, or zero,
    /// is rejected as an invalid parameter. No gas is charged for a view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::decimal::option")]
    #[schemars(with = "Option<crate::decimal::Decimal<u64>>")]
    pub gas_limit: Option<u64>,
    /// Minimum committed snapshot height. Does not wait or force a refresh.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::option"
    )]
    #[schemars(with = "Option<crate::decimal::Decimal<BlockHeight>>")]
    pub min_height: Option<BlockHeight>,
}

/// One batch entry. Freshness belongs to the group, never to an individual call.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewCall {
    /// Optional assumed authorization for this independent call only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_claim: Option<ViewAuthClaim>,
    pub contract: ContractAddress,
    /// Contract entrypoint id. Must be exactly 8 bytes.
    pub entrypoint: HexBytes,
    pub calldata: HexBytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<ContractAddress>,
    /// Optional per-call gas budget. The endpoint's per-call and aggregate limits apply.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::option"
    )]
    #[schemars(with = "Option<crate::decimal::Decimal<u64>>")]
    pub gas_limit: Option<u64>,
}

impl From<ViewCall> for ViewRequest {
    fn from(call: ViewCall) -> Self {
        let ViewCall {
            auth_claim,
            contract,
            entrypoint,
            calldata,
            caller,
            gas_limit,
        } = call;
        Self {
            auth_claim,
            contract,
            entrypoint,
            calldata,
            caller,
            gas_limit,
            min_height: None,
        }
    }
}
