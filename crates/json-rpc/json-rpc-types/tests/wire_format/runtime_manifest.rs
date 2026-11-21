use serde_json::json;
use thruput_json_rpc_types::runtime_manifest::{
    BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BootstrapAccount, HOSTED_DEVNET_RUNTIME_MANIFEST_KIND,
    RUNTIME_MANIFEST_SCHEMA_VERSION, RuntimeManifest, RuntimeManifestChainState,
    RuntimeManifestEndpoints, RuntimeManifestParseError, parse_runtime_manifest,
};

#[test]
fn old_runtime_schema_is_rejected_before_native_account_interpretation() {
    let mut value = full_manifest();
    value["schemaVersion"] = serde_json::json!(2);
    assert!(matches!(
        parse_runtime_manifest(&serde_json::to_vec(&value).unwrap()),
        Err(RuntimeManifestParseError::UnsupportedSchema {
            expected: 5,
            actual: 2
        })
    ));
}

#[test]
fn runtime_manifest_roundtrips_the_wire_fields() {
    let wire = full_manifest();
    let manifest = parse_runtime_manifest(&serde_json::to_vec(&wire).unwrap()).unwrap();
    assert_eq!(manifest.chain_state, Some(RuntimeManifestChainState::Fresh));
    assert_eq!(manifest.network_hash, [0x42; 32].into());
    assert_eq!(manifest.producer.binary, "hogd-dev");
    let endpoints = &manifest.endpoints;
    assert_eq!(endpoints.scheduler.port(), 1000);
    assert_eq!(endpoints.scheduler_gateway.unwrap().port(), 1010);
    assert_eq!(endpoints.indexer.unwrap().port(), 1001);
    assert_eq!(endpoints.db_gateway.unwrap().port(), 1002);
    assert_eq!(endpoints.view_gateway.unwrap().port(), 1003);
    let account = &manifest.bootstrap_accounts[0];
    assert_eq!(account.public_key.to_string(), base58_text(0x61));
    assert_eq!(account.wallet_address.to_string(), base58_text(0x71));
    assert_eq!(serde_json::to_value(manifest).unwrap(), wire);
}

#[test]
fn runtime_manifest_rejects_invalid_network_hash() {
    let mut value = full_manifest();
    value["networkHash"] = json!("groundhog-local");

    let error = serde_json::from_value::<RuntimeManifest>(value)
        .expect_err("invalid network hash should fail");

    assert!(
        error
            .to_string()
            .contains("networkHash must be base58 encoding of exactly 32 bytes")
    );
}

#[test]
fn runtime_manifest_rejects_noncanonical_network_hash() {
    let mut value = full_manifest();
    value["networkHash"] = json!("AA".repeat(32));

    let error = serde_json::from_value::<RuntimeManifest>(value)
        .expect_err("legacy hex network hash should fail");

    assert!(error.to_string().contains("base58 encoding"));
}

#[test]
fn runtime_manifest_rejects_unknown_chain_state() {
    let mut value = full_manifest();
    value["chainState"] = json!("existing_data_dir");

    let error = serde_json::from_value::<RuntimeManifest>(value)
        .expect_err("unknown chain state should fail");

    assert!(error.to_string().contains("unknown variant"));
}

#[test]
fn runtime_manifest_allows_missing_additive_fields() {
    let mut value = full_manifest();
    {
        let object = value.as_object_mut().expect("bug: manifest is an object");
        object.remove("chainState");
        object
            .get_mut("endpoints")
            .and_then(serde_json::Value::as_object_mut)
            .expect("bug: endpoints is an object")
            .remove("schedulerGateway");
    }

    let manifest = serde_json::from_value::<RuntimeManifest>(value)
        .expect("the runtime schema should allow a missing additive field");

    assert_eq!(manifest.chain_state, None);
    assert_eq!(manifest.endpoints.scheduler_gateway, None);
}

