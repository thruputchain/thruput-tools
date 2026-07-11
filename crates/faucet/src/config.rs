use crate::airdrop::{AirdropRuntimeConfig, AirdropRuntimeConfigError};
use crate::key_source::{
    DefaultFaucetSecretKeyLoader, FaucetSecretKeyLoadError, FaucetSecretKeyLoader,
    FaucetSecretKeySource,
};
use crate::wallet_create::{WalletCreateRuntimeConfig, WalletCreateRuntimeConfigError};
use axum::http::HeaderValue;
use groundhog_json_rpc_types::hex::HexBytes;
use groundhog_json_rpc_types::network_descriptor::{
    FaucetDescriptor, NativeHogDescriptor, NetworkDescriptor, NetworkDescriptorEndpoints,
    WalletCreateDescriptor,
};
use groundhog_json_rpc_types::{ContractAddress, SHA256Hash};
use groundhog_signing::LocalSigner;
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
    /// Private-preview policy controls.
    #[serde(default)]
    pub policy: PolicyConfig,
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
    /// Private-preview policy controls.
    pub policy: PolicyRuntimeConfig,
}

impl FaucetConfigFile {
    /// Convert this config into the public network descriptor served by `/v1/network`.
    pub fn into_network_descriptor(self) -> NetworkDescriptor {
        self.network.into_descriptor()
    }

    /// Split this config into the optional listen override and public descriptor.
    pub async fn into_parts(self) -> Result<FaucetConfigParts, FaucetConfigError> {
        self.into_parts_with_key_loader(&DefaultFaucetSecretKeyLoader)
            .await
    }

    /// Validate config using the supplied signing-key loader.
    pub async fn into_parts_with_key_loader(
        self,
        key_loader: &dyn FaucetSecretKeyLoader,
    ) -> Result<FaucetConfigParts, FaucetConfigError> {
        if self.network.wallet_create.prefund_raw_amount != self.airdrop.raw_amount {
            return Err(FaucetConfigError::WalletCreatePrefundMismatch {
                wallet_create_prefund_raw_amount: self.network.wallet_create.prefund_raw_amount,
                airdrop_raw_amount: self.airdrop.raw_amount,
            });
        }
        validate_faucet_public_key_shape(&self.network.faucet.public_key)?;
        let faucet_secret_key = key_loader.load(&self.airdrop.secret_key_source).await?;
        validate_faucet_public_key_match(&self.network.faucet.public_key, &faucet_secret_key)?;
        let airdrop = self.airdrop.into_runtime_config(faucet_secret_key)?;
        let wallet_create = self.wallet_create.into_runtime_config()?;
        if self.network.wallet_create.prefund_raw_amount != airdrop.raw_amount.to_string() {
            return Err(FaucetConfigError::WalletCreatePrefundMismatch {
                wallet_create_prefund_raw_amount: self.network.wallet_create.prefund_raw_amount,
                airdrop_raw_amount: airdrop.raw_amount.to_string(),
            });
        }
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
            policy: self.policy.into_runtime_config(),
            cors_allowed_origins,
        })
    }
}

/// Private-preview faucet policy config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    /// Trust reverse proxy client IP headers for per-client quota identity.
    pub trust_proxy_client_ip_headers: bool,
    /// Airdrop policy controls.
    pub airdrop: AirdropPolicyConfig,
    /// Wallet-create policy controls.
    pub wallet_create: WalletCreatePolicyConfig,
}

impl PolicyConfig {
    fn into_runtime_config(self) -> PolicyRuntimeConfig {
        PolicyRuntimeConfig {
            trust_proxy_client_ip_headers: self.trust_proxy_client_ip_headers,
            airdrop: AirdropPolicyRuntimeConfig {
                recipient_cooldown_secs: self.airdrop.recipient_cooldown_secs,
                ip_quota_window_secs: self.airdrop.ip_quota_window_secs,
                ip_quota_max: self.airdrop.ip_quota_max,
                max_pending_per_recipient: self.airdrop.max_pending_per_recipient,
            },
            wallet_create: WalletCreatePolicyRuntimeConfig {
                owner_quota_window_secs: self.wallet_create.owner_quota_window_secs,
                owner_quota_max: self.wallet_create.owner_quota_max,
                ip_quota_window_secs: self.wallet_create.ip_quota_window_secs,
                ip_quota_max: self.wallet_create.ip_quota_max,
                max_pending_per_owner: self.wallet_create.max_pending_per_owner,
                unprefunded_attempt_ttl_secs: self.wallet_create.unprefunded_attempt_ttl_secs,
                prefunded_attempt_retention_secs: self
                    .wallet_create
                    .prefunded_attempt_retention_secs,
            },
        }
    }
}

