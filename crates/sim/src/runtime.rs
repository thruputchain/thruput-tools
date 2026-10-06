use std::{future::Future, path::Path, time::Duration};

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::{
    NetworkHashHex,
    protocol_token::protocol_text,
    runtime_manifest::{
        BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, BootstrapAccount,
        RuntimeManifest, RuntimeManifestParseError, parse_runtime_manifest,
    },
};
use groundhog_signing::{LocalSigner, derive_dev_account_signer};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct SimAccount {
    pub index: u32,
    pub signer: LocalSigner,
    pub wallet_address: groundhog_xdr::types::ContractAddress,
    /// Network this account signs for. Part of every signature preimage.
    pub network_hash: groundhog_xdr::types::NetworkHash,
}

#[derive(Debug, Clone)]
pub struct LoadedRuntimeManifest {
    pub manifest: RuntimeManifest,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub config_sha256: String,
}

pub async fn load_runtime_manifest(
    path: &Path,
) -> Result<LoadedRuntimeManifest, RuntimeManifestError> {
    let path_display = path.display().to_string();
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|source| RuntimeManifestError::Read {
            path: path_display.clone(),
            source,
        })?;
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_RUNTIME_MANIFEST_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| RuntimeManifestError::Read {
            path: path_display.clone(),
            source,
        })?;
    if bytes.len() as u64 > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(RuntimeManifestError::TooLarge {
            path: path_display,
            max_bytes: MAX_RUNTIME_MANIFEST_BYTES,
        });
    }
    let manifest = parse_runtime_manifest(&bytes).map_err(|error| match error {
        RuntimeManifestParseError::Json(source) => RuntimeManifestError::Parse {
            path: path_display.clone(),
            source,
        },
        RuntimeManifestParseError::UnsupportedSchema { actual, .. } => {
            RuntimeManifestError::UnsupportedSchema { actual }
        }
    })?;

    let sha256 = format!("0x{}", hex::encode(Sha256::digest(&bytes)));
    let config_sha256 =
        runtime_config_sha256(&bytes).map_err(|source| RuntimeManifestError::Parse {
            path: path_display,
            source,
        })?;
    Ok(LoadedRuntimeManifest {
        manifest,
        bytes,
        sha256,
        config_sha256,
    })
}

pub(crate) async fn preflight_chain_identity(
    manifest: &RuntimeManifest,
    rpc_url: &str,
    request_timeout: Duration,
) -> Result<(), RuntimeManifestError> {
    // This checks this client's wire compatibility, not remotely observed server
    // protocol: RC2 networkInfo exposes only networkHash, and chainTip only
    // height/blockHash. Keep the distinction explicit until that API changes.
    let compiled_protocol = protocol_text(&groundhog_xdr::PROTOCOL_VERSION);
    if manifest.protocol != compiled_protocol {
        return Err(RuntimeManifestError::ClientProtocolMismatch {
            expected: manifest.protocol.clone(),
            compiled: compiled_protocol,
        });
    }
    let public_client = HttpClientBuilder::default()
        .request_timeout(request_timeout)
        .build(rpc_url)
        .map_err(|error| RuntimeManifestError::PublicRpcClient {
            rpc_url: rpc_url.to_string(),
            error: error.to_string(),
        })?;
    let info = preflight_timeout(
        request_timeout,
        GroundhogApiClient::network_info(&public_client),
    )
    .await
    .map_err(|_| RuntimeManifestError::PublicNetworkInfoTimeout {
        rpc_url: rpc_url.to_string(),
        timeout: request_timeout,
    })?
    .map_err(|error| RuntimeManifestError::PublicNetworkInfo {
        rpc_url: rpc_url.to_string(),
        error: error.to_string(),
    })?;
    let mut observed_bytes = [0_u8; 32];
    observed_bytes.copy_from_slice(info.network_hash.into_inner().as_ref());
    let observed = NetworkHashHex::from(observed_bytes);
    if manifest.network_hash != observed {
        return Err(RuntimeManifestError::NetworkHashMismatch {
            expected: manifest.network_hash.clone(),
            observed,
        });
    }
    preflight_timeout(
        request_timeout,
        GroundhogApiClient::chain_tip(&public_client),
    )
    .await
    .map_err(|_| RuntimeManifestError::PublicChainTipTimeout {
        rpc_url: rpc_url.to_string(),
        timeout: request_timeout,
    })?
    .map_err(|error| RuntimeManifestError::PublicChainTip {
        rpc_url: rpc_url.to_string(),
        error: error.to_string(),
    })?;
    Ok(())
}

