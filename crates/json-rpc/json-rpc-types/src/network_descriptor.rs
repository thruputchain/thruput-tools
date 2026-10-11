//! Public hosted-network descriptor types.
//!
//! This module is the public counterpart to `runtime_manifest`. It intentionally
//! excludes private bootstrap seeds, bootstrap account derivation material, and
//! any sponsor/faucet private key material.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::HexBytes;
use crate::{ContractAddress, NetworkHash, SHA256Hash};

/// Current public network descriptor schema version.
pub const NETWORK_DESCRIPTOR_SCHEMA_VERSION: u32 = 5;
/// Descriptor family for public hosted-network metadata.
pub const NETWORK_DESCRIPTOR_KIND: &str = "groundhog-network";

/// Public metadata served by hosted Devnet at `GET /v1/network`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NetworkDescriptor {
    /// Version of this JSON contract.
    pub schema_version: u32,
    /// Descriptor family, for example `groundhog-network`.
    pub kind: String,
    /// Stable identifier shared by the RPC endpoint, faucet, and CLI.
    pub network_id: String,
    /// Hash of the network name bound into transaction signatures.
    pub network_hash: NetworkHash,
    /// x23 protocol string expected by backend services.
    pub protocol: String,
    /// Hash of the materialized genesis state.
    pub genesis_hash: SHA256Hash,
    /// Public endpoints clients may call for this network.
    pub endpoints: NetworkDescriptorEndpoints,
    /// Native HOG token metadata required to build public token calls.
    pub native_hog: NativeHogDescriptor,
    /// Public faucet account metadata. No signing material is included.
    pub faucet: FaucetDescriptor,
    /// Static hosted-faucet airdrop policy. Live balance and usage are intentionally excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub airdrop: Option<AirdropDescriptor>,
}

impl NetworkDescriptor {
    /// Construct a public network descriptor with the current schema version and kind.
    pub fn new(
        network_id: String,
        network_hash: NetworkHash,
        protocol: String,
        genesis_hash: SHA256Hash,
        endpoints: NetworkDescriptorEndpoints,
        native_hog: NativeHogDescriptor,
        faucet: FaucetDescriptor,
    ) -> Self {
        Self {
            schema_version: NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            kind: NETWORK_DESCRIPTOR_KIND.to_owned(),
            network_id,
            network_hash,
            protocol,
            genesis_hash,
            endpoints,
            native_hog,
            faucet,
            airdrop: None,
        }
    }

    /// Add static hosted-faucet airdrop policy metadata.
    #[must_use]
    pub fn with_airdrop(mut self, airdrop: AirdropDescriptor) -> Self {
        self.airdrop = Some(airdrop);
        self
    }
}

/// Static hosted-faucet airdrop policy advertised to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AirdropDescriptor {
    /// Fixed raw HOG amount transferred per successful request.
    pub raw_amount: String,
    /// Cooldown applied to each recipient.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub recipient_cooldown_seconds: u64,
    /// Number of requests allowed for each client identity in the quota window.
    pub client_quota_max: u32,
    /// Client quota window duration.
    #[serde(with = "crate::decimal")]
    #[schemars(with = "crate::decimal::Decimal<u64>")]
    pub client_quota_window_seconds: u64,
    /// Maximum in-flight requests for one recipient.
    pub max_pending_per_recipient: u32,
}

impl AirdropDescriptor {
    /// Construct static hosted-faucet airdrop policy metadata.
    pub fn new(
        raw_amount: String,
        recipient_cooldown_seconds: u64,
        client_quota_max: u32,
        client_quota_window_seconds: u64,
        max_pending_per_recipient: u32,
    ) -> Self {
        Self {
            raw_amount,
            recipient_cooldown_seconds,
            client_quota_max,
            client_quota_window_seconds,
            max_pending_per_recipient,
        }
    }
}

/// Public endpoints for a hosted network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NetworkDescriptorEndpoints {
    /// JSON-RPC endpoint used for reads and user-submitted transactions.
    pub rpc_url: String,
}

impl NetworkDescriptorEndpoints {
    /// Construct public RPC endpoint metadata.
    pub fn new(rpc_url: String) -> Self {
        Self { rpc_url }
    }
}

/// Native HOG token metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NativeHogDescriptor {
    /// Native HOG token contract address.
    pub token_address: ContractAddress,
    /// Display decimals for native HOG amounts.
    pub decimals: u32,
    /// Standard ERC20-compatible transfer entrypoint.
    pub transfer_entrypoint: HexBytes,
}

impl NativeHogDescriptor {
    /// Construct native HOG token metadata.
    pub fn new(
        token_address: ContractAddress,
        decimals: u32,
        transfer_entrypoint: HexBytes,
    ) -> Self {
        Self {
            token_address,
            decimals,
            transfer_entrypoint,
        }
    }
}

/// Public faucet account metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct FaucetDescriptor {
    /// Faucet wallet address used as the public sponsor account.
    pub wallet_address: ContractAddress,
}

impl FaucetDescriptor {
    /// Construct public faucet account metadata.
    pub fn new(wallet_address: ContractAddress) -> Self {
        Self { wallet_address }
    }
}
