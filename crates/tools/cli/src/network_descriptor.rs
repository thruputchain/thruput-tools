use groundhog_json_rpc_types::network_descriptor::{
    AirdropDescriptor, NETWORK_DESCRIPTOR_KIND, NETWORK_DESCRIPTOR_SCHEMA_VERSION,
    NetworkDescriptor,
};
use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
use groundhog_signing::WALLET_DEPLOY_ENTRYPOINT;
use groundhog_xdr::contract::ContractEntrypointID;

use crate::CliError;
use crate::config::ResolvedProfile;
use crate::faucet_client::{client, get_json};
use crate::runtime_manifest::load_configured_runtime_manifest;

pub(crate) const WALLET_ENTRYPOINT_SOURCE_RUNTIME_MANIFEST: &str = "runtime_manifest";
pub(crate) const WALLET_ENTRYPOINT_SOURCE_NETWORK_DESCRIPTOR: &str = "network_descriptor";
pub(crate) const WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT: &str = "legacy_default";

// Frozen compatibility selector for RPC-only saved-wallet profiles that have no
// runtime manifest or network descriptor metadata.
const LEGACY_WALLET_DO_ACTION_ENTRYPOINT: [u8; 8] = 0x1984_6D75_0FAD_81D6_u64.to_be_bytes();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WalletEntrypoints {
    pub(crate) deploy: ContractEntrypointID,
    pub(crate) do_action: ContractEntrypointID,
    pub(crate) source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAirdropDescriptor {
    pub(crate) network_id: String,
    pub(crate) genesis_hash: String,
    pub(crate) policy: AirdropDescriptor,
    pub(crate) amount_hog: String,
}

pub(crate) fn resolve_wallet_entrypoints(
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<WalletEntrypoints, CliError> {
    let runtime_manifest = load_configured_runtime_manifest(profile, key_name)?;
    let Some(faucet_url) = profile.faucet_url.as_deref() else {
        return runtime_or_legacy_entrypoints(runtime_manifest.as_ref(), key_name);
    };
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let descriptor = fetch_network_descriptor_value(faucet_url)?;
    descriptor_entrypoints(key_name, rpc_url, &descriptor, runtime_manifest.as_ref())
}

pub(crate) fn fetch_network_descriptor(
    profile: &ResolvedProfile,
    key_name: &str,
) -> Result<NetworkDescriptor, CliError> {
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| descriptor_invalid("profile.faucetUrl", "is required".into()))?;
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let descriptor = fetch_network_descriptor_value(faucet_url)?;
    let runtime_manifest = load_configured_runtime_manifest(profile, key_name)?;
    let deploy = descriptor
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|error| descriptor_invalid("devWallet.deployEntrypoint", error.to_string()))?;
    let do_action = descriptor
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| descriptor_invalid("devWallet.doActionEntrypoint", error.to_string()))?;
    validate_descriptor(
        key_name,
        rpc_url,
        &descriptor,
        runtime_manifest.as_ref(),
        deploy,
        do_action,
    )?;
    parse_positive_raw_amount(
        "walletCreate.prefundRawAmount",
        &descriptor.wallet_create.prefund_raw_amount,
    )?;
    Ok(descriptor)
}

pub(crate) fn fetch_remote_airdrop_descriptor(
    profile: &ResolvedProfile,
) -> Result<RemoteAirdropDescriptor, CliError> {
    let faucet_url = profile
        .faucet_url
        .as_deref()
        .ok_or_else(|| descriptor_invalid("profile.faucetUrl", "is required".into()))?;
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let descriptor = fetch_network_descriptor_value(faucet_url)?;
    remote_airdrop_descriptor(rpc_url, descriptor)
}

#[cfg(test)]
pub(crate) fn parse_remote_airdrop_descriptor(
    rpc_url: &str,
    value: serde_json::Value,
) -> Result<RemoteAirdropDescriptor, CliError> {
    let descriptor = parse_descriptor_value(value)?;
    remote_airdrop_descriptor(rpc_url, descriptor)
}

