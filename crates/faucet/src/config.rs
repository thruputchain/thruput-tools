use crate::airdrop::{AirdropRuntimeConfig, AirdropRuntimeConfigError};
use crate::key_source::{
    DefaultFaucetSecretKeyLoader, FaucetSecretKeyLoadError, FaucetSecretKeyLoader,
    FaucetSecretKeySource,
};
use axum::http::HeaderValue;
use serde::Deserialize;
use std::collections::HashSet;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::network_descriptor::{
    AirdropDescriptor, FaucetDescriptor, NativeTokenDescriptor, NetworkDescriptor,
    NetworkDescriptorEndpoints,
};
use thruput_json_rpc_types::{
    ContractAddress, Ed25519PublicKey, NetworkHash, NetworkHashText, SHA256Hash,
};
use thruput_signing::{LocalSigner, PubkeyAccount};

pub(crate) const MIN_AIRDROP_TERMINAL_RETENTION_SECS: u64 = 12 * 60 * 60;

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
    /// Network hash every faucet-signed transaction is bound to.
    pub network_hash: thruput_xdr::types::NetworkHash,
    /// Browser origins allowed to call the API.
    pub cors_allowed_origins: Vec<HeaderValue>,
    /// Private-preview policy controls.
    pub policy: PolicyRuntimeConfig,
}

