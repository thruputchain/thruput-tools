//! Runtime manifest types emitted by local and benchmark backends.

use std::{fmt, net::SocketAddr};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ContractAddress, Ed25519PublicKey, NetworkHashText, SHA256Hash};

/// Current runtime manifest schema version.
pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 5;

/// Runtime manifest family emitted by hog-devnode.
pub const HOGD_DEV_RUNTIME_MANIFEST_KIND: &str = "hogd-dev-runtime";

/// Runtime manifest family emitted by chuckd hosted bootstrap.
pub const HOSTED_DEVNET_RUNTIME_MANIFEST_KIND: &str = "hosted-devnet-runtime";

/// Runtime role assigned to the fixed local faucet account.
pub const BOOTSTRAP_ACCOUNT_ROLE_FAUCET: &str = "faucet";

/// Runtime role assigned to local developer accounts.
pub const BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER: &str = "local_user";

/// Runtime role hogd's hosted bootstrap generator assigns to the deterministic
/// dev accounts of a hosted devnet.
pub const BOOTSTRAP_ACCOUNT_ROLE_HOSTED_DEV_ACCOUNT: &str = "hosted_dev_account";

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
    pub network_hash: NetworkHashText,
    /// x23 protocol string expected by backend services.
    pub protocol: String,
    /// Build metadata for the manifest producer.
    pub producer: RuntimeManifestProducer,
    /// Dial addresses for services available in this runtime.
    pub endpoints: RuntimeManifestEndpoints,
    /// Derivation metadata advertised only by hosted schema-4 runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_account_derivation: Option<BootstrapAccountDerivation>,
    /// Accounts present in genesis and funded by the producer.
    pub bootstrap_accounts: Vec<BootstrapAccount>,
    /// Storage cost model used at genesis.
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
    #[serde(default)]
    kind: Option<String>,
}

/// Parse a runtime manifest after validating its schema envelope.
pub fn parse_runtime_manifest(bytes: &[u8]) -> Result<RuntimeManifest, RuntimeManifestParseError> {
    use serde::de::Error;

    let envelope: RuntimeManifestEnvelope = serde_json::from_slice(bytes)?;
    let expected = match envelope.kind.as_deref() {
        Some(HOSTED_DEVNET_RUNTIME_MANIFEST_KIND) => 4,
        Some(HOGD_DEV_RUNTIME_MANIFEST_KIND) | None => RUNTIME_MANIFEST_SCHEMA_VERSION,
        Some(kind) => {
            return Err(serde_json::Error::custom(format!(
                "unsupported runtime manifest kind {kind}"
            ))
            .into());
        }
    };
    if envelope.schema_version != expected {
        return Err(RuntimeManifestParseError::UnsupportedSchema {
            expected,
            actual: envelope.schema_version,
        });
    }
    let manifest: RuntimeManifest = serde_json::from_slice(bytes)?;
    if manifest.kind == HOGD_DEV_RUNTIME_MANIFEST_KIND {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        if value.get("bootstrapAccountDerivation").is_some()
            || value["bootstrapAccounts"]
                .as_array()
                .is_some_and(|accounts| {
                    accounts
                        .iter()
                        .any(|account| account.get("index").is_some())
                })
        {
            return Err(serde_json::Error::custom(
                "local schema 5 must not contain account derivation or index fields",
            )
            .into());
        }
    } else if manifest.bootstrap_account_derivation.is_none()
        || manifest
            .bootstrap_accounts
            .iter()
            .any(|account| account.index.is_none())
    {
        return Err(serde_json::Error::custom(
            "hosted schema 4 requires account derivation and indices",
        )
        .into());
    }
    Ok(manifest)
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
    /// Coordinator-facing scheduler service used by local orchestration.
    pub scheduler: SocketAddr,
    /// Public gateway-facing scheduler service, omitted when unavailable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_endpoint"
    )]
    pub scheduler_gateway: Option<SocketAddr>,
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
    /// Hosted schema-4 fixture derivation index; absent on local schema 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// Account role discriminator. Local runtimes use `faucet` for the service
    /// account and `local_user` for ordinary workload accounts.
    pub role: String,
    /// Public key authorized for the wallet.
    pub public_key: Ed25519PublicKey,
    /// Nonce-zero native pubkey account funded at genesis.
    pub wallet_address: ContractAddress,
    /// Initial storage-token balance as a decimal string.
    pub initial_storage_tokens: String,
}

/// Runtime bootstrap metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RuntimeManifestBootstrap {
    /// Concrete storage costs used at genesis.
    pub storage_cost_model: RuntimeManifestStorageCostModel,
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