fn remote_airdrop_descriptor(
    rpc_url: &str,
    descriptor: NetworkDescriptor,
) -> Result<RemoteAirdropDescriptor, CliError> {
    validate_descriptor_identity(rpc_url, &descriptor)?;
    if descriptor.native_hog.decimals != crate::hog::DECIMALS {
        return Err(descriptor_invalid(
            "nativeHog.decimals",
            format!(
                "unsupported value {}; expected {}",
                descriptor.native_hog.decimals,
                crate::hog::DECIMALS
            ),
        ));
    }
    let policy = descriptor.airdrop.ok_or_else(|| {
        descriptor_invalid(
            "airdrop",
            "is required for remote airdrop; use a faucet that advertises its airdrop policy"
                .into(),
        )
    })?;
    let raw_amount = parse_positive_raw_amount("airdrop.rawAmount", &policy.raw_amount)?;

    Ok(RemoteAirdropDescriptor {
        network_id: descriptor.network_id,
        genesis_hash: format_hex(descriptor.genesis_hash.0.as_ref()),
        policy,
        amount_hog: crate::hog::format_amount(raw_amount),
    })
}

fn fetch_network_descriptor_value(faucet_url: &str) -> Result<NetworkDescriptor, CliError> {
    let descriptor_url = format!("{}/v1/network", faucet_url.trim_end_matches('/'));
    let descriptor_value: serde_json::Value = get_json(&client()?, &descriptor_url)?;
    parse_descriptor_value(descriptor_value)
}

fn parse_descriptor_value(value: serde_json::Value) -> Result<NetworkDescriptor, CliError> {
    let actual_schema = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .map_or_else(|| "<missing>".into(), |version| version.to_string());
    matches_expected(
        "schemaVersion",
        &NETWORK_DESCRIPTOR_SCHEMA_VERSION.to_string(),
        &actual_schema,
    )?;
    serde_json::from_value(value)
        .map_err(|error| descriptor_invalid("$", format!("invalid descriptor JSON: {error}")))
}

fn runtime_or_legacy_entrypoints(
    manifest: Option<&RuntimeManifest>,
    key_name: &str,
) -> Result<WalletEntrypoints, CliError> {
    let Some(manifest) = manifest else {
        return Ok(WalletEntrypoints {
            deploy: ContractEntrypointID::from(WALLET_DEPLOY_ENTRYPOINT),
            do_action: ContractEntrypointID::from(LEGACY_WALLET_DO_ACTION_ENTRYPOINT),
            source: WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT,
        });
    };
    let deploy = manifest
        .bootstrap
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|error| CliError::ManifestEntrypoint {
            key_name: key_name.to_owned(),
            field: "bootstrap.devWallet.deployEntrypoint",
            message: error.to_string(),
        })?;
    let do_action = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| CliError::ManifestEntrypoint {
            key_name: key_name.to_owned(),
            field: "bootstrap.devWallet.doActionEntrypoint",
            message: error.to_string(),
        })?;
    Ok(WalletEntrypoints {
        deploy,
        do_action,
        source: WALLET_ENTRYPOINT_SOURCE_RUNTIME_MANIFEST,
    })
}

fn descriptor_entrypoints(
    key_name: &str,
    rpc_url: &str,
    descriptor: &NetworkDescriptor,
    runtime_manifest: Option<&RuntimeManifest>,
) -> Result<WalletEntrypoints, CliError> {
    let deploy = descriptor
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|error| descriptor_invalid("devWallet.deployEntrypoint", error.to_string()))?;
    let do_action = descriptor
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| descriptor_invalid("devWallet.doActionEntrypoint", error.to_string()))?;
    validate_descriptor(
        key_name,
        rpc_url,
        descriptor,
        runtime_manifest,
        deploy,
        do_action,
    )?;
    Ok(WalletEntrypoints {
        deploy,
        do_action,
        source: WALLET_ENTRYPOINT_SOURCE_NETWORK_DESCRIPTOR,
    })
}