impl FaucetConfigFile {
    /// Convert this config into the public network descriptor served by `/v1/network`.
    pub fn into_network_descriptor(self) -> NetworkDescriptor {
        let policy = self.policy.into_runtime_config();
        self.network
            .into_descriptor(self.airdrop.raw_amount, &policy.airdrop)
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
        if !(1..=1_000_000).contains(&self.policy.airdrop.max_retained_attempts) {
            return Err(FaucetConfigError::InvalidAirdropRetainedAttempts);
        }
        validate_airdrop_client_overrides(&self.policy.airdrop.client_overrides)?;
        validate_airdrop_terminal_retention(self.policy.airdrop.recipient_cooldown_secs)?;
        validate_quota_window(
            "policy.airdrop.ip_quota_window_secs",
            self.policy.airdrop.ip_quota_window_secs,
        )?;
        let public_key = self.network.faucet.public_key.0.into_inner();
        if PubkeyAccount::from_public_key_bytes(public_key, 0).address()
            != self.network.faucet.wallet_address.0
        {
            return Err(FaucetConfigError::FaucetAddressMismatch);
        }
        let faucet_secret_key = key_loader.load(&self.airdrop.secret_key_source).await?;
        validate_faucet_public_key_match(&self.network.faucet.public_key, &faucet_secret_key)?;
        let airdrop_raw_amount = self.airdrop.raw_amount.clone();
        let airdrop = self.airdrop.into_runtime_config(faucet_secret_key)?;
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
        let policy = self.policy.into_runtime_config();
        let network_hash = self.network.network_hash.to_network_hash();
        let network_descriptor = self
            .network
            .into_descriptor(airdrop_raw_amount, &policy.airdrop);
        Ok(FaucetConfigParts {
            listen_address: self.listen_address,
            network_descriptor,
            airdrop,
            network_hash,
            policy,
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
}

impl PolicyConfig {
    fn into_runtime_config(self) -> PolicyRuntimeConfig {
        PolicyRuntimeConfig {
            trust_proxy_client_ip_headers: self.trust_proxy_client_ip_headers,
            airdrop: AirdropPolicyRuntimeConfig {
                recipient_cooldown_secs: self.airdrop.recipient_cooldown_secs,
                ip_quota_window_secs: self.airdrop.ip_quota_window_secs,
                ip_quota_max: self.airdrop.ip_quota_max,
                client_overrides: self.airdrop.client_overrides,
                max_pending_per_recipient: self.airdrop.max_pending_per_recipient,
                max_retained_attempts: self.airdrop.max_retained_attempts,
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
    pub client_overrides: Vec<AirdropClientOverride>,
    pub max_pending_per_recipient: u32,
    /// Global bound on retained airdrop idempotency records, including unresolved attempts.
    pub max_retained_attempts: usize,
}

/// Finite airdrop quota for a client identified by its socket peer IP.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirdropClientOverride {
    /// Socket peer IP receiving this quota instead of the default client quota.
    pub ip: IpAddr,
    /// Maximum accepted attempts within the override window.
    pub quota_max: u32,
    /// Override quota window in seconds.
    pub quota_window_secs: u64,
}

impl Default for AirdropPolicyConfig {
    fn default() -> Self {
        Self {
            recipient_cooldown_secs: 60,
            ip_quota_window_secs: 60 * 60,
            ip_quota_max: 10,
            client_overrides: Vec::new(),
            max_pending_per_recipient: 1,
            max_retained_attempts: 10_000,
        }
    }
}

/// Runtime policy controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRuntimeConfig {
    pub trust_proxy_client_ip_headers: bool,
    pub airdrop: AirdropPolicyRuntimeConfig,
}

/// Runtime airdrop policy controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirdropPolicyRuntimeConfig {
    pub recipient_cooldown_secs: u64,
    pub ip_quota_window_secs: u64,
    pub ip_quota_max: u32,
    pub client_overrides: Vec<AirdropClientOverride>,
    pub max_pending_per_recipient: u32,
    /// Global bound on retained airdrop idempotency records, including unresolved attempts.
    pub max_retained_attempts: usize,
}

/// Errors returned while validating faucet service config.
#[derive(Debug, thiserror::Error)]
pub enum FaucetConfigError {
    /// Airdrop idempotency storage must use the supported finite capacity.
    #[error("policy.airdrop.max_retained_attempts must be between 1 and 1000000")]
    InvalidAirdropRetainedAttempts,
    /// A gateway client quota override used a zero-length window.
    #[error("airdrop client override for {ip} must use a nonzero quota window")]
    ZeroAirdropClientOverrideWindow { ip: IpAddr },
    /// More than one gateway quota override targeted the same socket IP.
    #[error("duplicate airdrop client override for {ip}")]
    DuplicateAirdropClientOverride { ip: IpAddr },
    /// A gateway quota window cannot be represented by the system clock.
    #[error("airdrop client override for {ip} uses an unrepresentable quota window")]
    AirdropClientOverrideWindowTooLarge { ip: IpAddr },
    /// Terminal airdrop retention cannot be represented by the system clock.
    #[error("airdrop recipient cooldown uses an unrepresentable retention window")]
    AirdropTerminalRetentionTooLarge,
    /// A default quota window cannot be represented by the system clock.
    #[error("{field} uses an unrepresentable quota window")]
    QuotaWindowTooLarge { field: &'static str },
    /// The configured faucet signing key could not be loaded safely.
    #[error(transparent)]
    SecretKey(#[from] FaucetSecretKeyLoadError),
    /// The private airdrop config was invalid.
    #[error(transparent)]
    Airdrop(#[from] AirdropRuntimeConfigError),
    /// A configured CORS origin was invalid.
    #[error("invalid CORS origin `{origin}`: {message}")]
    InvalidCorsOrigin { origin: String, message: String },
    /// The configured address must be the public key's nonce-zero native account.
    #[error(
        "network.faucet.wallet_address does not match the public key nonce-zero pubkey account"
    )]
    FaucetAddressMismatch,
    /// The loaded secret key did not control the configured faucet account.
    #[error("loaded faucet signing key does not match network.faucet.public_key")]
    FaucetPublicKeyMismatch,
    /// The network descriptor did not advertise an airdrop policy.
    #[error("faucet network descriptor must advertise airdrop policy")]
    MissingAirdropDescriptor,
    /// A descriptor amount was not a valid decimal u128.
    #[error("network descriptor {field} `{value}` is not a valid decimal amount")]
    InvalidDescriptorAmount {
        /// Descriptor field holding the invalid amount.
        field: &'static str,
        /// The rejected value.
        value: String,
    },
    /// A submitter's transfer amount disagreed with the public descriptor.
    #[error("{submitter} amount {actual} does not match network descriptor amount {expected}")]
    SubmitterAmountMismatch {
        /// Which submitter carried the mismatched amount.
        submitter: &'static str,
        /// Amount advertised by the network descriptor.
        expected: u128,
        /// Amount the submitter would transfer.
        actual: u128,
    },
}

fn validate_airdrop_client_overrides(
    overrides: &[AirdropClientOverride],
) -> Result<(), FaucetConfigError> {
    let mut ips = HashSet::with_capacity(overrides.len());
    for client_override in overrides {
        if client_override.quota_window_secs == 0 {
            return Err(FaucetConfigError::ZeroAirdropClientOverrideWindow {
                ip: client_override.ip,
            });
        }
        if SystemTime::now()
            .checked_add(Duration::from_secs(client_override.quota_window_secs))
            .is_none()
        {
            return Err(FaucetConfigError::AirdropClientOverrideWindowTooLarge {
                ip: client_override.ip,
            });
        }
        if !ips.insert(client_override.ip) {
            return Err(FaucetConfigError::DuplicateAirdropClientOverride {
                ip: client_override.ip,
            });
        }
    }
    Ok(())
}

fn validate_airdrop_terminal_retention(
    recipient_cooldown_secs: u64,
) -> Result<(), FaucetConfigError> {
    SystemTime::now()
        .checked_add(Duration::from_secs(
            recipient_cooldown_secs.max(MIN_AIRDROP_TERMINAL_RETENTION_SECS),
        ))
        .ok_or(FaucetConfigError::AirdropTerminalRetentionTooLarge)
        .map(|_| ())
}

fn validate_quota_window(field: &'static str, seconds: u64) -> Result<(), FaucetConfigError> {
    SystemTime::now()
        .checked_add(Duration::from_secs(seconds))
        .ok_or(FaucetConfigError::QuotaWindowTooLarge { field })
        .map(|_| ())
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
    /// Network hash the faucet signs against and advertises publicly.
    pub network_hash: NetworkHashText,
    /// Public endpoint configuration.
    pub endpoints: EndpointConfig,
    /// Native token configuration.
    pub native_token: NativeTokenConfig,
    /// Public faucet account configuration.
    pub faucet: FaucetAccountConfig,
}

impl NetworkConfig {
    fn into_descriptor(
        self,
        airdrop_raw_amount: String,
        policy: &AirdropPolicyRuntimeConfig,
    ) -> NetworkDescriptor {
        let network_hash = NetworkHash::from(self.network_hash.to_network_hash().into_inner());
        NetworkDescriptor::new(
            self.network_id,
            network_hash,
            self.protocol,
            self.genesis_hash,
            NetworkDescriptorEndpoints::new(self.endpoints.rpc_url),
            NativeTokenDescriptor::new(
                self.native_token.token_address,
                self.native_token.decimals,
                self.native_token.transfer_entrypoint,
            ),
            FaucetDescriptor::new(self.faucet.wallet_address),
        )
        .with_airdrop(AirdropDescriptor::new(
            airdrop_raw_amount,
            policy.recipient_cooldown_secs,
            policy.ip_quota_max,
            policy.ip_quota_window_secs,
            policy.max_pending_per_recipient,
        ))
    }
}

/// Public HTTP endpoint configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointConfig {
    /// Backend JSON-RPC endpoint URL used by the faucet service.
    pub rpc_url: String,
}

/// Native token configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTokenConfig {
    /// Native token contract address.
    pub token_address: ContractAddress,
    /// Display decimals for native token amounts.
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
    pub public_key: Ed25519PublicKey,
}

fn validate_faucet_public_key_match(
    expected_public_key: &Ed25519PublicKey,
    faucet_secret_key: &[u8; 32],
) -> Result<(), FaucetConfigError> {
    let actual_public_key = LocalSigner::from_secret_key(*faucet_secret_key).verifying_key_bytes();
    if expected_public_key.0.as_ref() != actual_public_key {
        return Err(FaucetConfigError::FaucetPublicKeyMismatch);
    }
    Ok(())
}

/// Private airdrop execution configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirdropConfig {
    /// Raw native token units transferred for each airdrop request.
    pub raw_amount: String,
    /// Source used to load the faucet signing key at startup.
    pub secret_key_source: FaucetSecretKeySource,
}

impl AirdropConfig {
    /// Convert wire config into validated runtime airdrop config.
    pub fn into_runtime_config(
        self,
        faucet_secret_key: zeroize::Zeroizing<[u8; 32]>,
    ) -> Result<AirdropRuntimeConfig, AirdropRuntimeConfigError> {
        AirdropRuntimeConfig::from_wire(&self.raw_amount, faucet_secret_key)
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use thruput_json_rpc_types::Ed25519PublicKey;
    use thruput_signing::{LocalSigner, PubkeyAccount};
    use zeroize::Zeroizing;

    use crate::key_source::{
        FaucetSecretKeyLoadError, FaucetSecretKeyLoader, FaucetSecretKeySource,
    };

    use super::{
        AirdropClientOverride, AirdropConfig, FaucetConfigError, FaucetConfigFile,
        validate_airdrop_client_overrides, validate_airdrop_terminal_retention,
        validate_quota_window,
    };

    #[tokio::test]
    async fn retained_attempt_capacity_defaults_and_rejects_invalid_bounds_before_key_loading() {
        let mut config: FaucetConfigFile = toml::from_str(&test_config()).unwrap();
        assert_eq!(config.policy.airdrop.max_retained_attempts, 10_000);
        for capacity in [0, 1_000_001, usize::MAX] {
            config.policy.airdrop.max_retained_attempts = capacity;
            assert!(matches!(
                config
                    .clone()
                    .into_parts_with_key_loader(&FailIfCalledKeyLoader)
                    .await,
                Err(FaucetConfigError::InvalidAirdropRetainedAttempts)
            ));
        }
    }

    #[test]
    fn retained_attempt_capacity_rejects_negative_and_overflow_values() {
        for value in ["-1", "18446744073709551616"] {
            let source = format!(
                "{}\n[policy.airdrop]\nmax_retained_attempts = {value}",
                test_config()
            );
            assert!(toml::from_str::<FaucetConfigFile>(&source).is_err());
        }
    }

    struct TestKeyLoader;

    struct FailIfCalledKeyLoader;

    #[test]
    fn example_config_parses() {
        let config: FaucetConfigFile = toml::from_str(include_str!("../faucet.example.toml"))
            .expect("bug: example faucet config should parse");

        assert_eq!(config.network.native_token.decimals, 7);
        assert_eq!(config.airdrop.raw_amount, "1000000000");
    }

    #[test]
    fn airdrop_client_override_rejects_zero_window() {
        let error = validate_airdrop_client_overrides(&[AirdropClientOverride {
            ip: "10.0.0.12".parse().expect("test IP should parse"),
            quota_max: 10_000,
            quota_window_secs: 0,
        }])
        .expect_err("zero window must not disable quota enforcement");

        assert!(matches!(
            error,
            FaucetConfigError::ZeroAirdropClientOverrideWindow { .. }
        ));
    }

    #[test]
    fn airdrop_client_override_rejects_duplicate_ip() {
        let client_override = AirdropClientOverride {
            ip: "10.0.0.12".parse().expect("test IP should parse"),
            quota_max: 10_000,
            quota_window_secs: 3_600,
        };
        let error = validate_airdrop_client_overrides(&[client_override.clone(), client_override])
            .expect_err("duplicate IP overrides must not depend on declaration order");

        assert!(matches!(
            error,
            FaucetConfigError::DuplicateAirdropClientOverride { .. }
        ));
    }

    #[test]
    fn airdrop_client_override_rejects_unrepresentable_window() {
        let error = validate_airdrop_client_overrides(&[AirdropClientOverride {
            ip: "10.0.0.12".parse().expect("test IP should parse"),
            quota_max: 10_000,
            quota_window_secs: u64::MAX,
        }])
        .expect_err("overflowing window must not disable quota enforcement");

        assert!(matches!(
            error,
            FaucetConfigError::AirdropClientOverrideWindowTooLarge { .. }
        ));
    }

    #[test]
    fn airdrop_policy_rejects_unrepresentable_terminal_retention() {
        let error = validate_airdrop_terminal_retention(u64::MAX)
            .expect_err("overflowing retention must not disable expiration");

        assert!(matches!(
            error,
            FaucetConfigError::AirdropTerminalRetentionTooLarge
        ));
    }

    #[test]
    fn default_policy_rejects_unrepresentable_quota_window() {
        let error = validate_quota_window("policy.airdrop.ip_quota_window_secs", u64::MAX)
            .expect_err("overflowing window must not disable quota enforcement");

        assert!(matches!(
            error,
            FaucetConfigError::QuotaWindowTooLarge {
                field: "policy.airdrop.ip_quota_window_secs"
            }
        ));
    }

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

        assert_eq!(
            descriptor.schema_version,
            thruput_json_rpc_types::network_descriptor::NETWORK_DESCRIPTOR_SCHEMA_VERSION
        );
        assert_eq!(descriptor.kind, "groundhog-network");
        assert_eq!(descriptor.network_id, "devnet-preview");
        assert_eq!(descriptor.endpoints.rpc_url, "https://rpc.devnet.example");
        assert_eq!(descriptor.native_token.decimals, 7);
        let airdrop = descriptor
            .airdrop
            .expect("bug: faucet descriptor should advertise airdrop policy");
        assert_eq!(airdrop.raw_amount, "100000000");
        assert_eq!(airdrop.recipient_cooldown_seconds, 60);
        assert_eq!(airdrop.client_quota_max, 10);
        assert_eq!(airdrop.client_quota_window_seconds, 3_600);
        assert_eq!(airdrop.max_pending_per_recipient, 1);
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
    async fn config_builds_default_policy_config() {
        let config: FaucetConfigFile =
            toml::from_str(&test_config()).expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: policy runtime config should validate");

        assert!(!parts.policy.trust_proxy_client_ip_headers);
        assert_eq!(parts.policy.airdrop.recipient_cooldown_secs, 60);
        assert_eq!(parts.policy.airdrop.ip_quota_max, 10);
    }

    #[tokio::test]
    async fn config_overrides_policy_config() {
        let config: FaucetConfigFile = toml::from_str(&format!(
            "{}\n{}",
            test_config(),
            r#"
            [policy]
            trust_proxy_client_ip_headers = true

            [policy.airdrop]
            recipient_cooldown_secs = 60
            ip_quota_window_secs = 30
            ip_quota_max = 2
            max_retained_attempts = 200000
            max_pending_per_recipient = 1

            [[policy.airdrop.client_overrides]]
            ip = "10.0.0.12"
            quota_max = 10000
            quota_window_secs = 3600
            "#
        ))
        .expect("bug: fixture config should parse");

        let parts = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .expect("bug: policy runtime config should validate");

        assert!(parts.policy.trust_proxy_client_ip_headers);
        assert_eq!(parts.policy.airdrop.max_retained_attempts, 200_000);
        assert_eq!(parts.policy.airdrop.recipient_cooldown_secs, 60);
        assert_eq!(parts.policy.airdrop.client_overrides[0].quota_max, 10_000);
    }

    #[tokio::test]
    async fn config_advertises_zero_airdrop_policy_values() {
        let config: FaucetConfigFile = toml::from_str(&format!(
            "{}\n{}",
            test_config(),
            r"
            [policy.airdrop]
            recipient_cooldown_secs = 0
            ip_quota_window_secs = 0
            ip_quota_max = 0
            max_pending_per_recipient = 0
            "
        ))
        .expect("bug: zero-policy fixture should parse");

        let descriptor = config.into_network_descriptor();
        let policy = descriptor
            .airdrop
            .expect("airdrop policy should be advertised");

        assert_eq!(policy.recipient_cooldown_seconds, 0);
        assert_eq!(policy.client_quota_max, 0);
        assert_eq!(policy.client_quota_window_seconds, 0);
        assert_eq!(policy.max_pending_per_recipient, 0);
    }

    #[test]
    fn config_rejects_unknown_fields() {
        // Start from a complete, valid document so the unknown key is the only
        // problem the parser can report.
        let err =
            toml::from_str::<FaucetConfigFile>(&format!("unexpected = true\n{}", test_config()))
                .expect_err("bug: unknown fields should fail");

        assert!(err.to_string().contains("unknown field"), "{err}");
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
    async fn config_rejects_secret_that_does_not_match_faucet_public_key() {
        let mut config: FaucetConfigFile = toml::from_str(&test_config()).unwrap();
        config.network.faucet.public_key =
            Ed25519PublicKey::from(thruput_xdr::types::Ed25519PublicKey::from([0xaa; 32]));
        config.network.faucet.wallet_address = thruput_json_rpc_types::base58::Base58(
            PubkeyAccount::from_public_key_bytes([0xaa; 32], 0).address(),
        );
        let error = config
            .into_parts_with_key_loader(&TestKeyLoader)
            .await
            .unwrap_err();
        assert!(matches!(error, FaucetConfigError::FaucetPublicKeyMismatch));
        assert!(!error.to_string().contains(&"aa".repeat(32)));
    }

    #[test]
    fn config_rejects_malformed_public_key_before_loading_secret() {
        let mut config: toml::Value = toml::from_str(&test_config()).unwrap();
        for key in ["1", "0xdeadbeef", "not-a-key"] {
            config["network"]["faucet"]["public_key"] = toml::Value::String(key.into());
            assert!(config.clone().try_into::<FaucetConfigFile>().is_err());
        }
    }

    #[tokio::test]
    async fn config_rejects_wrong_native_address_before_loading_secret() {
        let mut config: FaucetConfigFile = toml::from_str(&test_config()).unwrap();
        config.network.faucet.wallet_address = thruput_json_rpc_types::base58::Base58(
            PubkeyAccount::from_public_key_bytes(
                LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes(),
                1,
            )
            .address(),
        );
        let error = config
            .into_parts_with_key_loader(&FailIfCalledKeyLoader)
            .await
            .unwrap_err();
        assert!(matches!(error, FaucetConfigError::FaucetAddressMismatch));
    }

    #[test]
    fn retired_wallet_configuration_is_rejected() {
        for section in [
            "[network.wallet_create]\nprefund_raw_amount = \"1\"",
            "[network.dev_wallet]\ndeploy_entrypoint = \"0x00\"",
            "[wallet_create]\nwallet_artifact_path = \"dev_wallet.rv32a\"",
            "[policy.wallet_create]\nowner_quota_max = 3",
        ] {
            assert!(
                toml::from_str::<FaucetConfigFile>(&format!("{}\n{section}", test_config()))
                    .is_err()
            );
        }
    }

    #[test]
    fn config_parses_aws_secrets_manager_key_source() {
        let config: FaucetConfigFile = toml::from_str(&test_config().replace(
            "type = \"environment\"\n        variable = \"GROUNDHOG_FAUCET_SECRET_KEY\"",
            "type = \"aws_secrets_manager\"\n        secret_id = \"groundhog/devnet/faucet\"\n        version_id = \"version-1\"\n        region = \"us-west-2\"",
        ))
        .expect("bug: AWS key-source fixture should parse");

        assert!(matches!(
            config.airdrop.secret_key_source,
            FaucetSecretKeySource::AwsSecretsManager {
                ref secret_id,
                ref version_id,
                ref region,
            } if secret_id == "groundhog/devnet/faucet"
                && version_id == "version-1"
                && region == "us-west-2"
        ));
    }

    #[test]
    fn config_rejects_aws_secrets_manager_source_without_version() {
        let result = toml::from_str::<FaucetConfigFile>(&test_config().replace(
            "type = \"environment\"\n        variable = \"GROUNDHOG_FAUCET_SECRET_KEY\"",
            "type = \"aws_secrets_manager\"\n        secret_id = \"groundhog/devnet/faucet\"\n        region = \"us-west-2\"",
        ));

        assert!(result.is_err(), "AWS secret source must pin a version ID");
    }

    fn test_config() -> String {
        let public_key = Ed25519PublicKey::from(thruput_xdr::types::Ed25519PublicKey::from(
            LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes(),
        ))
        .to_string();
        let address = thruput_json_rpc_types::ContractAddress::from(
            PubkeyAccount::from_public_key_bytes(
                LocalSigner::from_secret_key([0x11; 32]).verifying_key_bytes(),
                0,
            )
            .address(),
        )
        .to_string();
        TEST_CONFIG
            .replace("__FAUCET_PUBLIC_KEY__", &public_key)
            .replace("__FAUCET_ADDRESS__", &address)
    }

    const TEST_CONFIG: &str = r#"
        listen_address = "127.0.0.1:8081"
        cors_allowed_origins = ["http://127.0.0.1:5173"]

        [network]
        network_id = "devnet-preview"
        protocol = "x23"
        genesis_hash = "11111111111111111111111111111111"
        network_hash = "11111111111111111111111111111111"

        [network.endpoints]
        rpc_url = "https://rpc.devnet.example"

        [network.native_token]
        token_address = "11111111111111111111111111111112"
        decimals = 7
        transfer_entrypoint = "0x63285b3523f9dc3f"

        [network.faucet]
        wallet_address = "__FAUCET_ADDRESS__"
        public_key = "__FAUCET_PUBLIC_KEY__"

        [airdrop]
        raw_amount = "100000000"

        [airdrop.secret_key_source]
        type = "environment"
        variable = "GROUNDHOG_FAUCET_SECRET_KEY"
        "#;
}
