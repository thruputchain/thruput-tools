//! Public hosted-network descriptor types.
//!
//! This module is the public counterpart to `runtime_manifest`. It intentionally
//! excludes private bootstrap seeds, bootstrap account derivation material, and
//! any sponsor/faucet private key material.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hex::HexBytes;
use crate::{ContractAddress, SHA256Hash};
use groundhog_xdr::contract::ContractEntrypointID;

/// Current public network descriptor schema version.
pub const NETWORK_DESCRIPTOR_SCHEMA_VERSION: u32 = 2;
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
    /// Public metadata required for faucet-backed wallet creation.
    pub wallet_create: WalletCreateDescriptor,
    /// Public dev-wallet entrypoints required to deploy and call contracts.
    pub dev_wallet: DevWalletDescriptor,
}

impl NetworkDescriptor {
    /// Construct a public network descriptor with the current schema version and kind.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network_id: String,
        protocol: String,
        genesis_hash: SHA256Hash,
        endpoints: NetworkDescriptorEndpoints,
        native_hog: NativeHogDescriptor,
        faucet: FaucetDescriptor,
        wallet_create: WalletCreateDescriptor,
        dev_wallet: DevWalletDescriptor,
    ) -> Self {
        Self {
            schema_version: NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            kind: NETWORK_DESCRIPTOR_KIND.to_owned(),
            network_id,
            protocol,
            genesis_hash,
            endpoints,
            native_hog,
            faucet,
            airdrop: None,
            wallet_create,
            dev_wallet,
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
    pub recipient_cooldown_seconds: u64,
    /// Number of requests allowed for each client identity in the quota window.
    pub client_quota_max: u32,
    /// Client quota window duration.
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

/// Public dev-wallet entrypoints used by hosted clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DevWalletDescriptor {
    /// Entrypoint used to deploy a contract through the wallet.
    pub deploy_entrypoint: HexBytes,
    /// Entrypoint used to invoke contract actions through the wallet.
    pub do_action_entrypoint: HexBytes,
}

impl DevWalletDescriptor {
    /// Construct public dev-wallet entrypoint metadata.
    pub fn new(deploy_entrypoint: HexBytes, do_action_entrypoint: HexBytes) -> Self {
        Self {
            deploy_entrypoint,
            do_action_entrypoint,
        }
    }

    /// Parse the advertised wallet deploy entrypoint into its XDR ID.
    pub fn deploy_entrypoint_id(
        &self,
    ) -> Result<ContractEntrypointID, NetworkDescriptorEntrypointError> {
        parse_entrypoint_id("devWallet.deployEntrypoint", &self.deploy_entrypoint)
    }

    /// Parse the advertised wallet doAction entrypoint into its XDR ID.
    pub fn do_action_entrypoint_id(
        &self,
    ) -> Result<ContractEntrypointID, NetworkDescriptorEntrypointError> {
        parse_entrypoint_id("devWallet.doActionEntrypoint", &self.do_action_entrypoint)
    }
}

fn parse_entrypoint_id(
    field: &'static str,
    value: &HexBytes,
) -> Result<ContractEntrypointID, NetworkDescriptorEntrypointError> {
    let len = value.0.len();
    let bytes: [u8; 8] = value
        .0
        .as_slice()
        .try_into()
        .map_err(|_| NetworkDescriptorEntrypointError::InvalidLength { field, len })?;
    Ok(ContractEntrypointID::from(bytes))
}

/// Errors returned while parsing entrypoint IDs in a public network descriptor.
#[derive(Debug, thiserror::Error)]
pub enum NetworkDescriptorEntrypointError {
    /// The descriptor field decoded to the wrong byte length.
    #[error("{field} must be 8 bytes, got {len}")]
    InvalidLength {
        /// JSON field path being parsed.
        field: &'static str,
        /// Decoded byte length.
        len: usize,
    },
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

impl WalletCreateDescriptor {
    /// Construct public wallet-create funding and derivation metadata.
    pub fn new(prefund_raw_amount: String, wallet_contract_hash: SHA256Hash) -> Self {
        Self {
            prefund_raw_amount,
            wallet_contract_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_wallet_entrypoints_parse_rotated_ids() {
        let descriptor = DevWalletDescriptor::new(
            HexBytes(vec![0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]),
            HexBytes(vec![0xa0, 0xff, 0xf9, 0x4c, 0x64, 0xfb, 0xaf, 0x3c]),
        );

        assert_eq!(
            descriptor.deploy_entrypoint_id().unwrap().as_ref(),
            &[0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]
        );
        assert_eq!(
            descriptor.do_action_entrypoint_id().unwrap().as_ref(),
            &[0xa0, 0xff, 0xf9, 0x4c, 0x64, 0xfb, 0xaf, 0x3c]
        );
    }

    #[test]
    fn dev_wallet_entrypoints_reject_wrong_lengths() {
        let descriptor = DevWalletDescriptor::new(HexBytes(vec![0xf9; 7]), HexBytes(vec![0xa0; 9]));

        assert!(
            descriptor
                .deploy_entrypoint_id()
                .unwrap_err()
                .to_string()
                .contains("got 7")
        );
        assert!(
            descriptor
                .do_action_entrypoint_id()
                .unwrap_err()
                .to_string()
                .contains("got 9")
        );
    }
}
