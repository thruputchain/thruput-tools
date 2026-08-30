//! Runtime manifest types emitted by local and benchmark backends.

use std::{fmt, net::SocketAddr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ContractAddress, Ed25519PublicKey, NetworkHashHex, SHA256Hash};
use groundhog_xdr::contract::ContractEntrypointID;

/// Current runtime manifest schema version.
pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Runtime manifest family emitted by hog-devnode.
pub const HOGD_DEV_RUNTIME_MANIFEST_KIND: &str = "hogd-dev-runtime";

/// Runtime manifest family emitted by chuckd hosted bootstrap.
pub const HOSTED_DEVNET_RUNTIME_MANIFEST_KIND: &str = "hosted-devnet-runtime";

/// Runtime role assigned to the fixed local faucet account.
pub const BOOTSTRAP_ACCOUNT_ROLE_FAUCET: &str = "faucet";

/// Runtime role assigned to local developer accounts.
pub const BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER: &str = "local_user";

/// Runtime manifest written by a backend after bootstrap is complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifest {
    /// Version of this JSON contract.
    pub schema_version: u32,
    /// Producer family, for example [`HOGD_DEV_RUNTIME_MANIFEST_KIND`] or
    /// [`HOSTED_DEVNET_RUNTIME_MANIFEST_KIND`].
    pub kind: String,
    /// Whether this process initialized a new chain or recovered a committed chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_state: Option<RuntimeManifestChainState>,
    /// Unprefixed 32-byte network hash shared by all services for this runtime.
    pub network_hash: NetworkHashHex,
    /// x23 protocol string expected by backend services.
    pub protocol: String,
    /// Build metadata for the manifest producer.
    pub producer: RuntimeManifestProducer,
    /// Dial addresses for services available in this runtime.
    pub endpoints: RuntimeManifestEndpoints,
    /// Seed and scheme shared by all listed bootstrap accounts.
    pub bootstrap_account_derivation: BootstrapAccountDerivation,
    /// Accounts present in genesis and funded by the producer.
    pub bootstrap_accounts: Vec<BootstrapAccount>,
    /// Deployed bootstrap artifacts and cost model metadata.
    pub bootstrap: RuntimeManifestBootstrap,
}

/// How the producer obtained the chain served by this runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeManifestChainState {
    /// No committed chain existed, so the producer initialized a new chain.
    Fresh,
    /// The producer opened and recovered an existing committed chain.
    Recovered,
}

impl fmt::Display for RuntimeManifestChainState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fresh => "fresh",
            Self::Recovered => "recovered",
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeManifestEnvelope {
    schema_version: u32,
}

/// Parse a runtime manifest after validating its schema envelope.
pub fn parse_runtime_manifest(bytes: &[u8]) -> Result<RuntimeManifest, RuntimeManifestParseError> {
    let envelope: RuntimeManifestEnvelope = serde_json::from_slice(bytes)?;
    if envelope.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(RuntimeManifestParseError::UnsupportedSchema {
            expected: RUNTIME_MANIFEST_SCHEMA_VERSION,
            actual: envelope.schema_version,
        });
    }
    Ok(serde_json::from_slice(bytes)?)
}

/// Errors returned while parsing a versioned runtime manifest.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeManifestParseError {
    /// The manifest is not valid JSON for its declared schema.
    #[error("invalid runtime manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The manifest declares a schema this build does not support.
    #[error("unsupported runtime manifest schemaVersion {actual}; expected {expected}")]
    UnsupportedSchema {
        /// Schema supported by this build.
        expected: u32,
        /// Schema declared by the manifest.
        actual: u32,
    },
}

/// Runtime manifest producer metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestProducer {
    /// Executable name that wrote the manifest.
    pub binary: String,
    /// Build version reported by the producer.
    pub version: String,
    /// Source revision when the producer can report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
}

