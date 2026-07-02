use crate::airdrop::{AirdropRuntimeConfig, AirdropRuntimeConfigError};
use crate::wallet_create::{WalletCreateRuntimeConfig, WalletCreateRuntimeConfigError};
use axum::http::HeaderValue;
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::network_descriptor::{
    FaucetDescriptor, NativeHogDescriptor, NetworkDescriptor, NetworkDescriptorEndpoints,
    WalletCreateDescriptor,
};
use groundhog_json_rpc_types::{ContractAddress, SHA256Hash};
use serde::Deserialize;
use std::path::PathBuf;

/// Faucet service TOML config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaucetConfigFile {
    /// HTTP listen address. Defaults to the service default when omitted.
    #[serde(default)]
    pub listen_address: Option<String>,
    /// Browser origins allowed to call the API from static UI deployments.
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
    /// Public network descriptor source fields.
    pub network: NetworkConfig,
    /// Private faucet airdrop execution config.
    pub airdrop: AirdropConfig,
    /// Private wallet-create execution config.
    pub wallet_create: WalletCreateExecutionConfig,
}

/// Validated faucet service config split into public and private runtime parts.
#[derive(Debug)]
pub struct FaucetConfigParts {
    /// HTTP listen address override from config.
    pub listen_address: Option<String>,
    /// Public descriptor served by `/v1/network`.
    pub network_descriptor: NetworkDescriptor,
    /// Private airdrop execution config.
    pub airdrop: AirdropRuntimeConfig,
    /// Private wallet-create execution config.
    pub wallet_create: WalletCreateRuntimeConfig,
    /// Browser origins allowed to call the API.
    pub cors_allowed_origins: Vec<HeaderValue>,
}

impl FaucetConfigFile {
    /// Convert this config into the public network descriptor served by `/v1/network`.
    pub fn into_network_descriptor(self) -> NetworkDescriptor {
        self.network.into_descriptor()
    }

    /// Split this config into the optional listen override and public descriptor.
    pub fn into_parts(self) -> Result<FaucetConfigParts, FaucetConfigError> {
        let airdrop = self.airdrop.into_runtime_config()?;
        let wallet_create = self.wallet_create.into_runtime_config()?;
        let cors_allowed_origins = self
            .cors_allowed_origins
            .iter()
            .map(|origin| {
                HeaderValue::from_str(origin).map_err(|error| {
                    FaucetConfigError::InvalidCorsOrigin {
                        origin: origin.clone(),
                        message: error.to_string(),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FaucetConfigParts {
            listen_address: self.listen_address,
            network_descriptor: self.network.into_descriptor(),
            airdrop,
            wallet_create,
            cors_allowed_origins,
        })
    }
}

/// Errors returned while validating faucet service config.
#[derive(Debug, thiserror::Error)]
pub enum FaucetConfigError {
    /// The private airdrop config was invalid.
    #[error(transparent)]
    Airdrop(#[from] AirdropRuntimeConfigError),
    /// The private wallet-create config was invalid.
    #[error(transparent)]
    WalletCreate(#[from] WalletCreateRuntimeConfigError),
    /// A configured CORS origin was invalid.
    #[error("invalid CORS origin `{origin}`: {message}")]
    InvalidCorsOrigin { origin: String, message: String },
}

/// Public network metadata fields used to build a descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// Stable network identifier.
    pub network_id: String,
    /// x23 protocol string expected by backend services.
    pub protocol: String,
    /// Hash of the materialized genesis state.
    pub genesis_hash: SHA256Hash,
    /// Public endpoint configuration.
    pub endpoints: EndpointConfig,
    /// Native HOG token configuration.
    pub native_hog: NativeHogConfig,
    /// Public faucet account configuration.
    pub faucet: FaucetAccountConfig,
    /// Wallet-create metadata configuration.
    pub wallet_create: WalletCreateConfig,
}

impl NetworkConfig {
    fn into_descriptor(self) -> NetworkDescriptor {
        NetworkDescriptor::new(
            self.network_id,
            self.protocol,
            self.genesis_hash,
            NetworkDescriptorEndpoints::new(self.endpoints.rpc_url, self.endpoints.faucet_url),
            NativeHogDescriptor::new(
                self.native_hog.token_address,
                self.native_hog.decimals,
                self.native_hog.transfer_entrypoint,
            ),
            FaucetDescriptor::new(self.faucet.wallet_address),
            WalletCreateDescriptor::new(
                self.wallet_create.prefund_raw_amount,
                self.wallet_create.wallet_contract_hash,
            ),
        )
    }
}

/// Public HTTP endpoint configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// JSON-RPC endpoint URL.
    pub rpc_url: String,
    /// Faucet service base URL.
    pub faucet_url: String,
}

/// Native HOG token configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHogConfig {
    /// Native HOG token contract address.
    pub token_address: ContractAddress,
    /// Display decimals for native HOG amounts.
    pub decimals: u32,
    /// Standard ERC20-compatible transfer entrypoint.
    pub transfer_entrypoint: HexBytes,
}

/// Public faucet account configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaucetAccountConfig {
    /// Faucet wallet address used as the public sponsor account.
    pub wallet_address: ContractAddress,
}

/// Wallet-create metadata configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletCreateConfig {
    /// Raw HOG units prefunded to the deterministic future wallet address.
    pub prefund_raw_amount: String,
    /// Contract hash used for deterministic wallet address derivation.
    pub wallet_contract_hash: SHA256Hash,
}

/// Private airdrop execution configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirdropConfig {
    /// Raw HOG units transferred for each airdrop request.
    pub raw_amount: String,
    /// Faucet wallet `doAction` entrypoint.
    pub wallet_do_action_entrypoint: HexBytes,
    /// 32-byte faucet signing secret key.
    pub faucet_secret_key: HexBytes,
}

