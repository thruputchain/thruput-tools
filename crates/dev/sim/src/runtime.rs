use std::{future::Future, path::Path, time::Duration};

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_types::{
    BlockHeight, NetworkHashHex, SHA256Hash,
    block::{ChainTip, consensus_value_hash},
    runtime_manifest::{
        BOOTSTRAP_ACCOUNT_ROLE_FAUCET, BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER, BootstrapAccount,
        RuntimeManifest, RuntimeManifestParseError, parse_runtime_manifest,
    },
};
use groundhog_scheduler::sched_coord::{
    SchedulerCoord_SchedulerCoord1_Client, SchedulerGetTipResult,
};
use groundhog_signing::{LocalSigner, derive_dev_account_signer};
use groundhog_x23_transport::{ConnectError, connect};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const SUPPORTED_DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
const MAX_RUNTIME_MANIFEST_BYTES: u64 = 1024 * 1024;
const CHAIN_IDENTITY_PREFLIGHT_ATTEMPTS: usize = 5;

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
    let scheduler_address = manifest.endpoints.scheduler.to_string();
    let scheduler = preflight_timeout(
        request_timeout,
        connect(
            "sim_scheduler_preflight",
            &scheduler_address,
            1,
            SchedulerCoord_SchedulerCoord1_Client::new,
        ),
    )
    .await
    .map_err(|_| RuntimeManifestError::SchedulerConnectTimeout {
        address: scheduler_address.clone(),
        timeout: request_timeout,
    })?
    .map_err(|source| RuntimeManifestError::SchedulerConnect {
        address: scheduler_address.clone(),
        source,
    })?;
    let public_client = HttpClientBuilder::default()
        .request_timeout(request_timeout)
        .build(rpc_url)
        .map_err(|error| RuntimeManifestError::PublicRpcClient {
            rpc_url: rpc_url.to_string(),
            error: error.to_string(),
        })?;
    for attempt in 0..CHAIN_IDENTITY_PREFLIGHT_ATTEMPTS {
        let scheduler_before = scheduler_tip(
            &scheduler,
            &scheduler_address,
            request_timeout,
            &manifest.network_hash,
        )
        .await?;
        let public_tip = preflight_timeout(
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

        if chain_tips_match(&scheduler_before, &public_tip) {
            return Ok(());
        }

        let scheduler_after = scheduler_tip(
            &scheduler,
            &scheduler_address,
            request_timeout,
            &manifest.network_hash,
        )
        .await?;
        match validate_public_scheduler_tips(&scheduler_before, &scheduler_after, &public_tip) {
            Ok(()) => return Ok(()),
            Err(_)
                if attempt + 1 < CHAIN_IDENTITY_PREFLIGHT_ATTEMPTS
                    && scheduler_advanced_past_public(
                        &scheduler_before,
                        &scheduler_after,
                        &public_tip,
                    ) => {}
            Err(error) => return Err(error),
        }
    }

    unreachable!("chain identity preflight loop always returns on its final attempt")
}

async fn scheduler_tip(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
    scheduler_address: &str,
    request_timeout: Duration,
    expected_network_hash: &NetworkHashHex,
) -> Result<ChainTip, RuntimeManifestError> {
    let tip = preflight_timeout(request_timeout, scheduler.schedcoord_get_tip())
        .await
        .map_err(|_| RuntimeManifestError::SchedulerTipTimeout {
            address: scheduler_address.to_string(),
            timeout: request_timeout,
        })?
        .map_err(|error| RuntimeManifestError::SchedulerTip {
            address: scheduler_address.to_string(),
            error: error.to_string(),
        })?;
    match tip {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => {
            validate_network_hash(
                expected_network_hash,
                db_tip
                    .value
                    .proposal_header
                    .network_hash
                    .clone()
                    .into_inner()
                    .as_ref(),
            )?;
            Ok(ChainTip {
                height: db_tip.value.proposal_header.block_height.clone(),
                block_hash: consensus_value_hash(&db_tip.value)
                    .map_err(|source| RuntimeManifestError::SchedulerTipEncoding { source })?,
                protocol: Some(db_tip.value.protocol.clone()),
            })
        }
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
            Err(RuntimeManifestError::SchedulerHasNoGenesis)
        }
        other => Err(RuntimeManifestError::UnexpectedSchedulerTip {
            result: format!("{other:?}"),
        }),
    }
}

async fn preflight_timeout<T>(
    timeout: Duration,
    future: impl Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, future).await
}

fn chain_tips_match(left: &ChainTip, right: &ChainTip) -> bool {
    left.height == right.height && left.block_hash == right.block_hash
}

fn scheduler_advanced_past_public(
    scheduler_before: &ChainTip,
    scheduler_after: &ChainTip,
    public: &ChainTip,
) -> bool {
    scheduler_before.height < public.height && public.height < scheduler_after.height
}