fn validate_descriptor(
    key_name: &str,
    rpc_url: &str,
    descriptor: &NetworkDescriptor,
    runtime_manifest: Option<&RuntimeManifest>,
    descriptor_deploy: ContractEntrypointID,
    descriptor_do_action: ContractEntrypointID,
) -> Result<(), CliError> {
    validate_descriptor_identity(rpc_url, descriptor)?;
    let Some(manifest) = runtime_manifest else {
        return Ok(());
    };
    matches_expected("protocol", &manifest.protocol, &descriptor.protocol)?;
    matches_expected(
        "genesisHash",
        &format_hex(manifest.genesis_hash.as_ref()),
        &format_hex(descriptor.genesis_hash.0.as_ref()),
    )?;
    matches_expected(
        "walletCreate.walletContractHash",
        &format_hex(manifest.bootstrap.dev_wallet.content_hash.as_ref()),
        &format_hex(descriptor.wallet_create.wallet_contract_hash.0.as_ref()),
    )?;
    let runtime_deploy = manifest
        .bootstrap
        .dev_wallet
        .deploy_entrypoint_id()
        .map_err(|error| CliError::ManifestEntrypoint {
            key_name: key_name.to_owned(),
            field: "bootstrap.devWallet.deployEntrypoint",
            message: error.to_string(),
        })?;
    let runtime_do_action = manifest
        .bootstrap
        .dev_wallet
        .do_action_entrypoint_id()
        .map_err(|error| CliError::ManifestEntrypoint {
            key_name: key_name.to_owned(),
            field: "bootstrap.devWallet.doActionEntrypoint",
            message: error.to_string(),
        })?;
    matches_expected(
        "devWallet.deployEntrypoint",
        &format_hex(runtime_deploy.as_ref()),
        &format_hex(descriptor_deploy.as_ref()),
    )?;
    matches_expected(
        "devWallet.doActionEntrypoint",
        &format_hex(runtime_do_action.as_ref()),
        &format_hex(descriptor_do_action.as_ref()),
    )?;
    Ok(())
}

fn validate_descriptor_identity(
    rpc_url: &str,
    descriptor: &NetworkDescriptor,
) -> Result<(), CliError> {
    matches_expected(
        "schemaVersion",
        &NETWORK_DESCRIPTOR_SCHEMA_VERSION.to_string(),
        &descriptor.schema_version.to_string(),
    )?;
    matches_expected("kind", NETWORK_DESCRIPTOR_KIND, &descriptor.kind)?;
    matches_expected(
        "endpoints.rpcUrl",
        &canonical_rpc_url("profile.rpcUrl", rpc_url)?,
        &canonical_rpc_url("endpoints.rpcUrl", &descriptor.endpoints.rpc_url)?,
    )?;
    if descriptor.network_id.trim().is_empty() {
        return Err(descriptor_invalid("networkId", "must not be empty".into()));
    }
    if descriptor.protocol.trim().is_empty() {
        return Err(descriptor_invalid("protocol", "must not be empty".into()));
    }
    Ok(())
}

fn parse_positive_raw_amount(field: &'static str, raw: &str) -> Result<u128, CliError> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(descriptor_invalid(
            field,
            "must be an exact unsigned raw decimal amount".into(),
        ));
    }
    match raw.parse::<u128>() {
        Ok(amount) if amount > 0 => Ok(amount),
        Ok(_) => Err(descriptor_invalid(field, "must be positive".into())),
        Err(error) => Err(descriptor_invalid(
            field,
            format!("must fit in an unsigned 128-bit raw amount: {error}"),
        )),
    }
}

fn canonical_rpc_url(field: &'static str, url: &str) -> Result<String, CliError> {
    reqwest::Url::parse(url)
        .map(|url| url.to_string())
        .map_err(|error| descriptor_invalid(field, format!("invalid URL: {error}")))
}

fn format_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn matches_expected(field: &'static str, expected: &str, actual: &str) -> Result<(), CliError> {
    if expected == actual {
        return Ok(());
    }
    Err(CliError::NetworkDescriptorMismatch {
        field,
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    })
}