/// Advertised backend endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestEndpoints {
    /// Required mempool scheduler service.
    pub scheduler: SocketAddr,
    /// Block explorer service, omitted when the read plane is unavailable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_endpoint"
    )]
    pub indexer: Option<SocketAddr>,
    /// Database gateway service, omitted when storage reads are unavailable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_endpoint"
    )]
    pub db_gateway: Option<SocketAddr>,
    /// View gateway service, omitted when contract views are unavailable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_endpoint"
    )]
    pub view_gateway: Option<SocketAddr>,
}

/// Deterministic account derivation metadata shared by all bootstrap accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BootstrapAccountDerivation {
    /// Algorithm identifier for reproducing bootstrap account keys.
    pub scheme: String,
    /// Public deterministic seed consumed by the derivation scheme.
    pub seed: String,
}

/// Public bootstrap account metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BootstrapAccount {
    /// Position passed to the derivation scheme.
    pub index: u32,
    /// Account role discriminator. Local runtimes use `faucet` for index 0 and
    /// `local_user` for ordinary workload accounts.
    pub role: String,
    /// Public key authorized for the wallet.
    pub public_key: Ed25519PublicKey,
    /// Wallet contract deployed and funded for this account.
    pub wallet_address: ContractAddress,
    /// Initial storage-token balance as a decimal string.
    pub initial_storage_tokens: String,
}

/// Runtime bootstrap metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestBootstrap {
    /// Wallet artifact installed during bootstrap.
    pub dev_wallet: RuntimeManifestDevWallet,
    /// Concrete storage costs used at genesis.
    pub storage_cost_model: RuntimeManifestStorageCostModel,
}

/// Dev-wallet bootstrap artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestDevWallet {
    /// Hash of the wallet artifact bytes.
    pub content_hash: SHA256Hash,
    /// Hash of the code installed in genesis.
    pub deployed_code_hash: SHA256Hash,
    /// Human-readable artifact location hint from the producer.
    pub artifact_hint: String,
    /// Entrypoint used to deploy the wallet.
    pub deploy_entrypoint: String,
    /// Entrypoint used to sponsor creation of a new initialized wallet.
    pub create_wallet_entrypoint: String,
    /// Entrypoint used to submit wallet actions.
    pub do_action_entrypoint: String,
}

impl RuntimeManifestDevWallet {
    /// Parse the advertised dev-wallet `deploy` entrypoint into its XDR ID.
    pub fn deploy_entrypoint_id(
        &self,
    ) -> Result<ContractEntrypointID, RuntimeManifestEntrypointError> {
        parse_entrypoint_id(
            "bootstrap.devWallet.deployEntrypoint",
            &self.deploy_entrypoint,
        )
    }

    /// Parse the advertised dev-wallet `doAction` entrypoint into its XDR ID.
    pub fn do_action_entrypoint_id(
        &self,
    ) -> Result<ContractEntrypointID, RuntimeManifestEntrypointError> {
        parse_entrypoint_id(
            "bootstrap.devWallet.doActionEntrypoint",
            &self.do_action_entrypoint,
        )
    }

    /// Parse the advertised dev-wallet `createWallet` entrypoint into its XDR ID.
    pub fn create_wallet_entrypoint_id(
        &self,
    ) -> Result<ContractEntrypointID, RuntimeManifestEntrypointError> {
        parse_entrypoint_id(
            "bootstrap.devWallet.createWalletEntrypoint",
            &self.create_wallet_entrypoint,
        )
    }
}

fn parse_entrypoint_id(
    field: &'static str,
    value: &str,
) -> Result<ContractEntrypointID, RuntimeManifestEntrypointError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex::decode(value)
        .map_err(|source| RuntimeManifestEntrypointError::InvalidHex { field, source })?;
    let len = bytes.len();
    let bytes: [u8; 8] = bytes
        .try_into()
        .map_err(|_| RuntimeManifestEntrypointError::InvalidLength { field, len })?;
    Ok(ContractEntrypointID::from(bytes))
}