impl Default for PolicyRuntimeConfig {
    fn default() -> Self {
        PolicyConfig::default().into_runtime_config()
    }
}

/// Airdrop policy config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AirdropPolicyConfig {
    pub recipient_cooldown_secs: u64,
    pub ip_quota_window_secs: u64,
    pub ip_quota_max: u32,
    pub max_pending_per_recipient: u32,
}

impl Default for AirdropPolicyConfig {
    fn default() -> Self {
        Self {
            recipient_cooldown_secs: 12 * 60 * 60,
            ip_quota_window_secs: 60 * 60,
            ip_quota_max: 10,
            max_pending_per_recipient: 1,
        }
    }
}

/// Wallet-create policy config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletCreatePolicyConfig {
    pub owner_quota_window_secs: u64,
    pub owner_quota_max: u32,
    pub ip_quota_window_secs: u64,
    pub ip_quota_max: u32,
    pub max_pending_per_owner: u32,
    pub unprefunded_attempt_ttl_secs: u64,
    pub prefunded_attempt_retention_secs: u64,
}

impl Default for WalletCreatePolicyConfig {
    fn default() -> Self {
        Self {
            owner_quota_window_secs: 24 * 60 * 60,
            owner_quota_max: 3,
            ip_quota_window_secs: 24 * 60 * 60,
            ip_quota_max: 20,
            max_pending_per_owner: 1,
            unprefunded_attempt_ttl_secs: 15 * 60,
            prefunded_attempt_retention_secs: 24 * 60 * 60,
        }
    }
}

/// Runtime policy controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRuntimeConfig {
    pub trust_proxy_client_ip_headers: bool,
    pub airdrop: AirdropPolicyRuntimeConfig,
    pub wallet_create: WalletCreatePolicyRuntimeConfig,
}

/// Runtime airdrop policy controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirdropPolicyRuntimeConfig {
    pub recipient_cooldown_secs: u64,
    pub ip_quota_window_secs: u64,
    pub ip_quota_max: u32,
    pub max_pending_per_recipient: u32,
}

/// Runtime wallet-create policy controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletCreatePolicyRuntimeConfig {
    pub owner_quota_window_secs: u64,
    pub owner_quota_max: u32,
    pub ip_quota_window_secs: u64,
    pub ip_quota_max: u32,
    pub max_pending_per_owner: u32,
    pub unprefunded_attempt_ttl_secs: u64,
    pub prefunded_attempt_retention_secs: u64,
}