fn descriptor_invalid(field: &'static str, message: String) -> CliError {
    CliError::NetworkDescriptorInvalid { field, message }
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::hex::Hex0x;

    use super::*;

    #[test]
    fn legacy_default_entrypoints_remain_frozen_without_metadata() {
        let entrypoints = runtime_or_legacy_entrypoints(None, "alice")
            .expect("bug: RPC-only profiles should retain legacy wallet compatibility");

        assert_eq!(
            entrypoints.deploy,
            ContractEntrypointID::from(0xADAC_4982_2214_8E4F_u64.to_be_bytes())
        );
        assert_eq!(
            entrypoints.do_action,
            ContractEntrypointID::from(0x1984_6D75_0FAD_81D6_u64.to_be_bytes())
        );
        assert_eq!(entrypoints.source, WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT);
    }

    #[test]
    fn hosted_descriptor_rotates_both_wallet_entrypoints() {
        let entrypoints =
            descriptor_entrypoints("alice", "https://rpc.devnet.example/", &descriptor(), None)
                .expect("bug: matching hosted descriptor should resolve");

        assert_eq!(
            entrypoints.deploy.as_ref(),
            &[0xf9, 0x69, 0x54, 0x25, 0x72, 0xbd, 0x68, 0x60]
        );
        assert_eq!(
            entrypoints.do_action.as_ref(),
            &[0xa0, 0xff, 0xf9, 0x4c, 0x64, 0xfb, 0xaf, 0x3c]
        );
        assert_eq!(
            entrypoints.source,
            WALLET_ENTRYPOINT_SOURCE_NETWORK_DESCRIPTOR
        );
    }

    #[test]
    fn hosted_descriptor_without_airdrop_policy_still_resolves_wallet_entrypoints() {
        let mut descriptor = descriptor();
        descriptor.airdrop = None;

        descriptor_entrypoints("alice", "https://rpc.devnet.example/", &descriptor, None)
            .expect("bug: airdrop policy is unrelated to deploy and call entrypoints");
    }

    #[test]
    fn remote_airdrop_rejects_zero_amount_but_allows_zero_policy_values() {
        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["airdrop"]["rawAmount"] = serde_json::json!("0");
        let error = parse_remote_airdrop_descriptor("https://rpc.devnet.example/", value)
            .expect_err("bug: zero airdrop amount should fail validation");
        assert_invalid_field(error, "airdrop.rawAmount");

        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["airdrop"]["recipientCooldownSeconds"] = serde_json::json!(0);
        value["airdrop"]["clientQuotaMax"] = serde_json::json!(0);
        value["airdrop"]["clientQuotaWindowSeconds"] = serde_json::json!(0);
        value["airdrop"]["maxPendingPerRecipient"] = serde_json::json!(0);
        parse_remote_airdrop_descriptor("https://rpc.devnet.example/", value)
            .expect("bug: zero policy values should be displayed and enforced by the faucet");
    }

    #[test]
    fn wallet_prefund_rejects_non_positive_amount() {
        let error = parse_positive_raw_amount("walletCreate.prefundRawAmount", "invalid")
            .expect_err("bug: invalid wallet prefund should fail validation");

        assert_invalid_field(error, "walletCreate.prefundRawAmount");
    }

    #[test]
    fn remote_airdrop_ignores_unrelated_wallet_semantics() {
        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["walletCreate"]["prefundRawAmount"] = serde_json::json!("not-an-amount");
        value["devWallet"]["deployEntrypoint"] = serde_json::json!("0x00");

        let descriptor = parse_remote_airdrop_descriptor("https://rpc.devnet.example/", value)
            .expect("bug: remote airdrop should validate only its required descriptor projection");

        assert_eq!(descriptor.amount_hog, "10");
    }

    #[test]
    fn remote_airdrop_rejects_unsupported_decimals_and_inexact_raw_amount() {
        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["nativeHog"]["decimals"] = serde_json::json!(6);
        let error = parse_remote_airdrop_descriptor("https://rpc.devnet.example/", value)
            .expect_err("bug: unsupported native HOG decimals should fail");
        assert_invalid_field(error, "nativeHog.decimals");

        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["airdrop"]["rawAmount"] = serde_json::json!("1.0");
        let error = parse_remote_airdrop_descriptor("https://rpc.devnet.example/", value)
            .expect_err("bug: raw amount must be an exact integer string");
        assert_invalid_field(error, "airdrop.rawAmount");
    }

    #[test]
    fn hosted_descriptor_reports_old_schema_before_missing_new_fields() {
        let mut value = serde_json::to_value(descriptor()).unwrap();
        value["schemaVersion"] = serde_json::json!(1);
        value.as_object_mut().unwrap().remove("devWallet");

        let error = parse_descriptor_value(value)
            .expect_err("bug: old descriptor schema should fail before deserialization");

        assert_mismatch_field(error, "schemaVersion");
    }

    #[test]
    fn hosted_descriptor_rejects_profile_rpc_mismatch() {
        let error =
            descriptor_entrypoints("alice", "https://other.devnet.example", &descriptor(), None)
                .expect_err("bug: mismatched RPC URL should fail");

        assert_mismatch_field(error, "endpoints.rpcUrl");
    }

    #[test]
    fn hosted_descriptor_accepts_equivalent_rpc_urls() {
        let mut descriptor = descriptor();
        descriptor.endpoints.rpc_url = "https://RPC.DEVNET.EXAMPLE:443".into();

        descriptor_entrypoints("alice", "https://rpc.devnet.example/", &descriptor, None)
            .expect("bug: equivalent RPC URLs should match");
    }

    #[test]
    fn hosted_descriptor_rejects_distinct_rpc_paths() {
        let mut descriptor = descriptor();
        descriptor.endpoints.rpc_url = "https://rpc.devnet.example/rpc".into();

        let error =
            descriptor_entrypoints("alice", "https://rpc.devnet.example/", &descriptor, None)
                .expect_err("bug: distinct RPC paths should not match");

        assert_mismatch_field(error, "endpoints.rpcUrl");
    }

    #[test]
    fn hosted_descriptor_rejects_runtime_genesis_mismatch() {
        let mut manifest = runtime_manifest();
        manifest.genesis_hash = Hex0x(groundhog_xdr::types::SHA256Hash::from([0x99; 32]));

        let error = descriptor_entrypoints(
            "alice",
            "https://rpc.devnet.example",
            &descriptor(),
            Some(&manifest),
        )
        .expect_err("bug: mismatched genesis should fail");

        assert_mismatch_field(error, "genesisHash");
    }

    #[test]
    fn hosted_descriptor_rejects_runtime_wallet_contract_mismatch() {
        let mut manifest = runtime_manifest();
        manifest.bootstrap.dev_wallet.content_hash =
            Hex0x(groundhog_xdr::types::SHA256Hash::from([0x99; 32]));

        let error = descriptor_entrypoints(
            "alice",
            "https://rpc.devnet.example",
            &descriptor(),
            Some(&manifest),
        )
        .expect_err("bug: mismatched wallet contract should fail");

        assert_mismatch_field(error, "walletCreate.walletContractHash");
    }

    #[test]
    fn hosted_descriptor_rejects_runtime_entrypoint_mismatch() {
        let mut manifest = runtime_manifest();
        manifest.bootstrap.dev_wallet.do_action_entrypoint = "0x1111111111111111".into();

        let error = descriptor_entrypoints(
            "alice",
            "https://rpc.devnet.example",
            &descriptor(),
            Some(&manifest),
        )
        .expect_err("bug: mismatched wallet entrypoint should fail");

        assert_mismatch_field(error, "devWallet.doActionEntrypoint");
    }

    #[test]
    fn hosted_descriptor_compares_runtime_entrypoint_bytes_not_text() {
        let mut manifest = runtime_manifest();
        manifest.bootstrap.dev_wallet.deploy_entrypoint = "F969542572BD6860".into();
        manifest.bootstrap.dev_wallet.do_action_entrypoint = "0xA0FFF94C64FBAF3C".into();

        descriptor_entrypoints(
            "alice",
            "https://rpc.devnet.example",
            &descriptor(),
            Some(&manifest),
        )
        .expect("bug: equivalent entrypoint bytes should match");
    }

    #[test]
    fn hosted_descriptor_rejects_malformed_entrypoint_before_runtime_comparison() {
        let mut descriptor = descriptor();
        descriptor.dev_wallet.deploy_entrypoint.0 = vec![0xf9; 7];

        let error = descriptor_entrypoints(
            "alice",
            "https://rpc.devnet.example",
            &descriptor,
            Some(&runtime_manifest()),
        )
        .expect_err("bug: malformed descriptor entrypoint should fail");

        match error {
            CliError::NetworkDescriptorInvalid { field, .. } => {
                assert_eq!(field, "devWallet.deployEntrypoint");
            }
            other => panic!("bug: expected invalid descriptor, got {other}"),
        }
    }

    fn assert_mismatch_field(error: CliError, expected_field: &str) {
        match error {
            CliError::NetworkDescriptorMismatch { field, .. } => {
                assert_eq!(field, expected_field);
            }
            other => panic!("bug: expected descriptor mismatch, got {other}"),
        }
    }

    fn assert_invalid_field(error: CliError, expected_field: &str) {
        match error {
            CliError::NetworkDescriptorInvalid { field, .. } => assert_eq!(field, expected_field),
            other => panic!("bug: expected invalid descriptor, got {other}"),
        }
    }

    fn descriptor() -> NetworkDescriptor {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": NETWORK_DESCRIPTOR_SCHEMA_VERSION,
            "kind": NETWORK_DESCRIPTOR_KIND,
            "networkId": "devnet",
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "endpoints": { "rpcUrl": "https://rpc.devnet.example" },
            "nativeHog": {
                "tokenAddress": format!("0x{}", "61".repeat(32)),
                "decimals": 7,
                "transferEntrypoint": "0x63285b3523f9dc3f"
            },
            "faucet": { "walletAddress": format!("0x{}", "71".repeat(32)) },
            "airdrop": {
                "rawAmount": "100000000",
                "recipientCooldownSeconds": 43200,
                "clientQuotaMax": 10,
                "clientQuotaWindowSeconds": 3600,
                "maxPendingPerRecipient": 1
            },
            "walletCreate": {
                "prefundRawAmount": "100000000",
                "walletContractHash": format!("0x{}", "51".repeat(32))
            },
            "devWallet": {
                "deployEntrypoint": "0xf969542572bd6860",
                "doActionEntrypoint": "0xa0fff94c64fbaf3c"
            }
        }))
        .expect("bug: descriptor fixture should deserialize")
    }

    fn runtime_manifest() -> RuntimeManifest {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "42".repeat(32),
            "protocol": "hogp/test",
            "genesisHash": format!("0x{}", "42".repeat(32)),
            "producer": { "binary": "hogd", "version": "0.1.0" },
            "endpoints": { "scheduler": "127.0.0.1:1000" },
            "bootstrapAccountDerivation": {
                "scheme": "groundhog-bootstrap-account-v1",
                "seed": "test"
            },
            "bootstrapAccounts": [],
            "bootstrap": {
                "devWallet": {
                    "contentHash": format!("0x{}", "51".repeat(32)),
                    "deployedCodeHash": format!("0x{}", "52".repeat(32)),
                    "artifactHint": "./dev_wallet.rv32a",
                    "deployEntrypoint": "0xf969542572bd6860",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0xa0fff94c64fbaf3c"
                },
                "storageCostModel": {
                    "hash": format!("0x{}", "53".repeat(32)),
                    "baseDataItemCost": "12000",
                    "bytestringCostPerByte": "10",
                    "baseContractDeploymentCost": "12000",
                    "contractDeploymentCostPerByte": "10",
                    "authPerKeyCost": "1111",
                    "rcqCostPerEntry": "8",
                    "replayCacheCostPerEntry": "7",
                    "balanceCost": "1234"
                }
            }
        }))
        .expect("bug: runtime manifest fixture should deserialize")
    }
}