fn validate_public_scheduler_tips(
    scheduler_before: &ChainTip,
    scheduler_after: &ChainTip,
    public: &ChainTip,
) -> Result<(), RuntimeManifestError> {
    if chain_tips_match(scheduler_before, public) || chain_tips_match(scheduler_after, public) {
        return Ok(());
    }
    Err(RuntimeManifestError::PublicSchedulerTipMismatch {
        scheduler_height: scheduler_after.height.clone(),
        scheduler_block_hash: scheduler_after.block_hash,
        public_height: public.height.clone(),
        public_block_hash: public.block_hash,
    })
}

fn validate_network_hash(
    expected: &NetworkHashHex,
    observed_bytes: &[u8],
) -> Result<(), RuntimeManifestError> {
    let mut bytes = [0; 32];
    bytes.copy_from_slice(observed_bytes);
    let observed = NetworkHashHex::from(bytes);
    if expected == &observed {
        return Ok(());
    }
    Err(RuntimeManifestError::NetworkHashMismatch {
        expected: expected.clone(),
        observed,
    })
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
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(RuntimeManifestError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }
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
        .find(|account| account.index == 0 && account.role == BOOTSTRAP_ACCOUNT_ROLE_FAUCET)
        .ok_or(RuntimeManifestError::MissingProvisioningFaucet)?;
    derive_account(manifest, account)
}

pub fn derive_provisioned_account_signer(
    manifest: &RuntimeManifest,
    index: u32,
) -> Result<LocalSigner, RuntimeManifestError> {
    if manifest.bootstrap_account_derivation.scheme != SUPPORTED_DERIVATION_SCHEME {
        return Err(RuntimeManifestError::UnsupportedDerivationScheme {
            scheme: manifest.bootstrap_account_derivation.scheme.clone(),
        });
    }
    let index = usize::try_from(index)
        .map_err(|_| RuntimeManifestError::BootstrapAccountIndexTooLarge { index })?;
    Ok(derive_dev_account_signer(
        &manifest.bootstrap_account_derivation.seed,
        index,
    ))
}