async fn preflight_timeout<T>(
    timeout: Duration,
    future: impl Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, future).await
}

/// Hash runtime configuration without run-assigned backend dial addresses.
///
/// Endpoint presence remains part of the hash because it describes runtime
/// capabilities, while concrete addresses are execution-local discovery data.
pub fn runtime_config_sha256(bytes: &[u8]) -> Result<String, serde_json::Error> {
    let mut config: serde_json::Value = serde_json::from_slice(bytes)?;
    if let Some(endpoints) = config
        .get_mut("endpoints")
        .and_then(serde_json::Value::as_object_mut)
    {
        for endpoint in endpoints.values_mut() {
            if !endpoint.is_null() {
                *endpoint = serde_json::Value::String("0.0.0.0:0".into());
            }
        }
    }
    Ok(format!(
        "0x{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&config)?))
    ))
}

pub fn derive_accounts(
    manifest: &RuntimeManifest,
) -> Result<Vec<SimAccount>, RuntimeManifestError> {
    hosted_derivation(manifest)?;
    manifest
        .bootstrap_accounts
        .iter()
        .filter(|account| account.role == BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER)
        .map(|account| derive_account(manifest, account))
        .collect()
}

/// Resolve and verify the index-0 bootstrap faucet used to sponsor actor
/// provisioning. The returned account is external to the workload actors
/// returned by [`derive_accounts`].
pub fn derive_provisioning_sponsor(
    manifest: &RuntimeManifest,
) -> Result<SimAccount, RuntimeManifestError> {
    let account = manifest
        .bootstrap_accounts
        .iter()
        .find(|account| account.index == Some(0) && account.role == BOOTSTRAP_ACCOUNT_ROLE_FAUCET)
        .ok_or(RuntimeManifestError::MissingProvisioningFaucet)?;
    derive_account(manifest, account)
}

pub fn derive_provisioned_account_signer(
    manifest: &RuntimeManifest,
    index: u32,
) -> Result<LocalSigner, RuntimeManifestError> {
    let derivation = hosted_derivation(manifest)?;
    let index = usize::try_from(index)
        .map_err(|_| RuntimeManifestError::BootstrapAccountIndexTooLarge { index })?;
    Ok(derive_dev_account_signer(&derivation.seed, index))
}

fn derive_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
) -> Result<SimAccount, RuntimeManifestError> {
    let derivation = hosted_derivation(manifest)?;
    let account_index = account
        .index
        .ok_or(RuntimeManifestError::ExplicitAccountsRequired)?;
    let index = usize::try_from(account_index).map_err(|_| {
        RuntimeManifestError::BootstrapAccountIndexTooLarge {
            index: account_index,
        }
    })?;
    let signer = derive_dev_account_signer(&derivation.seed, index);
    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_ref() != expected_public_key.as_slice() {
        return Err(RuntimeManifestError::BootstrapPublicKeyMismatch {
            index: account_index,
        });
    }

    if groundhog_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0)
        .address()
        != account.wallet_address.0
    {
        return Err(RuntimeManifestError::BootstrapAddressMismatch {
            index: account_index,
        });
    }
    Ok(SimAccount {
        index: account_index,
        signer,
        wallet_address: account.wallet_address.0,
        network_hash: manifest.network_hash.to_network_hash(),
    })
}

fn hosted_derivation(
    manifest: &RuntimeManifest,
) -> Result<
    &groundhog_json_rpc_types::runtime_manifest::BootstrapAccountDerivation,
    RuntimeManifestError,