/// Errors returned while validating faucet service config.
#[derive(Debug, thiserror::Error)]
pub enum FaucetConfigError {
    /// The configured faucet signing key could not be loaded safely.
    #[error(transparent)]
    SecretKey(#[from] FaucetSecretKeyLoadError),
    /// The private airdrop config was invalid.
    #[error(transparent)]
    Airdrop(#[from] AirdropRuntimeConfigError),
    /// The private wallet-create config was invalid.
    #[error(transparent)]
    WalletCreate(#[from] WalletCreateRuntimeConfigError),
    /// A configured CORS origin was invalid.
    #[error("invalid CORS origin `{origin}`: {message}")]
    InvalidCorsOrigin { origin: String, message: String },
    /// Wallet-create prefunding currently uses the generic airdrop endpoint.
    #[error(
        "wallet-create prefund_raw_amount `{wallet_create_prefund_raw_amount}` must match airdrop raw_amount `{airdrop_raw_amount}`"
    )]
    WalletCreatePrefundMismatch {
        wallet_create_prefund_raw_amount: String,
        airdrop_raw_amount: String,
    },
    /// The expected faucet public key was not 32 bytes.
    #[error("network faucet public key must be 32 bytes, got {0}")]
    InvalidFaucetPublicKey(usize),
    /// The loaded secret key did not control the configured faucet account.
    #[error("loaded faucet signing key does not match network.faucet.public_key")]
    FaucetPublicKeyMismatch,
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
            NetworkDescriptorEndpoints::new(self.endpoints.rpc_url),
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
    /// Backend JSON-RPC endpoint URL used by the faucet service.
    pub rpc_url: String,
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
    /// Ed25519 public key expected for the loaded faucet signing secret.
    pub public_key: HexBytes,
}

fn validate_faucet_public_key_shape(
    expected_public_key: &HexBytes,
) -> Result<(), FaucetConfigError> {
    if expected_public_key.0.len() == 32 {
        return Ok(());
    }
    Err(FaucetConfigError::InvalidFaucetPublicKey(
        expected_public_key.0.len(),
    ))
}

fn validate_faucet_public_key_match(
    expected_public_key: &HexBytes,
    faucet_secret_key: &[u8; 32],
) -> Result<(), FaucetConfigError> {
    let actual_public_key = LocalSigner::from_secret_key(*faucet_secret_key).verifying_key_bytes();
    if expected_public_key.0.as_slice() != actual_public_key {
        return Err(FaucetConfigError::FaucetPublicKeyMismatch);
    }
    Ok(())
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
    /// Source used to load the faucet signing key at startup.
    pub secret_key_source: FaucetSecretKeySource,
}

impl AirdropConfig {
    /// Convert wire config into validated runtime airdrop config.
    pub fn into_runtime_config(
        self,
        faucet_secret_key: zeroize::Zeroizing<[u8; 32]>,
    ) -> Result<AirdropRuntimeConfig, AirdropRuntimeConfigError> {
        AirdropRuntimeConfig::from_wire(
            &self.raw_amount,
            self.wallet_do_action_entrypoint,
            faucet_secret_key,
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
    use async_trait::async_trait;
    use groundhog_json_rpc_types::hex::HexBytes;
    use groundhog_signing::LocalSigner;
    use zeroize::Zeroizing;

    use crate::key_source::{
        FaucetSecretKeyLoadError, FaucetSecretKeyLoader, FaucetSecretKeySource,
    };

    use super::{AirdropConfig, FaucetConfigFile};

    struct TestKeyLoader;

    struct FailIfCalledKeyLoader;

    #[async_trait]
    impl FaucetSecretKeyLoader for TestKeyLoader {
        async fn load(
            &self,
            _source: &FaucetSecretKeySource,
        ) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
            Ok(Zeroizing::new([0x11; 32]))
        }
    }

    #[async_trait]
    impl FaucetSecretKeyLoader for FailIfCalledKeyLoader {
        async fn load(
            &self,
            _source: &FaucetSecretKeySource,
        ) -> Result<Zeroizing<[u8; 32]>, FaucetSecretKeyLoadError> {
            panic!("bug: key loader must not run for invalid non-secret config")
        }
    }

    #[test]
    fn airdrop_config_contains_only_key_source() {
        let config = AirdropConfig {
            raw_amount: "100000000".into(),
            wallet_do_action_entrypoint: HexBytes(vec![0x64; 8]),
            secret_key_source: FaucetSecretKeySource::Environment {
                variable: "GROUNDHOG_FAUCET_SECRET_KEY".into(),
            },
        };

        let debug = format!("{config:?}");

        assert!(debug.contains("GROUNDHOG_FAUCET_SECRET_KEY"), "{debug}");
        assert!(!debug.contains(&"ab".repeat(32)), "{debug}");
    }

    #[test]
    fn config_builds_public_network_descriptor() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        assert_eq!(config.listen_address.as_deref(), Some("127.0.0.1:8081"));

        let descriptor = config.into_network_descriptor();

        assert_eq!(descriptor.schema_version, 1);
        assert_eq!(descriptor.kind, "groundhog-network");
        assert_eq!(descriptor.network_id, "devnet-preview");
        assert_eq!(descriptor.endpoints.rpc_url, "https://rpc.devnet.example");
        assert_eq!(descriptor.native_hog.decimals, 7);
        assert_eq!(descriptor.wallet_create.prefund_raw_amount, "100000000");
    }

    #[tokio::test]
    async fn config_builds_airdrop_runtime_config() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: airdrop runtime config should validate");

        assert_eq!(parts.airdrop.raw_amount, 100_000_000);
    }

    #[tokio::test]
    async fn config_builds_wallet_create_runtime_config() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: wallet-create runtime config should validate");