fn derive_account(
    manifest: &RuntimeManifest,
    account: &BootstrapAccount,
) -> Result<SimAccount, RuntimeManifestError> {
    let index = usize::try_from(account.index).map_err(|_| {
        RuntimeManifestError::BootstrapAccountIndexTooLarge {
            index: account.index,
        }
    })?;
    let signer = derive_dev_account_signer(&manifest.bootstrap_account_derivation.seed, index);
    let expected_public_key = signer.verifying_key_bytes();
    if account.public_key.as_ref() != expected_public_key.as_slice() {
        return Err(RuntimeManifestError::BootstrapPublicKeyMismatch {
            index: account.index,
        });
    }

    Ok(SimAccount {
        index: account.index,
        signer,
        wallet_address: account.wallet_address.0,
        network_hash: manifest.network_hash.to_network_hash(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeManifestError {
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
    #[error("runtime manifest bootstrap account index {index} does not fit this platform")]
    BootstrapAccountIndexTooLarge { index: u32 },
    #[error("runtime manifest bootstrap account {index} public key does not match derivation seed")]
    BootstrapPublicKeyMismatch { index: u32 },
    #[error("failed to connect to runtime scheduler at {address}: {source}")]
    SchedulerConnect {
        address: String,
        #[source]
        source: ConnectError,
    },
    #[error("timed out connecting to runtime scheduler at {address} after {timeout:?}")]
    SchedulerConnectTimeout { address: String, timeout: Duration },
    #[error("failed to query runtime scheduler tip at {address}: {error}")]
    SchedulerTip { address: String, error: String },
    #[error("timed out querying runtime scheduler tip at {address} after {timeout:?}")]
    SchedulerTipTimeout { address: String, timeout: Duration },
    #[error("failed to encode runtime scheduler tip: {source}")]
    SchedulerTipEncoding {
        #[source]
        source: xdrs::WriteError,
    },
    #[error("runtime scheduler has no committed genesis block")]
    SchedulerHasNoGenesis,
    #[error("runtime scheduler returned an unexpected tip result: {result}")]
    UnexpectedSchedulerTip { result: String },
    #[error(
        "runtime network hash mismatch: manifest expected {expected}, scheduler reported {observed}"
    )]
    NetworkHashMismatch {
        expected: NetworkHashHex,
        observed: NetworkHashHex,
    },
    #[error("failed to build public JSON-RPC client for {rpc_url}: {error}")]
    PublicRpcClient { rpc_url: String, error: String },
    #[error("public groundhog_chainTip failed at {rpc_url}: {error}")]
    PublicChainTip { rpc_url: String, error: String },
    #[error("public groundhog_chainTip at {rpc_url} timed out after {timeout:?}")]
    PublicChainTipTimeout { rpc_url: String, timeout: Duration },
    #[error(
        "public RPC tip does not match scheduler tip: scheduler height {scheduler_height:?} hash \
         {scheduler_block_hash:?}, public height {public_height:?} hash {public_block_hash:?}"
    )]
    PublicSchedulerTipMismatch {
        scheduler_height: BlockHeight,
        scheduler_block_hash: SHA256Hash,
        public_height: BlockHeight,
        public_block_hash: SHA256Hash,
    },
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
        local_user.index = 3;
        local_user.role = BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER.into();
        local_user.public_key = Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes()));
        manifest.bootstrap_accounts.push(local_user);

        let accounts = derive_accounts(&manifest).expect("local-user account should derive");

        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].index, 3);
        assert_eq!(
            accounts[0].signer.verifying_key_bytes(),
            signer.verifying_key_bytes()
        );
    }

    #[test]
    fn runtime_preflight_rejects_scheduler_network_hash_mismatch() {
        let manifest = runtime_manifest(false);

        let err = validate_network_hash(&manifest.network_hash, &[0xff; 32])
            .expect_err("mismatched scheduler identity should fail preflight");

        assert!(matches!(
            err,
            RuntimeManifestError::NetworkHashMismatch {
                expected,
                observed,
            } if expected == NetworkHashHex::from([0; 32])
                && observed == NetworkHashHex::from([0xff; 32])
        ));
    }

    #[test]
    fn runtime_preflight_rejects_public_rpc_scheduler_fork_after_recheck() {
        let scheduler_before = chain_tip(12, 0x11);
        let scheduler_after = chain_tip(13, 0x33);
        let public = chain_tip(13, 0x22);

        let err = validate_public_scheduler_tips(&scheduler_before, &scheduler_after, &public)
            .expect_err("public and scheduler block hashes must match");

        assert!(matches!(
            err,
            RuntimeManifestError::PublicSchedulerTipMismatch {
                scheduler_height,
                scheduler_block_hash,
                public_height,
                public_block_hash,
            } if scheduler_height == BlockHeight::from(13)
                && scheduler_block_hash == scheduler_after.block_hash
                && public_height == BlockHeight::from(13)
                && public_block_hash == public.block_hash
        ));
    }

    #[test]
    fn runtime_preflight_rejects_stable_public_rpc_scheduler_tip_mismatch() {
        let scheduler_before = chain_tip(12, 0x11);
        let scheduler_after = chain_tip(12, 0x11);
        let public = chain_tip(12, 0x22);

        validate_public_scheduler_tips(&scheduler_before, &scheduler_after, &public)
            .expect_err("stable wrong-chain tip must fail preflight");
    }

    #[test]
    fn runtime_preflight_accepts_public_tip_matching_initial_scheduler_tip() {
        let scheduler = chain_tip(12, 0x11);
        let public = chain_tip(12, 0x11);

        assert!(chain_tips_match(&scheduler, &public));
    }

    #[test]
    fn runtime_preflight_accepts_public_tip_matching_scheduler_recheck_after_advance() {
        let scheduler_before = chain_tip(12, 0x11);
        let scheduler_after = chain_tip(13, 0x22);
        let public = chain_tip(13, 0x22);

        validate_public_scheduler_tips(&scheduler_before, &scheduler_after, &public)
            .expect("public tip matching the scheduler recheck should pass");
    }

    #[test]
    fn runtime_preflight_retries_when_scheduler_advances_past_public_tip() {
        let scheduler_before = chain_tip(12, 0x11);
        let scheduler_after = chain_tip(14, 0x33);
        let public = chain_tip(13, 0x22);

        assert!(scheduler_advanced_past_public(
            &scheduler_before,
            &scheduler_after,
            &public
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

        let mut different_genesis = manifest_json.clone();
        different_genesis["genesisHash"] = json!(prefixed_hex(0x66));
        assert_ne!(
            expected,
            runtime_config_sha256(
                &serde_json::to_vec(&different_genesis).expect("runtime manifest should encode")
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
            "schemaVersion": 2,
            "kind": "hogd-dev-runtime",
            "chainState": "fresh",
            "networkHash": "00".repeat(32),
            "protocol": "hogp/test",
            "genesisHash": prefixed_hex(0x11),
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
                "walletAddress": prefixed_hex(0x22),
                "initialStorageTokens": "1000000000"
            }],
            "bootstrap": {
                "devWallet": {
                    "contentHash": prefixed_hex(0x33),
                    "deployedCodeHash": prefixed_hex(0x44),
                    "artifactHint": "dev_wallet.rv32a",
                    "deployEntrypoint": "0xadac498222148e4f",
                    "createWalletEntrypoint": "0x6372656174655761",
                    "doActionEntrypoint": "0x646f416374696f6e"
                },
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

    fn chain_tip(height: i64, block_hash: u8) -> ChainTip {
        ChainTip {
            height: BlockHeight::from(height),
            block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([block_hash; 32])),
            protocol: Some(groundhog_xdr::types::Protocol::new(1)),
        }
    }

    fn prefixed_hex(byte: u8) -> String {
        format!("0x{}", hex::encode([byte; 32]))
    }
}