/// Errors returned while parsing entrypoint IDs advertised by a runtime manifest.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeManifestEntrypointError {
    /// The manifest field was not valid hex.
    #[error("{field} is not valid hex: {source}")]
    InvalidHex {
        /// JSON field path being parsed.
        field: &'static str,
        /// Hex decoder failure.
        #[source]
        source: hex::FromHexError,
    },
    /// The manifest field decoded to the wrong byte length.
    #[error("{field} must be 8 bytes, got {len}")]
    InvalidLength {
        /// JSON field path being parsed.
        field: &'static str,
        /// Decoded byte length.
        len: usize,
    },
}

/// Concrete storage cost model used at genesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestStorageCostModel {
    /// Hash over the deterministic version-tagged cost-model encoding.
    pub hash: SHA256Hash,
    /// Decimal-string base cost charged per data item.
    pub base_data_item_cost: String,
    /// Decimal-string per-byte cost for bytestring values.
    pub bytestring_cost_per_byte: String,
    /// Decimal-string base cost charged per contract deployment.
    pub base_contract_deployment_cost: String,
    /// Decimal-string per-byte cost for deployed contract code.
    pub contract_deployment_cost_per_byte: String,
    /// Decimal-string cost charged per authorization key.
    pub auth_per_key_cost: String,
    /// Decimal-string cost charged per revalidation-queue entry.
    pub rcq_cost_per_entry: String,
    /// Decimal-string cost charged per replay-cache entry.
    pub replay_cache_cost_per_entry: String,
    /// Decimal-string cost charged for balance storage.
    pub balance_cost: String,
}

