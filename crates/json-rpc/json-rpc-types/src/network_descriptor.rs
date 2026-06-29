//! Public hosted-network descriptor types.
//!
//! This module is the public counterpart to `runtime_manifest`. It intentionally
//! excludes private bootstrap seeds, bootstrap account derivation material, and
//! any sponsor/faucet private key material.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::HexBytes;
use crate::{ContractAddress, SHA256Hash};

/// Current public network descriptor schema version.
pub const NETWORK_DESCRIPTOR_SCHEMA_VERSION: u32 = 1;

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
    /// Public metadata required for faucet-backed wallet creation.
    pub wallet_create: WalletCreateDescriptor,
}

/// Public endpoints for a hosted network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NetworkDescriptorEndpoints {
    /// JSON-RPC endpoint used for reads and user-submitted transactions.
    pub rpc_url: String,
    /// Faucet service base URL used for airdrop and wallet-create requests.
    pub faucet_url: String,
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

/// Public faucet account metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct FaucetDescriptor {
    /// Faucet wallet address used as the public sponsor account.
    pub wallet_address: ContractAddress,
}

/// Public wallet-create metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct WalletCreateDescriptor {
    /// Raw HOG units prefunded to the deterministic future wallet address.
    pub prefund_raw_amount: String,
    /// Contract hash used with faucet wallet address and deploy nonce to derive
    /// the future wallet address.
    pub wallet_contract_hash: SHA256Hash,
}
