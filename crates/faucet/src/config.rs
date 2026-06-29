use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::network_descriptor::{
    FaucetDescriptor, NativeHogDescriptor, NetworkDescriptor, NetworkDescriptorEndpoints,
    WalletCreateDescriptor,
};
use groundhog_json_rpc_types::{ContractAddress, SHA256Hash};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Faucet service TOML config.
pub struct FaucetConfigFile {
    /// HTTP listen address. Defaults to the service default when omitted.
    #[serde(default)]
    pub listen_address: Option<String>,
    /// Public network descriptor source fields.
    pub network: NetworkConfig,
}

impl FaucetConfigFile {
    /// Convert this config into the public network descriptor served by `/v1/network`.
    pub fn into_network_descriptor(self) -> NetworkDescriptor {
        self.into_parts().1
    }

    /// Split this config into the optional listen override and public descriptor.
    pub fn into_parts(self) -> (Option<String>, NetworkDescriptor) {
        (self.listen_address, self.network.into_descriptor())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Public network metadata fields used to build a descriptor.
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Public HTTP endpoint configuration.
pub struct EndpointConfig {
    /// JSON-RPC endpoint URL.
    pub rpc_url: String,
    /// Faucet service base URL.
    pub faucet_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Native HOG token configuration.
pub struct NativeHogConfig {
    /// Native HOG token contract address.
    pub token_address: ContractAddress,
    /// Display decimals for native HOG amounts.
    pub decimals: u32,
    /// Standard ERC20-compatible transfer entrypoint.
    pub transfer_entrypoint: HexBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Public faucet account configuration.
pub struct FaucetAccountConfig {
    /// Faucet wallet address used as the public sponsor account.
    pub wallet_address: ContractAddress,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
/// Wallet-create metadata configuration.
pub struct WalletCreateConfig {
    /// Raw HOG units prefunded to the deterministic future wallet address.
    pub prefund_raw_amount: String,
    /// Contract hash used for deterministic wallet address derivation.
    pub wallet_contract_hash: SHA256Hash,
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

    const TEST_CONFIG: &str = r#"
        listen_address = "127.0.0.1:8081"

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
    "#;
}
