//! Bootstrap manifest types consumed by local and benchmark backend launchers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SHA256Hash;

/// Current bootstrap manifest schema version.
pub const BOOTSTRAP_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Bootstrap intent consumed by a backend launcher before genesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BootstrapManifest {
    /// Version of this JSON contract.
    pub schema_version: u32,
    /// Manifest family, for example `groundhog-bootstrap`.
    pub kind: String,
    /// Identifier the launcher should assign to the materialized runtime.
    pub network_id: String,
    /// Free-form profile name used by launcher-specific defaults.
    pub profile: String,
    /// Desired dev-wallet bootstrap artifact/profile.
    pub dev_wallet: BootstrapManifestDevWallet,
    /// Desired deterministic bootstrap account group.
    pub bootstrap_accounts: BootstrapManifestAccounts,
    /// Desired storage-cost profile.
    pub storage_cost_model: BootstrapManifestStorageCostModel,
}

impl BootstrapManifest {
    /// Construct a bootstrap manifest.
    pub fn new(
        kind: impl Into<String>,
        network_id: impl Into<String>,
        profile: impl Into<String>,
        dev_wallet: BootstrapManifestDevWallet,
        bootstrap_accounts: BootstrapManifestAccounts,
        storage_cost_model: BootstrapManifestStorageCostModel,
    ) -> Self {
        Self {
            schema_version: BOOTSTRAP_MANIFEST_SCHEMA_VERSION,
            kind: kind.into(),
            network_id: network_id.into(),
            profile: profile.into(),
            dev_wallet,
            bootstrap_accounts,
            storage_cost_model,
        }
    }
}

/// Dev-wallet bootstrap intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BootstrapManifestDevWallet {
    /// Launcher-defined dev-wallet profile, for example `hogd-dev-default`.
    pub profile: String,
    /// Optional human-readable artifact lookup hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_hint: Option<String>,
    /// Optional expected artifact byte hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<SHA256Hash>,
    /// Optional deploy entrypoint override or assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy_entrypoint: Option<String>,
    /// Optional wallet action entrypoint override or assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub do_action_entrypoint: Option<String>,
}

impl BootstrapManifestDevWallet {
    /// Construct dev-wallet intent from a launcher-defined profile.
    pub fn profile(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            artifact_hint: None,
            content_hash: None,
            deploy_entrypoint: None,
            do_action_entrypoint: None,
        }
    }
}

/// Homogeneous deterministic bootstrap account group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BootstrapManifestAccounts {
    /// Number of accounts to materialize.
    pub count: u32,
    /// Seed consumed by the selected account derivation scheme.
    pub seed: String,
    /// Initial storage-token funding per account as a decimal string.
    pub initial_storage_tokens: String,
}

impl BootstrapManifestAccounts {
    /// Construct a homogeneous deterministic bootstrap account group.
    pub fn new(
        count: u32,
        seed: impl Into<String>,
        initial_storage_tokens: impl Into<String>,
    ) -> Self {
        Self {
            count,
            seed: seed.into(),
            initial_storage_tokens: initial_storage_tokens.into(),
        }
    }
}

/// Storage-cost bootstrap intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
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

#[cfg(test)]
mod tests {
    use groundhog_xdr::types::SHA256Hash;
    use serde_json::json;

    use crate::hex::Hex0x;

    use super::{
        BOOTSTRAP_MANIFEST_SCHEMA_VERSION, BootstrapManifest, BootstrapManifestAccounts,
        BootstrapManifestDevWallet, BootstrapManifestStorageCostModel,
    };

    #[test]
    fn bootstrap_manifest_serializes_expected_shape() {
        let value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 1,
                "kind": "groundhog-bootstrap",
                "networkId": "groundhog-bench-local",
                "profile": "local-dev",
                "devWallet": {
                    "profile": "hogd-dev-default",
                    "artifactHint": "./dev-wallet.wasm",
                    "contentHash": prefixed_hex(0x51),
                    "deployEntrypoint": "0xadac498222148e4f",
                    "doActionEntrypoint": "0x646f416374696f6e",
                },
                "bootstrapAccounts": {
                    "count": 100,
                    "seed": "groundhog-bench-accounts",
                    "initialStorageTokens": "1000000000",
                },
                "storageCostModel": {
                    "profile": "hogd-default",
                },
            })
        );
    }

    #[test]
    fn optional_assertion_fields_are_omitted() {
        let value = serde_json::to_value(BootstrapManifestDevWallet {
            profile: "hogd-dev-default".into(),
            artifact_hint: None,
            content_hash: None,
            deploy_entrypoint: None,
            do_action_entrypoint: None,
        })
        .expect("bug: dev wallet serializes");

        assert_eq!(
            value,
            json!({
                "profile": "hogd-dev-default",
            })
        );
    }

    fn full_manifest() -> BootstrapManifest {
        BootstrapManifest {
            schema_version: BOOTSTRAP_MANIFEST_SCHEMA_VERSION,
            kind: "groundhog-bootstrap".into(),
            network_id: "groundhog-bench-local".into(),
            profile: "local-dev".into(),
            dev_wallet: BootstrapManifestDevWallet {
                profile: "hogd-dev-default".into(),
                artifact_hint: Some("./dev-wallet.wasm".into()),
                content_hash: Some(hash(0x51)),
                deploy_entrypoint: Some("0xadac498222148e4f".into()),
                do_action_entrypoint: Some("0x646f416374696f6e".into()),
            },
            bootstrap_accounts: BootstrapManifestAccounts {
                count: 100,
                seed: "groundhog-bench-accounts".into(),
                initial_storage_tokens: "1000000000".into(),
            },
            storage_cost_model: BootstrapManifestStorageCostModel {
                profile: "hogd-default".into(),
            },
        }
    }

    fn hash(byte: u8) -> crate::SHA256Hash {
        Hex0x(SHA256Hash::from([byte; 32]))
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