fn optional_endpoint<'de, D>(deserializer: D) -> Result<Option<SocketAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    SocketAddr::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use groundhog_xdr::types::{ContractAddress, Ed25519PublicKey, SHA256Hash};
    use serde_json::json;

    use crate::hex::Hex0x;

    use super::{
        BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BootstrapAccount, BootstrapAccountDerivation,
        RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest, RuntimeManifestBootstrap,
        RuntimeManifestChainState, RuntimeManifestDevWallet, RuntimeManifestEndpoints,
        RuntimeManifestProducer, RuntimeManifestStorageCostModel,
    };

    #[test]
    fn runtime_manifest_serializes_expected_shape() {
        let value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 2,
                "kind": "hogd-dev-runtime",
                "chainState": "fresh",
                "networkHash": "4242424242424242424242424242424242424242424242424242424242424242",
                "protocol": "hogp/test00000000000",
                "producer": {
                    "binary": "hogd-dev",
                    "version": "0.1.0",
                    "commitHash": "abcdef0",
                },
                "endpoints": {
                    "scheduler": "127.0.0.1:1000",
                    "indexer": "127.0.0.1:1001",
                    "dbGateway": "127.0.0.1:1002",
                    "viewGateway": "127.0.0.1:1003",
                },
                "bootstrapAccountDerivation": {
                    "scheme": "groundhog-bootstrap-account-v1",
                    "seed": "groundhog-dev-accounts",
                },
                "bootstrapAccounts": [{
                    "index": 0,
                    "role": BOOTSTRAP_ACCOUNT_ROLE_FAUCET,
                    "publicKey": prefixed_hex(0x61),
                    "walletAddress": prefixed_hex(0x71),
                    "initialStorageTokens": "10000000000000",
                }],
                "bootstrap": {
                    "devWallet": {
                        "contentHash": prefixed_hex(0x51),
                        "deployedCodeHash": prefixed_hex(0x52),
                        "artifactHint": "./dev_wallet.rv32a",
                        "deployEntrypoint": "0xadac498222148e4f",
                        "createWalletEntrypoint": "0x6372656174655761",
                        "doActionEntrypoint": "0x646f416374696f6e",
                    },
                    "storageCostModel": {
                        "hash": prefixed_hex(0x53),
                        "baseDataItemCost": "12000",
                        "bytestringCostPerByte": "10",
                        "baseContractDeploymentCost": "12000",
                        "contractDeploymentCostPerByte": "10",
                        "authPerKeyCost": "1111",
                        "rcqCostPerEntry": "8",
                        "replayCacheCostPerEntry": "7",
                        "balanceCost": "1234",
                    },
                },
            })
        );
    }

    #[test]
    fn runtime_manifest_rejects_invalid_network_hash() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value["networkHash"] = json!("groundhog-local");

        let error = serde_json::from_value::<RuntimeManifest>(value)
            .expect_err("invalid network hash should fail");

        assert!(
            error
                .to_string()
                .contains("networkHash must be exactly 64 lowercase hexadecimal characters")
        );
    }

    #[test]
    fn runtime_manifest_rejects_noncanonical_network_hash() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value["networkHash"] = json!("AA".repeat(32));

        let error = serde_json::from_value::<RuntimeManifest>(value)
            .expect_err("uppercase network hash should fail");

        assert!(error.to_string().contains("lowercase hexadecimal"));
    }

    #[test]
    fn runtime_manifest_rejects_unknown_chain_state() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value["chainState"] = json!("existing_data_dir");

        let error = serde_json::from_value::<RuntimeManifest>(value)
            .expect_err("unknown chain state should fail");

        assert!(error.to_string().contains("unknown variant"));
    }

    #[test]
    fn runtime_manifest_allows_missing_additive_chain_state() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value
            .as_object_mut()
            .expect("bug: manifest is an object")
            .remove("chainState");

        let manifest = serde_json::from_value::<RuntimeManifest>(value)
            .expect("schema 2 compatibility should allow a missing additive field");

        assert_eq!(manifest.chain_state, None);
    }

    #[test]
    fn runtime_manifest_parser_rejects_schema_before_decoding_body() {
        let bytes = br#"{"schemaVersion":99,"futureBody":true}"#;

        let error = super::parse_runtime_manifest(bytes)
            .expect_err("unsupported schema should fail before body decoding");

        assert!(matches!(
            error,
            super::RuntimeManifestParseError::UnsupportedSchema {
                expected: RUNTIME_MANIFEST_SCHEMA_VERSION,
                actual: 99,
            }
        ));
    }

    fn full_manifest() -> RuntimeManifest {
        RuntimeManifest {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            kind: "hogd-dev-runtime".into(),
            chain_state: Some(RuntimeManifestChainState::Fresh),
            network_hash: crate::NetworkHashHex::from([0x42; 32]),
            protocol: "hogp/test00000000000".into(),
            producer: RuntimeManifestProducer {
                binary: "hogd-dev".into(),
                version: "0.1.0".into(),
                commit_hash: Some("abcdef0".into()),
            },
            endpoints: RuntimeManifestEndpoints {
                scheduler: addr("127.0.0.1:1000"),
                indexer: Some(addr("127.0.0.1:1001")),
                db_gateway: Some(addr("127.0.0.1:1002")),
                view_gateway: Some(addr("127.0.0.1:1003")),
            },
            bootstrap_account_derivation: BootstrapAccountDerivation {
                scheme: "groundhog-bootstrap-account-v1".into(),
                seed: "groundhog-dev-accounts".into(),
            },
            bootstrap_accounts: vec![BootstrapAccount {
                index: 0,
                role: BOOTSTRAP_ACCOUNT_ROLE_FAUCET.into(),
                public_key: public_key(0x61),
                wallet_address: contract_address(0x71),
                initial_storage_tokens: "10000000000000".into(),
            }],
            bootstrap: RuntimeManifestBootstrap {
                dev_wallet: RuntimeManifestDevWallet {
                    content_hash: hash(0x51),
                    deployed_code_hash: hash(0x52),
                    artifact_hint: "./dev_wallet.rv32a".into(),
                    deploy_entrypoint: "0xadac498222148e4f".into(),
                    create_wallet_entrypoint: "0x6372656174655761".into(),
                    do_action_entrypoint: "0x646f416374696f6e".into(),
                },
                storage_cost_model: RuntimeManifestStorageCostModel {
                    hash: hash(0x53),
                    base_data_item_cost: "12000".into(),
                    bytestring_cost_per_byte: "10".into(),
                    base_contract_deployment_cost: "12000".into(),
                    contract_deployment_cost_per_byte: "10".into(),
                    auth_per_key_cost: "1111".into(),
                    rcq_cost_per_entry: "8".into(),
                    replay_cache_cost_per_entry: "7".into(),
                    balance_cost: "1234".into(),
                },
            },
        }
    }

    #[test]
    fn scheduler_only_manifest_omits_optional_endpoints() {
        let endpoints = RuntimeManifestEndpoints {
            scheduler: addr("127.0.0.1:1000"),
            indexer: None,
            db_gateway: None,
            view_gateway: None,
        };

        let value = serde_json::to_value(endpoints).expect("bug: endpoints serialize");

        assert_eq!(
            value,
            json!({
                "scheduler": "127.0.0.1:1000",
            })
        );
    }

    #[test]
    fn optional_endpoints_must_be_absent_not_null() {
        let err = serde_json::from_value::<RuntimeManifestEndpoints>(json!({
            "scheduler": "127.0.0.1:1000",
            "indexer": null,
        }))
        .expect_err("bug: explicit null optional endpoints should fail");

        assert!(err.to_string().contains("invalid type"));
    }

    #[test]
    fn runtime_manifest_ignores_unknown_additive_fields() {
        let account = serde_json::from_value::<BootstrapAccount>(json!({
            "index": 0,
            "role": "local_user",
            "publicKey": prefixed_hex(0x61),
            "walletAddress": prefixed_hex(0x71),
            "initialStorageTokens": "1000000000",
            "producerAddedField": "ignored",
        }))
        .expect("bug: additive manifest fields should be ignored");

        assert_eq!(account.index, 0);
    }

    #[test]
    fn dev_wallet_do_action_entrypoint_parses_from_manifest() {
        let manifest = full_manifest();
        let entrypoint = manifest
            .bootstrap
            .dev_wallet
            .do_action_entrypoint_id()
            .expect("entrypoint should parse");

        assert_eq!(entrypoint.as_ref(), b"doAction");
    }

    #[test]
    fn dev_wallet_create_wallet_entrypoint_parses_from_manifest() {
        let manifest = full_manifest();
        let entrypoint = manifest
            .bootstrap
            .dev_wallet
            .create_wallet_entrypoint_id()
            .expect("entrypoint should parse");

        assert_eq!(entrypoint.as_ref(), b"createWa");
    }

    #[test]
    fn dev_wallet_deploy_entrypoint_parses_from_manifest() {
        let manifest = full_manifest();
        let entrypoint = manifest
            .bootstrap
            .dev_wallet
            .deploy_entrypoint_id()
            .expect("entrypoint should parse");

        assert_eq!(
            entrypoint.as_ref(),
            [0xad, 0xac, 0x49, 0x82, 0x22, 0x14, 0x8e, 0x4f]
        );
    }

    fn addr(value: &str) -> SocketAddr {
        value.parse().expect("bug: socket address parses")
    }

    fn hash(byte: u8) -> crate::SHA256Hash {
        Hex0x(SHA256Hash::from([byte; 32]))
    }

    fn public_key(byte: u8) -> crate::Ed25519PublicKey {
        Hex0x(Ed25519PublicKey::from([byte; 32]))
    }

    fn contract_address(byte: u8) -> crate::ContractAddress {
        Hex0x(ContractAddress::from([byte; 32]))
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