        assert_eq!(
            parts.wallet_create.wallet_artifact_path,
            std::path::PathBuf::from("contracts/dev_wallet.rv32a")
        );
    }

    #[tokio::test]
    async fn config_builds_default_policy_config() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: policy runtime config should validate");

        assert!(!parts.policy.trust_proxy_client_ip_headers);
        assert_eq!(parts.policy.airdrop.recipient_cooldown_secs, 12 * 60 * 60);
        assert_eq!(parts.policy.airdrop.ip_quota_max, 10);
        assert_eq!(parts.policy.wallet_create.owner_quota_max, 3);
        assert_eq!(parts.policy.wallet_create.ip_quota_max, 20);
    }

    #[tokio::test]
    async fn config_overrides_policy_config() {
        let config: FaucetConfigFile = toml::from_str(&format!(
            "{}\n{}",
            test_config(),
            r"
            [policy]
            trust_proxy_client_ip_headers = true

            [policy.airdrop]
            recipient_cooldown_secs = 60
            ip_quota_window_secs = 30
            ip_quota_max = 2
            max_pending_per_recipient = 1

            [policy.wallet_create]
            owner_quota_window_secs = 120
            owner_quota_max = 4
            ip_quota_window_secs = 180
            ip_quota_max = 5
            max_pending_per_owner = 1
            unprefunded_attempt_ttl_secs = 20
            prefunded_attempt_retention_secs = 240
            "
        ))
        .expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: policy runtime config should validate");

        assert!(parts.policy.trust_proxy_client_ip_headers);
        assert_eq!(parts.policy.airdrop.recipient_cooldown_secs, 60);
        assert_eq!(
            parts.policy.wallet_create.prefunded_attempt_retention_secs,
            240
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

    #[tokio::test]
    async fn config_builds_cors_origins() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: CORS origins should validate");

        assert_eq!(parts.cors_allowed_origins.len(), 1);
        assert_eq!(parts.cors_allowed_origins[0], "http://127.0.0.1:5173");
    }

    #[tokio::test]
    async fn config_rejects_wallet_create_prefund_airdrop_mismatch() {
        let config: FaucetConfigFile = toml::from_str(&test_config().replace(
            "prefund_raw_amount = \"100000000\"",
            "prefund_raw_amount = \"1\"",
        ))
        .expect("bug: fixture config should parse");

        let err = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect_err("bug: mismatched prefund and airdrop amounts should fail");

        assert!(err.to_string().contains("must match airdrop raw_amount"));
    }

    #[tokio::test]
    async fn config_rejects_secret_that_does_not_match_faucet_public_key() {
        let expected_public_key =
            hex::encode(LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes());
        let config: FaucetConfigFile = toml::from_str(&test_config().replace(
            &format!("public_key = \"0x{expected_public_key}\""),
            &format!("public_key = \"0x{}\"", "aa".repeat(32)),
        ))
        .expect("bug: fixture config should parse");

        let error = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect_err("bug: mismatched faucet key should fail");

        assert!(error.to_string().contains("does not match"), "{error}");
        assert!(!error.to_string().contains(&expected_public_key));
    }

    #[tokio::test]
    async fn config_rejects_malformed_public_key_before_loading_secret() {
        let expected_public_key =
            hex::encode(LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes());
        let config: FaucetConfigFile = toml::from_str(&test_config().replace(
            &format!("public_key = \"0x{expected_public_key}\""),
            "public_key = \"0xaa\"",
        ))
        .expect("bug: fixture config should parse");

        let error = config
            .into_parts_with_key_loader(&FailIfCalledKeyLoader)
            .await
            .expect_err("bug: malformed public key should fail");

        assert!(error.to_string().contains("32 bytes"), "{error}");
    }

    #[test]
    fn config_parses_aws_secrets_manager_key_source() {
        let config: FaucetConfigFile = toml::from_str(&test_config().replace(
            "type = \"environment\"\n        variable = \"GROUNDHOG_FAUCET_SECRET_KEY\"",
            "type = \"aws_secrets_manager\"\n        secret_id = \"groundhog/devnet/faucet\"\n        region = \"us-west-2\"",
        ))
        .expect("bug: AWS key-source fixture should parse");

        assert!(matches!(
            config.airdrop.secret_key_source,
            FaucetSecretKeySource::AwsSecretsManager { ref secret_id, ref region }
                if secret_id == "groundhog/devnet/faucet" && region == "us-west-2"
        ));
    }

    fn test_config() -> String {
        let public_key =
            hex::encode(LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes());
        TEST_CONFIG.replace("__FAUCET_PUBLIC_KEY__", &public_key)
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

        [network.native_hog]
        token_address = "0x0000000000000000000000000000000000000000000000000000000000000001"
        decimals = 7
        transfer_entrypoint = "0x63285b3523f9dc3f"

        [network.faucet]
        wallet_address = "0x0000000000000000000000000000000000000000000000000000000000000000"
        public_key = "0x__FAUCET_PUBLIC_KEY__"

        [network.wallet_create]
        prefund_raw_amount = "100000000"
        wallet_contract_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

        [airdrop]
        raw_amount = "100000000"
        wallet_do_action_entrypoint = "0x646f416374696f6e"

        [airdrop.secret_key_source]
        type = "environment"
        variable = "GROUNDHOG_FAUCET_SECRET_KEY"

        [wallet_create]
        create_wallet_entrypoint = "0x6372656174655761"
        wallet_artifact_path = "contracts/dev_wallet.rv32a"
        "#;
}