impl AirdropConfig {
    /// Convert wire config into validated runtime airdrop config.
    pub fn into_runtime_config(self) -> Result<AirdropRuntimeConfig, AirdropRuntimeConfigError> {
        AirdropRuntimeConfig::from_wire(
            &self.raw_amount,
            self.wallet_do_action_entrypoint,
            self.faucet_secret_key,
        )
    }
}

/// Private wallet-create execution configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalletCreateExecutionConfig {
    /// Faucet wallet `create_wallet` entrypoint.
    pub create_wallet_entrypoint: HexBytes,
    /// Dev-wallet artifact path included in wallet-create transactions.
    pub wallet_artifact_path: PathBuf,
}

impl WalletCreateExecutionConfig {
    /// Convert wire config into validated runtime wallet-create config.
    pub fn into_runtime_config(
        self,
    ) -> Result<WalletCreateRuntimeConfig, WalletCreateRuntimeConfigError> {
        WalletCreateRuntimeConfig::from_wire(
            self.create_wallet_entrypoint,
            self.wallet_artifact_path,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::FaucetConfigFile;

    #[test]
    fn config_builds_public_network_descriptor() {
        let config: FaucetConfigFile =
            toml::from_str(TEST_CONFIG).expect("bug: fixture config should parse");

        assert_eq!(config.listen_address.as_deref(), Some("127.0.0.1:8081"));

        let descriptor = config.into_network_descriptor();

        assert_eq!(descriptor.schema_version, 1);
        assert_eq!(descriptor.kind, "groundhog-network");
        assert_eq!(descriptor.network_id, "devnet-preview");
        assert_eq!(descriptor.endpoints.rpc_url, "https://rpc.devnet.example");
        assert_eq!(descriptor.native_hog.decimals, 7);
        assert_eq!(descriptor.wallet_create.prefund_raw_amount, "100000000");
    }

    #[test]
    fn config_builds_airdrop_runtime_config() {
        let config: FaucetConfigFile =
            toml::from_str(TEST_CONFIG).expect("bug: fixture config should parse");

        let parts = config
            .into_parts()
            .expect("bug: airdrop runtime config should validate");

        assert_eq!(parts.airdrop.raw_amount, 100_000_000);
    }

    #[test]
    fn config_builds_wallet_create_runtime_config() {
        let config: FaucetConfigFile =
            toml::from_str(TEST_CONFIG).expect("bug: fixture config should parse");

        let parts = config
            .into_parts()
            .expect("bug: wallet-create runtime config should validate");

        assert_eq!(
            parts.wallet_create.wallet_artifact_path,
            std::path::PathBuf::from("contracts/dev_wallet.rv32a")
        );
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let err = toml::from_str::<FaucetConfigFile>(
            r#"
            unexpected = true

            [network]
            network_id = "devnet-preview"
            protocol = "x23"
            genesis_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

            [network.endpoints]
            rpc_url = "https://rpc.devnet.example"
            faucet_url = "https://faucet.devnet.example"

            [network.native_hog]
            token_address = "0x0000000000000000000000000000000000000000000000000000000000000001"
            decimals = 7
            transfer_entrypoint = "0x63285b3523f9dc3f"

            [network.faucet]
            wallet_address = "0x0000000000000000000000000000000000000000000000000000000000000000"

            [network.wallet_create]
            prefund_raw_amount = "100000000"
            wallet_contract_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"
            "#,
        )
        .expect_err("bug: unknown fields should fail");

        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn config_builds_cors_origins() {
        let config: FaucetConfigFile =
            toml::from_str(TEST_CONFIG).expect("bug: fixture config should parse");

        let parts = config
            .into_parts()
            .expect("bug: CORS origins should validate");

        assert_eq!(parts.cors_allowed_origins.len(), 1);
        assert_eq!(parts.cors_allowed_origins[0], "http://127.0.0.1:5173");
    }

    const TEST_CONFIG: &str = r#"
        listen_address = "127.0.0.1:8081"
        cors_allowed_origins = ["http://127.0.0.1:5173"]

        [network]
        network_id = "devnet-preview"
        protocol = "x23"
        genesis_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

        [network.endpoints]
        rpc_url = "https://rpc.devnet.example"
        faucet_url = "https://faucet.devnet.example"

        [network.native_hog]
        token_address = "0x0000000000000000000000000000000000000000000000000000000000000001"
        decimals = 7
        transfer_entrypoint = "0x63285b3523f9dc3f"

        [network.faucet]
        wallet_address = "0x0000000000000000000000000000000000000000000000000000000000000000"

        [network.wallet_create]
        prefund_raw_amount = "100000000"
        wallet_contract_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

        [airdrop]
        raw_amount = "100000000"
        wallet_do_action_entrypoint = "0x646f416374696f6e"
        faucet_secret_key = "0x1111111111111111111111111111111111111111111111111111111111111111"

        [wallet_create]
        create_wallet_entrypoint = "0x6372656174655761"
        wallet_artifact_path = "contracts/dev_wallet.rv32a"
    "#;
}