> {
    if manifest.kind
        != groundhog_json_rpc_types::runtime_manifest::HOSTED_DEVNET_RUNTIME_MANIFEST_KIND
    {
        return Err(RuntimeManifestError::ExplicitAccountsRequired);
    }
    let derivation = manifest
        .bootstrap_account_derivation
        .as_ref()
        .ok_or(RuntimeManifestError::ExplicitAccountsRequired)?;
    if derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(RuntimeManifestError::UnsupportedDerivationScheme {
            scheme: derivation.scheme.clone(),
        });
    }
    Ok(derivation)
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeManifestError {
    #[error(
        "local runtime manifests contain public allocations only; supply explicit scenario-owned native accounts"
    )]
    ExplicitAccountsRequired,
    #[error("failed to read runtime manifest {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("runtime manifest {path} exceeds {max_bytes} bytes")]
    TooLarge { path: String, max_bytes: u64 },
    #[error("failed to parse runtime manifest {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported runtime manifest schemaVersion {actual}")]
    UnsupportedSchema { actual: u32 },
    #[error("unsupported bootstrap account derivation scheme `{scheme}`")]
    UnsupportedDerivationScheme { scheme: String },
    #[error("runtime manifest does not advertise bootstrap account 0 with role `faucet`")]
    MissingProvisioningFaucet,
    #[error(
        "runtime manifest bootstrap account {index} address is not its nonce-zero native account"
    )]
    BootstrapAddressMismatch { index: u32 },
    #[error("runtime manifest bootstrap account index {index} does not fit this platform")]
    BootstrapAccountIndexTooLarge { index: u32 },
    #[error("runtime manifest bootstrap account {index} public key does not match derivation seed")]
    BootstrapPublicKeyMismatch { index: u32 },
    #[error(
        "manifest protocol {expected} does not match this client's compiled protocol {compiled}"
    )]
    ClientProtocolMismatch { expected: String, compiled: String },
    #[error(
        "runtime network hash mismatch: manifest expected {expected}, public RPC reported {observed}"
    )]
    NetworkHashMismatch {
        expected: NetworkHashHex,
        observed: NetworkHashHex,
    },
    #[error("public groundhog_networkInfo failed at {rpc_url}: {error}")]
    PublicNetworkInfo { rpc_url: String, error: String },
    #[error("public groundhog_networkInfo at {rpc_url} timed out after {timeout:?}")]
    PublicNetworkInfoTimeout { rpc_url: String, timeout: Duration },
    #[error("failed to build public JSON-RPC client for {rpc_url}: {error}")]
    PublicRpcClient { rpc_url: String, error: String },
    #[error("public groundhog_chainTip failed at {rpc_url}: {error}")]
    PublicChainTip { rpc_url: String, error: String },
    #[error("public groundhog_chainTip at {rpc_url} timed out after {timeout:?}")]
    PublicChainTipTimeout { rpc_url: String, timeout: Duration },
}

#[cfg(test)]
mod tests {
    use groundhog_json_rpc_types::hex::Hex0x;
    use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
    use groundhog_signing::derive_dev_account_signer;
    use groundhog_xdr::types::Ed25519PublicKey;
    use serde_json::json;

    use super::*;