#[test]
fn runtime_manifest_parser_rejects_schema_before_decoding_body() {
    let bytes = br#"{"schemaVersion":99,"futureBody":true}"#;

    let error = parse_runtime_manifest(bytes)
        .expect_err("unsupported schema should fail before body decoding");

    assert!(matches!(
        error,
        RuntimeManifestParseError::UnsupportedSchema {
            expected: RUNTIME_MANIFEST_SCHEMA_VERSION,
            actual: 99,
        }
    ));
}

#[test]
fn hosted_four_is_supported_but_local_four_is_rejected() {
    let mut value = full_manifest();
    value["schemaVersion"] = json!(4);
    assert!(parse_runtime_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
    value["kind"] = json!(HOSTED_DEVNET_RUNTIME_MANIFEST_KIND);
    value["bootstrapAccountDerivation"] =
        json!({"scheme":"thruput-bootstrap-account-v1","seed":"hosted-fixture"});
    value["bootstrapAccounts"][0]["index"] = json!(0);
    assert!(parse_runtime_manifest(&serde_json::to_vec(&value).unwrap()).is_ok());
    value["schemaVersion"] = json!(5);
    assert!(parse_runtime_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn local_five_rejects_legacy_derivation_even_null() {
    for field in ["bootstrapAccountDerivation", "index"] {
        let mut value = full_manifest();
        if field == "index" {
            value["bootstrapAccounts"][0][field] = json!(null);
        } else {
            value[field] = json!(null);
        }
        assert!(parse_runtime_manifest(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn runtime_manifest_rejects_duplicate_known_fields() {
    let original = serde_json::to_string(&full_manifest()).unwrap();
    for (needle, replacement) in [
        (
            "\"schemaVersion\":5",
            "\"schemaVersion\":5,\"schemaVersion\":5",
        ),
        (
            "\"kind\":\"hogd-dev-runtime\"",
            "\"kind\":\"hogd-dev-runtime\",\"kind\":\"hogd-dev-runtime\"",
        ),
        (
            "\"role\":\"faucet\"",
            "\"role\":\"faucet\",\"role\":\"faucet\"",
        ),
    ] {
        let duplicate = original.replace(needle, replacement);
        assert_ne!(duplicate, original);
        let error = parse_runtime_manifest(duplicate.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("duplicate field"), "{error}");
    }
}

fn full_manifest() -> serde_json::Value {
    json!({
        "schemaVersion": 5,
        "kind": "hogd-dev-runtime",
        "chainState": "fresh",
        "networkHash": base58_text(0x42),
        "protocol": "hogp/test00000000000",
        "producer": {
            "binary": "hogd-dev",
            "version": "0.1.0",
            "commitHash": "abcdef0",
        },
        "endpoints": {
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
            "indexer": "127.0.0.1:1001",
            "dbGateway": "127.0.0.1:1002",
            "viewGateway": "127.0.0.1:1003",
        },
        "bootstrapAccounts": [{
            "role": BOOTSTRAP_ACCOUNT_ROLE_FAUCET,
            "publicKey": base58_text(0x61),
            "walletAddress": base58_text(0x71),
            "initialStorageTokens": "10000000000000",
        }],
        "bootstrap": {
            "storageCostModel": {
                "hash": base58_text(0x53),
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
}

#[test]
fn scheduler_only_manifest_omits_optional_endpoints() {
    let endpoints: RuntimeManifestEndpoints = serde_json::from_value(json!({
        "scheduler": "127.0.0.1:1000", "schedulerGateway": "127.0.0.1:1010",
    }))
    .unwrap();
    let value = serde_json::to_value(endpoints).unwrap();
    assert_eq!(
        value,
        json!({
            "scheduler": "127.0.0.1:1000",
            "schedulerGateway": "127.0.0.1:1010",
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
        "publicKey": base58_text(0x61),
        "walletAddress": base58_text(0x71),
        "initialStorageTokens": "1000000000",
        "producerAddedField": "ignored",
    }))
    .expect("bug: additive manifest fields should be ignored");

    assert_eq!(account.index, Some(0));
}

fn base58_text(byte: u8) -> String {
    thruput_encoding::encode(&[byte; 32])
}
