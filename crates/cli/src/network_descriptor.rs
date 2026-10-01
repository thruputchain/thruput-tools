use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_json_rpc_types::hex::prefixed_hex;
use groundhog_json_rpc_types::network_descriptor::{
    AirdropDescriptor, NETWORK_DESCRIPTOR_KIND, NETWORK_DESCRIPTOR_SCHEMA_VERSION,
    NetworkDescriptor,
};
use groundhog_json_rpc_types::network_info::NetworkInfo;
use groundhog_signing::WALLET_DEPLOY_ENTRYPOINT;
use groundhog_xdr::contract::ContractEntrypointID;

use crate::CliError;
use crate::config::ResolvedProfile;
use crate::faucet_client::{client, get_json};

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
pub(crate) struct WalletSigningContext {
    pub(crate) entrypoints: WalletEntrypoints,
    pub(crate) network_hash: groundhog_xdr::types::NetworkHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteAirdropDescriptor {
    pub(crate) network_id: String,
    pub(crate) genesis_hash: String,
    pub(crate) policy: AirdropDescriptor,
    pub(crate) amount_hog: String,
}

pub(crate) fn resolve_native_signing_context(
    profile: &ResolvedProfile,
    configured: &groundhog_xdr::types::NetworkHash,
    runtime: &tokio::runtime::Runtime,
) -> Result<WalletSigningContext, CliError> {
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let network_info = fetch_rpc_network_info(runtime, rpc_url)?;
    Ok(WalletSigningContext {
        network_hash: validated_signing_network_hash(configured, &network_info)?,
        entrypoints: WalletEntrypoints {
            deploy: groundhog_signing::default_wallet::DEFAULT_WALLET_DEPLOY_ENTRYPOINT,
            do_action: groundhog_signing::default_wallet::DEFAULT_WALLET_EXECUTE_ENTRYPOINT,
            source: "native_default_wallet",
        },
    })
}

pub(crate) fn resolve_wallet_signing_context(
    profile: &ResolvedProfile,
    _key_name: &str,
    configured_network_hash: groundhog_xdr::types::NetworkHash,
    runtime: &tokio::runtime::Runtime,
) -> Result<WalletSigningContext, CliError> {
    let Some(faucet_url) = profile.faucet_url.as_deref() else {
        return Ok(WalletSigningContext {
            entrypoints: legacy_entrypoints(),
            network_hash: configured_network_hash,
        });
    };
    let rpc_url = profile.rpc_url.as_deref().ok_or(CliError::MissingRpcUrl)?;
    let descriptor = fetch_network_descriptor_value(faucet_url)?;
    let network_info = fetch_rpc_network_info(runtime, rpc_url)?;
    validate_network_info(&descriptor, &network_info)?;
    validate_descriptor_identity(rpc_url, &descriptor)?;
    let entrypoints = legacy_entrypoints();
    let network_hash = validated_signing_network_hash(&configured_network_hash, &network_info)?;
    Ok(WalletSigningContext {
        entrypoints,
        network_hash,
    })
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
    validate_rpc_network_info(rpc_url, &descriptor)?;
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
        genesis_hash: prefixed_hex(descriptor.genesis_hash.0.as_ref()),
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

// Saved legacy wallet bindings use the canonical deployed-wallet ABI independently
// of the native bootstrap account metadata.
fn legacy_entrypoints() -> WalletEntrypoints {
    WalletEntrypoints {
        deploy: ContractEntrypointID::from(WALLET_DEPLOY_ENTRYPOINT),
        do_action: ContractEntrypointID::from(LEGACY_WALLET_DO_ACTION_ENTRYPOINT),
        source: WALLET_ENTRYPOINT_SOURCE_LEGACY_DEFAULT,
    }
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

fn validate_rpc_network_info(
    rpc_url: &str,
    descriptor: &NetworkDescriptor,
) -> Result<(), CliError> {
    let runtime =
        tokio::runtime::Runtime::new().map_err(|error| CliError::RuntimeInit(error.to_string()))?;
    let network_info = fetch_rpc_network_info(&runtime, rpc_url)?;
    validate_network_info(descriptor, &network_info)
}

fn fetch_rpc_network_info(
    runtime: &tokio::runtime::Runtime,
    rpc_url: &str,
) -> Result<NetworkInfo, CliError> {
    runtime.block_on(async {
        crate::http_client::rpc_client(rpc_url)?
            .network_info()
            .await
            .map_err(|error| CliError::RpcMethodFailed {
                method: "networkInfo",
                message: error.to_string(),
            })
    })
}

fn validate_network_info(
    descriptor: &NetworkDescriptor,
    network_info: &NetworkInfo,
) -> Result<(), CliError> {
    matches_expected(
        "networkHash",
        &prefixed_hex(network_info.network_hash.0.as_ref()),
        &prefixed_hex(descriptor.network_hash.0.as_ref()),
    )
}

fn validated_signing_network_hash(
    configured: &groundhog_xdr::types::NetworkHash,
    network_info: &NetworkInfo,
) -> Result<groundhog_xdr::types::NetworkHash, CliError> {
    matches_expected(
        "networkHash",
        &prefixed_hex(configured.as_ref()),
        &prefixed_hex(network_info.network_hash.0.as_ref()),
    )?;
    Ok(groundhog_xdr::types::NetworkHash::from(
        network_info.network_hash.0,
    ))
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

    use super::*;

    #[test]
    fn legacy_default_entrypoints_remain_frozen_without_metadata() {
        let entrypoints = legacy_entrypoints();

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
        value.as_object_mut().unwrap().remove("nativeHog");

        let error = parse_descriptor_value(value)
            .expect_err("bug: old descriptor schema should fail before deserialization");

        assert_mismatch_field(error, "schemaVersion");
    }

    #[test]
    fn hosted_descriptor_rejects_profile_rpc_mismatch() {
        let error = validate_descriptor_identity("https://other.devnet.example", &descriptor())
            .expect_err("bug: mismatched RPC URL should fail");

        assert_mismatch_field(error, "endpoints.rpcUrl");
    }

    #[test]
    fn hosted_descriptor_accepts_equivalent_rpc_urls() {
        let mut descriptor = descriptor();
        descriptor.endpoints.rpc_url = "https://RPC.DEVNET.EXAMPLE:443".into();

        validate_descriptor_identity("https://rpc.devnet.example/", &descriptor)
            .expect("bug: equivalent RPC URLs should match");
    }

    #[test]
    fn hosted_descriptor_rejects_distinct_rpc_paths() {
        let mut descriptor = descriptor();
        descriptor.endpoints.rpc_url = "https://rpc.devnet.example/rpc".into();

        let error = validate_descriptor_identity("https://rpc.devnet.example/", &descriptor)
            .expect_err("bug: distinct RPC paths should not match");

        assert_mismatch_field(error, "endpoints.rpcUrl");
    }

    #[test]
    fn hosted_descriptor_rejects_rpc_network_hash_mismatch() {
        let descriptor = descriptor();
        let network_info = NetworkInfo::new(groundhog_json_rpc_types::NetworkHash::from(
            groundhog_xdr::types::SHA256Hash::from([0x99; 32]),
        ));

        let error = validate_network_info(&descriptor, &network_info)
            .expect_err("bug: RPC and faucet network hashes must match");

        assert_mismatch_field(error, "networkHash");
    }

    #[test]
    fn hosted_signing_returns_the_validated_rpc_network_hash() {
        let descriptor = descriptor();
        let configured = groundhog_xdr::types::NetworkHash::from(descriptor.network_hash.0);
        let network_info = NetworkInfo::new(descriptor.network_hash);

        let network_hash = validated_signing_network_hash(&configured, &network_info)
            .expect("bug: matching signing identity should resolve");

        assert_eq!(network_hash.as_ref(), &[0x42; 32]);
    }

    #[test]
    fn hosted_signing_rejects_configured_network_hash_mismatch() {
        let descriptor = descriptor();
        let configured = groundhog_xdr::types::NetworkHash::from(
            groundhog_xdr::types::SHA256Hash::from([0x99; 32]),
        );
        let network_info = NetworkInfo::new(descriptor.network_hash);

        let error = validated_signing_network_hash(&configured, &network_info)
            .expect_err("bug: stale signing identity should fail before signing");

        assert_mismatch_field(error, "networkHash");
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
            "networkHash": format!("0x{}", "42".repeat(32)),
            "protocol": "hogp/test00000000000",
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
            }
        }))
        .expect("bug: descriptor fixture should deserialize")
    }
}