    #[test]
    fn derive_accounts_validates_manifest_public_keys() {
        let manifest = runtime_manifest(false);

        let accounts = derive_accounts(&manifest).expect("matching public key should derive");

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].index, 0);
    }

    #[test]
    fn derive_accounts_rejects_public_key_mismatch() {
        let manifest = runtime_manifest(true);

        let err = derive_accounts(&manifest).expect_err("mismatch should fail");

        assert!(matches!(
            err,
            RuntimeManifestError::BootstrapPublicKeyMismatch { index: 0 }
        ));
    }

    #[test]
    fn derive_accounts_excludes_faucet_and_preserves_local_user_index() {
        let mut manifest = runtime_manifest(false);
        manifest.bootstrap_accounts[0].role = "faucet".into();
        let signer = derive_dev_account_signer("seed", 3);
        let mut local_user = manifest.bootstrap_accounts[0].clone();
        local_user.index = Some(3);
        local_user.role = BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER.into();
        local_user.public_key = Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes()));
        local_user.wallet_address = Hex0x(
            groundhog_signing::PubkeyAccount::from_public_key_bytes(
                signer.verifying_key_bytes(),
                0,
            )
            .address(),
        );
        manifest.bootstrap_accounts.push(local_user);

        let accounts = derive_accounts(&manifest).expect("local-user account should derive");

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].index, 3);
        assert_eq!(
            accounts[0].signer.verifying_key_bytes(),
            signer.verifying_key_bytes()
        );
    }

    async fn public_server(
        network: serde_json::Value,
        tip: serde_json::Value,
    ) -> (String, jsonrpsee::server::ServerHandle) {
        let server = jsonrpsee::server::ServerBuilder::default()
            .build("127.0.0.1:0")
            .await
            .expect("test server");
        let url = format!(
            "http://{}",
            server.local_addr().expect("test server address")
        );
        let mut module = jsonrpsee::RpcModule::new(());
        module
            .register_method("groundhog_networkInfo", move |_, (), _| network.clone())
            .expect("network method");
        module
            .register_method("groundhog_chainTip", move |_, (), _| tip.clone())
            .expect("tip method");
        (url, server.start(module))
    }

    #[tokio::test]
    async fn runtime_preflight_uses_public_rpc_without_connecting_to_scheduler() {
        let scheduler = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("scheduler trap");
        let mut manifest = runtime_manifest(false);
        manifest.endpoints.scheduler = scheduler
            .local_addr()
            .expect("trap address")
            .to_string()
            .parse()
            .expect("endpoint");
        let (url, server) = public_server(
            json!({"networkHash":prefixed_hex(0)}),
            json!({"height":"1","blockHash":prefixed_hex(1)}),
        )
        .await;
        preflight_chain_identity(&manifest, &url, Duration::from_secs(1))
            .await
            .expect("public identity matches");
        assert!(
            tokio::time::timeout(Duration::from_millis(10), scheduler.accept())
                .await
                .is_err()
        );
        server.stop().expect("stop server");
    }

    #[tokio::test]
    async fn runtime_preflight_rejects_public_network_mismatch() {
        let (url, server) = public_server(
            json!({"networkHash":prefixed_hex(0xff)}),
            json!({"height":"1","blockHash":prefixed_hex(1)}),
        )
        .await;
        let error =
            preflight_chain_identity(&runtime_manifest(false), &url, Duration::from_secs(1))
                .await
                .expect_err("wrong network");
        assert!(matches!(
            error,
            RuntimeManifestError::NetworkHashMismatch { .. }
        ));
        server.stop().expect("stop server");
    }

    #[tokio::test]
    async fn runtime_preflight_rejects_malformed_public_tip() {
        let (url, server) =
            public_server(json!({"networkHash":prefixed_hex(0)}), json!(null)).await;
        let error =
            preflight_chain_identity(&runtime_manifest(false), &url, Duration::from_secs(1))
                .await
                .expect_err("tip unavailable");
        assert!(matches!(error, RuntimeManifestError::PublicChainTip { .. }));
        server.stop().expect("stop server");
    }

    #[tokio::test]
    async fn runtime_preflight_rejects_local_client_protocol_mismatch_before_rpc() {
        let mut manifest = runtime_manifest(false);
        manifest.protocol = "hogp/other00000000000".into();
        let error =
            preflight_chain_identity(&manifest, "http://127.0.0.1:1", Duration::from_millis(10))
                .await
                .expect_err("incompatible client");
        assert!(matches!(
            error,
            RuntimeManifestError::ClientProtocolMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn runtime_preflight_timeout_bounds_pending_operations() {
        let result =
            preflight_timeout(Duration::from_millis(1), std::future::pending::<()>()).await;

        assert!(result.is_err());
    }

    #[test]
    fn runtime_config_hash_excludes_dial_addresses_but_preserves_capabilities() {
        let manifest = runtime_manifest(false);
        let manifest_json =
            serde_json::to_value(&manifest).expect("runtime manifest should encode");
        let expected = runtime_config_sha256(
            &serde_json::to_vec(&manifest_json).expect("runtime manifest should encode"),
        )
        .expect("runtime config should hash");

        let mut different_addresses = manifest_json.clone();
        different_addresses["endpoints"]["scheduler"] = json!("127.0.0.1:2000");
        different_addresses["endpoints"]["indexer"] = json!("127.0.0.1:2001");
        assert_eq!(
            expected,
            runtime_config_sha256(
                &serde_json::to_vec(&different_addresses).expect("runtime manifest should encode")
            )
            .expect("runtime config should hash")
        );

        different_addresses["endpoints"]
            .as_object_mut()
            .expect("endpoints should be an object")
            .remove("indexer");
        assert_ne!(
            expected,
            runtime_config_sha256(
                &serde_json::to_vec(&different_addresses).expect("runtime manifest should encode")
            )
            .expect("runtime config should hash")
        );

        let mut additive_endpoint_a = manifest_json.clone();
        additive_endpoint_a["endpoints"]["futureGateway"] = json!("127.0.0.1:3000");
        let additive_endpoint_hash = runtime_config_sha256(
            &serde_json::to_vec(&additive_endpoint_a).expect("runtime manifest should encode"),
        )
        .expect("runtime config should hash");
        let mut additive_endpoint_b = additive_endpoint_a.clone();
        additive_endpoint_b["endpoints"]["futureGateway"] = json!("127.0.0.1:3001");
        assert_eq!(
            additive_endpoint_hash,
            runtime_config_sha256(
                &serde_json::to_vec(&additive_endpoint_b).expect("runtime manifest should encode")
            )
            .expect("runtime config should hash")
        );
        additive_endpoint_b["endpoints"]
            .as_object_mut()
            .expect("endpoints should be an object")
            .remove("futureGateway");
        assert_ne!(
            additive_endpoint_hash,
            runtime_config_sha256(
                &serde_json::to_vec(&additive_endpoint_b).expect("runtime manifest should encode")
            )
            .expect("runtime config should hash")
        );

        let mut unknown_metadata = manifest_json;
        unknown_metadata["futureRuntimeMetadata"] = json!({"mode": "different"});
        assert_ne!(
            expected,
            runtime_config_sha256(
                &serde_json::to_vec(&unknown_metadata).expect("runtime manifest should encode")
            )
            .expect("runtime config should hash")
        );
    }

    fn runtime_manifest(mismatch: bool) -> RuntimeManifest {
        let signer = derive_dev_account_signer("seed", 0);
        let public_key = if mismatch {
            Hex0x(Ed25519PublicKey::from([0xff; 32]))
        } else {
            Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes()))
        };
        serde_json::from_value(json!({
            "schemaVersion": 3,
            "kind": "hosted-devnet-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": protocol_text(&groundhog_xdr::PROTOCOL_VERSION),
            "producer": {
                "binary": "hog-devnode",
                "version": "0.1.0"
            },
            "endpoints": {
                "scheduler": "127.0.0.1:1000",
                "indexer": "127.0.0.1:1001"
            },
            "bootstrapAccountDerivation": {
                "scheme": SUPPORTED_DERIVATION_SCHEME,
                "seed": "seed"
            },
            "bootstrapAccounts": [{
                "index": 0,
                "role": "local_user",
                "publicKey": public_key,
                "walletAddress": format!("0x{}", hex::encode(groundhog_signing::PubkeyAccount::from_public_key_bytes(signer.verifying_key_bytes(), 0).address().as_ref())),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "storageCostModel": {
                    "hash": prefixed_hex(0x55),
                    "baseDataItemCost": "1",
                    "bytestringCostPerByte": "1",
                    "baseContractDeploymentCost": "1",
                    "contractDeploymentCostPerByte": "1",
                    "authPerKeyCost": "1",
                    "rcqCostPerEntry": "1",
                    "replayCacheCostPerEntry": "1",
                    "balanceCost": "1"
                }
            }
        }))
        .expect("runtime manifest fixture should parse")
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
