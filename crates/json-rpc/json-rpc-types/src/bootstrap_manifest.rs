//! Bootstrap manifest types consumed by local and benchmark backend launchers.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Ed25519PublicKey, NetworkHashText};

/// Current bootstrap manifest schema version.
pub const BOOTSTRAP_MANIFEST_SCHEMA_VERSION: u32 = 5;

/// Bootstrap intent consumed by a backend launcher before genesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct BootstrapManifest {
    /// Version of this JSON contract.
    pub schema_version: u32,
    /// Manifest family, for example `groundhog-bootstrap`.
    pub kind: String,
    /// Unprefixed 32-byte network hash stamped into proposed block headers.
    pub network_hash: NetworkHashText,
    /// Free-form profile name used by launcher-specific defaults.
    pub profile: String,
    /// Public allocation for the supervisor-owned faucet.
    pub bootstrap_faucet: BootstrapManifestAllocation,
    /// Explicit nonce-zero native account allocations.
    pub bootstrap_accounts: Vec<BootstrapManifestAllocation>,
    /// Desired storage-cost profile.
    pub storage_cost_model: BootstrapManifestStorageCostModel,
}

impl BootstrapManifest {
    /// Construct a bootstrap manifest.
    pub fn new(
        kind: impl Into<String>,
        network_hash: NetworkHashText,
        profile: impl Into<String>,
        bootstrap_faucet: BootstrapManifestAllocation,
        bootstrap_accounts: Vec<BootstrapManifestAllocation>,
        storage_cost_model: BootstrapManifestStorageCostModel,
    ) -> Self {
        Self {
            schema_version: BOOTSTRAP_MANIFEST_SCHEMA_VERSION,
            kind: kind.into(),
            network_hash,
            profile: profile.into(),
            bootstrap_faucet,
            bootstrap_accounts,
            storage_cost_model,
        }
    }
}

/// Public intent for a nonce-zero native account. Private signers remain with callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct BootstrapManifestAllocation {
    /// Public key authorized to sign for the account.
    pub public_key: Ed25519PublicKey,
    /// Positive canonical decimal allocation in raw native token units.
    #[serde(deserialize_with = "positive_amount")]
    pub initial_storage_tokens: String,
}

impl BootstrapManifestAllocation {
    /// Construct an explicit public allocation.
    pub fn new(public_key: Ed25519PublicKey, initial_storage_tokens: impl Into<String>) -> Self {
        Self {
            public_key,
            initial_storage_tokens: initial_storage_tokens.into(),
        }
    }
}

fn positive_amount<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let text = String::deserialize(deserializer)?;
    match text.parse::<i64>() {
        Ok(amount) if amount > 0 && amount.to_string() == text => Ok(text),
        _ => Err(serde::de::Error::custom(
            "initialStorageTokens must be a positive canonical decimal amount",
        )),
    }
}

/// Storage-cost bootstrap intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[non_exhaustive]
pub struct BootstrapManifestStorageCostModel {
    /// Launcher-defined storage-cost profile, for example `hogd-default`.
    pub profile: String,
}

impl BootstrapManifestStorageCostModel {
    /// Construct storage-cost intent from a launcher-defined profile.
    pub fn profile(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
        }
    }
}
