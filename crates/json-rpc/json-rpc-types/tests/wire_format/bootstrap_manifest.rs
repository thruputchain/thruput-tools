use serde_json::json;

use thruput_json_rpc_types::bootstrap_manifest::{
    BootstrapManifest, BootstrapManifestAllocation, BootstrapManifestStorageCostModel,
};
use thruput_json_rpc_types::{NetworkHashText, base58::Base58};

#[test]
fn bootstrap_manifest_serializes_expected_shape() {
    let value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");

    assert_eq!(
        value,
        json!({
            "schemaVersion": 5,
            "kind": "groundhog-bootstrap",
            "networkHash": thruput_encoding::encode(&[0x51; 32]),
            "profile": "local-dev",
            "bootstrapFaucet": {
                "publicKey": thruput_encoding::encode(&[0x11; 32]),
                "initialStorageTokens": "10000000000000",
            },
            "bootstrapAccounts": [{
                "publicKey": thruput_encoding::encode(&[0x22; 32]),
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
            .contains("networkHash must be base58 encoding of exactly 32 bytes")
    );
}

#[test]
fn bootstrap_manifest_rejects_noncanonical_network_hash() {
    let mut value = serde_json::to_value(full_manifest()).expect("bug: manifest serializes");
    value["networkHash"] = json!("AA".repeat(32));

    let error = serde_json::from_value::<BootstrapManifest>(value)
        .expect_err("legacy hex network hash should fail");

    assert!(error.to_string().contains("base58 encoding"));
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
    let allocation = |byte, amount| {
        BootstrapManifestAllocation::new(
            Base58(thruput_xdr::types::Ed25519PublicKey::from([byte; 32])),
            amount,
        )
    };
    BootstrapManifest::new(
        "groundhog-bootstrap",
        NetworkHashText::from([0x51; 32]),
        "local-dev",
        allocation(0x11, "10000000000000"),
        vec![allocation(0x22, "1000000000")],
        BootstrapManifestStorageCostModel::profile("hogd-default"),
    )
}
