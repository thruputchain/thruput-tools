//! Bootstrap manifest types consumed by local and benchmark backend launchers.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Ed25519PublicKey, NetworkHashHex};

/// Current bootstrap manifest schema version.
pub const BOOTSTRAP_MANIFEST_SCHEMA_VERSION: u32 = 4;

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
    pub network_hash: NetworkHashHex,
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
        network_hash: NetworkHashHex,
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
    #[serde(deserialize_with = "canonical_public_key")]
    pub public_key: Ed25519PublicKey,
    /// Positive canonical decimal allocation in raw HOG units.
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

fn canonical_public_key<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Ed25519PublicKey, D::Error> {
    let text = String::deserialize(deserializer)?;
    if text.len() != 66
        || !text.starts_with("0x")
        || !text[2..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(serde::de::Error::custom(
            "publicKey must be 0x-prefixed lowercase 32-byte hex",
        ));
    }
    serde_json::from_value(serde_json::Value::String(text)).map_err(serde::de::Error::custom)
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        BOOTSTRAP_MANIFEST_SCHEMA_VERSION, BootstrapManifest, BootstrapManifestAllocation,
        BootstrapManifestStorageCostModel,
    };

    #[test]
    fn bootstrap_manifest_serializes_expected_shape() {
        let value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");

        assert_eq!(
            value,
            json!({
                "schemaVersion": 4,
                "kind": "groundhog-bootstrap",
                "networkHash": "5151515151515151515151515151515151515151515151515151515151515151",
                "profile": "local-dev",
                "bootstrapFaucet": {
                    "publicKey": "0x1111111111111111111111111111111111111111111111111111111111111111",
                    "initialStorageTokens": "10000000000000",
                },
                "bootstrapAccounts": [{
                    "publicKey": "0x2222222222222222222222222222222222222222222222222222222222222222",
                    "initialStorageTokens": "1000000000",
                }],
                "storageCostModel": {
                    "profile": "hogd-default",
                },
            })
        );
    }

    #[test]
    fn bootstrap_manifest_rejects_invalid_network_hash() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value["networkHash"] = json!("groundhog-local");

        let error = serde_json::from_value::<BootstrapManifest>(value)
            .expect_err("invalid network hash should fail");

        assert!(
            error
                .to_string()
                .contains("networkHash must be exactly 64 lowercase hexadecimal characters")
        );
    }

    #[test]
    fn bootstrap_manifest_rejects_noncanonical_network_hash() {
        let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
        value["networkHash"] = json!("AA".repeat(32));

        let error = serde_json::from_value::<BootstrapManifest>(value)
            .expect_err("uppercase network hash should fail");

        assert!(error.to_string().contains("lowercase hexadecimal"));
    }

    #[test]
    fn bootstrap_allocations_reject_legacy_and_noncanonical_fields() {
        for (field, value) in [
            ("seed", json!("old")),
            ("index", json!(0)),
            ("publicKey", json!("0xAA")),
            ("initialStorageTokens", json!("01")),
            ("initialStorageTokens", json!("0")),
        ] {
            let mut allocation = serde_json::to_value(full_manifest().bootstrap_faucet).unwrap();
            allocation[field] = value;
            assert!(
                serde_json::from_value::<BootstrapManifestAllocation>(allocation).is_err(),
                "{field}"
            );
        }
    }

    fn full_manifest() -> BootstrapManifest {
        BootstrapManifest {
            schema_version: BOOTSTRAP_MANIFEST_SCHEMA_VERSION,
            kind: "groundhog-bootstrap".into(),
            network_hash: crate::NetworkHashHex::from([0x51; 32]),
            profile: "local-dev".into(),
            bootstrap_faucet: BootstrapManifestAllocation::new(
                crate::hex::Hex0x(thruput_xdr::types::Ed25519PublicKey::from([0x11; 32])),
                "10000000000000",
            ),
            bootstrap_accounts: vec![BootstrapManifestAllocation::new(
                crate::hex::Hex0x(thruput_xdr::types::Ed25519PublicKey::from([0x22; 32])),
                "1000000000",
            )],
            storage_cost_model: BootstrapManifestStorageCostModel {
                profile: "hogd-default".into(),
            },
        }
    }
}
