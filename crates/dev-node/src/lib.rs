//! Groundhog local development node.
//!
//! Embeds the SDK's `native_exec` executor behind a standard
//! `GroundhogApi` JSON-RPC interface. Same execution semantics
//! as SDK unit tests, accessible over HTTP.

pub mod accounts;
pub mod backend;
pub mod error;
pub mod rpc;
pub mod wallet;
pub mod worker;

use groundhog_rpc_api::{GroundhogApiServer, GroundhogDevApiServer};
use jsonrpsee::server::{RpcModule, Server};

use crate::accounts::{DEFAULT_ACCOUNT_COUNT, DEFAULT_STORAGE_TOKENS};
use crate::rpc::DevNodeRpc;
use crate::worker::{WorkerHandle, spawn_worker};

/// Default number of committed transactions to retain with full execution detail.
pub const DEFAULT_TRANSACTION_RETENTION_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DevNodeStats {
    pub block_count: u64,
    pub tx_count: usize,
}

/// Configuration for the dev node server.
#[derive(Debug, Clone)]
pub struct DevNodeConfig {
    /// Listen address (default: "127.0.0.1:9545").
    pub listen_address: String,
    /// Number of pre-funded accounts (default: 10).
    pub account_count: usize,
    /// Storage tokens per account (default: `1_000_000_000`).
    pub storage_tokens: u64,
    /// Number of committed transactions to retain with full receipt/trace data.
    pub transaction_retention_limit: usize,
}

impl Default for DevNodeConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1:9545".to_string(),
            account_count: DEFAULT_ACCOUNT_COUNT,
            storage_tokens: DEFAULT_STORAGE_TOKENS,
            transaction_retention_limit: DEFAULT_TRANSACTION_RETENTION_LIMIT,
        }
    }
}

/// Start the dev node JSON-RPC server.
///
/// Returns a handle containing the pre-funded accounts and server
/// control. The caller can await `handle.stopped()` to block until
/// shutdown, or call `handle.stop()` to initiate shutdown.
pub async fn start_server(
    config: DevNodeConfig,
) -> Result<DevNodeHandle, Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(
        listen_address = %config.listen_address,
        account_count = config.account_count,
        storage_tokens = config.storage_tokens,
        transaction_retention_limit = config.transaction_retention_limit,
        "starting dev node"
    );

    let worker = spawn_worker(
        config.account_count,
        config.storage_tokens,
        config.transaction_retention_limit,
    )
    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
    let accounts = worker
        .get_accounts()
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

    let server = Server::builder().build(&config.listen_address).await?;

    // DevNodeRpc implements both GroundhogApiServer and GroundhogDevApiServer.
    // jsonrpsee's into_rpc() only picks up one trait. Use RpcModule::merge
    // to combine both namespaces into a single module.
    let rpc = DevNodeRpc::new(worker.clone());
    let mut module = RpcModule::new(());
    module.merge(GroundhogApiServer::into_rpc(rpc.clone()))?;
    module.merge(GroundhogDevApiServer::into_rpc(rpc))?;

    let addr = server.local_addr()?;
    let handle = server.start(module);

    tracing::info!(%addr, account_count = accounts.len(), "dev node listening");

    Ok(DevNodeHandle {
        accounts,
        addr,
        worker,
        handle,
    })
}

/// Running dev node server.
#[derive(Debug)]
pub struct DevNodeHandle {
    /// Pre-funded dev accounts.
    pub accounts: Vec<groundhog_rpc_api_types::dev::DevAccount>,
    /// Actual bound address (useful when binding to port 0).
    pub addr: std::net::SocketAddr,
    worker: WorkerHandle,
    handle: jsonrpsee::server::ServerHandle,
}

impl DevNodeHandle {
    /// Current dev-node summary stats.
    pub async fn stats(&self) -> Result<DevNodeStats, crate::error::DevNodeError> {
        let stats = self.worker.stats().await?;
        Ok(DevNodeStats {
            block_count: stats.block_count,
            tx_count: stats.tx_count,
        })
    }

    /// Wait until the server stops.
    pub async fn stopped(&self) {
        self.handle.clone().stopped().await;
    }

    /// Signal the server to stop.
    pub fn stop(&self) -> Result<(), jsonrpsee::server::AlreadyStoppedError> {
        self.handle.stop()
    }
}

#[cfg(test)]
mod tests {
    use super::{DevNodeConfig, start_server};
    use groundhog_rpc_api::{GroundhogApiClient, GroundhogDevApiClient};
    use groundhog_rpc_api_types::{ContractAddress, hex::HexBytes};
    use groundhog_signing::{
        BuildCallTransaction, BuildDeployTransaction, DEFAULT_PUBKEY_ACCOUNT_NONCE,
        DEFAULT_TX_GAS_LIMIT, PubkeyAccount, SignTransactionParams, Signer, build_call_transaction,
        build_deploy_transaction, derive_deploy_address, encode_signed_transaction_xdr,
        sign_transaction,
    };
    use groundhog_xdr::auth_claim::{AuthClaim, AuthClaimData};
    use groundhog_xdr::contract::{Contract, ContractBlob, ContractEntrypointID};
    use groundhog_xdr::types::SHA256Hash;
    use jsonrpsee::http_client::HttpClientBuilder;
    use sha2::{Digest, Sha256};
    use xdr_common::{BytesM, VecM};

    #[tokio::test]
    async fn get_storage_at_returns_present_bytestring_over_rpc() {
        let handle = start_server(DevNodeConfig {
            listen_address: "127.0.0.1:0".into(),
            ..Default::default()
        })
        .await
        .expect("bug: dev node should start");

        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.addr))
            .expect("bug: HTTP client should build");
        let address =
            ContractAddress::from(groundhog_xdr::types::ContractAddress::from([0x11; 32]));
        let key = [0x22; 32];

        handle
            .worker
            .seed_bytestring(address, key, b"hello".to_vec())
            .await
            .expect("bug: test seed write should succeed");

        let result = client
            .get_storage_at(address, HexBytes::from(key.to_vec()))
            .await
            .expect("bug: getStorageAt should succeed")
            .expect("bug: seeded storage item should be present");

        assert_eq!(result.kind, "bytestring");
        assert_eq!(result.summary, "Bytestring = 0x68656c6c6f");

        handle.stop().expect("bug: server should stop cleanly");
        handle.stopped().await;
    }

    #[tokio::test]
    async fn get_storage_at_returns_present_bytestring_over_rpc_for_deployed_address() {
        let handle = start_server(DevNodeConfig {
            listen_address: "127.0.0.1:0".into(),
            ..Default::default()
        })
        .await
        .expect("bug: dev node should start");

        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.addr))
            .expect("bug: HTTP client should build");
        let address =
            ContractAddress::from(groundhog_xdr::types::ContractAddress::from([0x33; 32]));
        let key = [0x44; 32];

        handle
            .worker
            .seed_deployed_bytestring(address, key, b"hello".to_vec())
            .await
            .expect("bug: deployed test seed write should succeed");

        let result = client
            .get_storage_at(address, HexBytes::from(key.to_vec()))
            .await
            .expect("bug: getStorageAt should succeed")
            .expect("bug: seeded storage item should be present");

        assert_eq!(result.kind, "bytestring");
        assert_eq!(result.summary, "Bytestring = 0x68656c6c6f");

        handle.stop().expect("bug: server should stop cleanly");
        handle.stopped().await;
    }

    #[tokio::test]
    async fn get_storage_at_missing_key_returns_none_over_rpc() {
        let handle = start_server(DevNodeConfig {
            listen_address: "127.0.0.1:0".into(),
            ..Default::default()
        })
        .await
        .expect("bug: dev node should start");

        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.addr))
            .expect("bug: HTTP client should build");

        let result = client
            .get_storage_at(
                ContractAddress::from(groundhog_xdr::types::ContractAddress::from([0x11; 32])),
                HexBytes::from(vec![0x22; 32]),
            )
            .await
            .expect("bug: getStorageAt should succeed");

        assert!(result.is_none());

        handle.stop().expect("bug: server should stop cleanly");
        handle.stopped().await;
    }

    #[tokio::test]
    async fn get_storage_at_after_real_wasm_deploy_and_call_over_rpc() {
        let wasm_path = std::env::var("GROUNDHOG_SMOKE_WASM").unwrap_or_else(|_| {
            "/tmp/gh-storage-smoke-target/groundhog/gh_storage_smoke.wasm".into()
        });
        if !std::path::Path::new(&wasm_path).exists() {
            eprintln!("skipping real wasm rpc reproduction; missing {wasm_path}");
            return;
        }

        let handle = start_server(DevNodeConfig {
            listen_address: "127.0.0.1:0".into(),
            ..Default::default()
        })
        .await
        .expect("bug: dev node should start");

        let client = HttpClientBuilder::default()
            .build(format!("http://{}", handle.addr))
            .expect("bug: HTTP client should build");
        let signer = crate::accounts::derive_dev_signer(0);
        let wallet_address = PubkeyAccount::new(signer.public_key(), DEFAULT_PUBKEY_ACCOUNT_NONCE)
            .address()
            .expect("bug: dev wallet address derivation should succeed");
        let wasm_bytes = std::fs::read(&wasm_path).expect("bug: smoke wasm should be readable");
        let contract = Contract::kWasmi {
            wasmi_contract: ContractBlob {
                wasm: BytesM(wasm_bytes),
            },
        };
        let contract_xdr = xdrs::txs::encode(&contract).expect("bug: contract should encode");
        let contract_hash = SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(&contract_xdr)));
        let deploy_nonce =
            SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(1234_u64.to_le_bytes())));
        let deployed_address: ContractAddress =
            derive_deploy_address(&wallet_address, &deploy_nonce, &contract_hash)
                .expect("bug: deploy address should derive")
                .into();
        let deploy_calldata = xdrs::cxs::encode(&(&deploy_nonce, &contract_hash))
            .expect("bug: deploy calldata should encode");
        let auth_claims: AuthClaimData = vec![AuthClaim::kInvokedAddressSignature {
            invoked_address_signature_id: 0,
        }]
        .try_into()
        .expect("bug: auth claim should fit");

        let deploy_tx = build_deploy_transaction(BuildDeployTransaction {
            invoked_address: wallet_address,
            entrypoint: ContractEntrypointID::from(groundhog_signing::WALLET_DEPLOY_ENTRYPOINT),
            calldata: deploy_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: i64::MAX,
            nonce: 1234,
            contracts_to_deploy: VecM(vec![contract]),
            auth_claims: auth_claims.clone(),
        });
        let signed_deploy = sign_transaction(deploy_tx, &signer, SignTransactionParams::default())
            .expect("bug: deploy tx should sign");
        let deploy_tx_bytes =
            encode_signed_transaction_xdr(&signed_deploy).expect("bug: deploy tx should encode");

        client
            .send_raw_transaction(format!("0x{}", hex::encode(&deploy_tx_bytes)))
            .await
            .expect("bug: deploy should commit");

        let set_entrypoint = contract_entrypoint_id("SmokeStorage", "0.1.0", "set");
        let set_calldata =
            xdrs::cxs::encode(&"cafebabe".to_string()).expect("bug: set calldata should encode");
        let inner_invocation = xdrs::txs::encode(&(
            deployed_address.0,
            set_entrypoint,
            DEFAULT_TX_GAS_LIMIT,
            set_calldata.as_slice(),
        ))
        .expect("bug: inner invocation should encode");
        let do_action_calldata = xdrs::cxs::encode(&inner_invocation.as_slice())
            .expect("bug: wallet calldata should encode");

        let set_tx = build_call_transaction(BuildCallTransaction {
            invoked_address: wallet_address,
            entrypoint: ContractEntrypointID::from(0x1984_6D75_0FAD_81D6_u64.to_be_bytes()),
            calldata: do_action_calldata,
            gas_limit: DEFAULT_TX_GAS_LIMIT,
            block_expiry_height: i64::MAX,
            nonce: 1235,
            auth_claims,
        });
        let signed_set = sign_transaction(set_tx, &signer, SignTransactionParams::default())
            .expect("bug: set tx should sign");
        let set_tx_bytes =
            encode_signed_transaction_xdr(&signed_set).expect("bug: set tx should encode");

        client
            .send_raw_transaction(format!("0x{}", hex::encode(&set_tx_bytes)))
            .await
            .expect("bug: set should commit");

        let result = client
            .get_storage_at(deployed_address, HexBytes::from(vec![0x42; 32]))
            .await
            .expect("bug: getStorageAt should succeed")
            .expect("bug: committed storage should be present");

        assert_eq!(result.kind, "bytestring");
        assert_eq!(result.summary, "Bytestring = 0x6361666562616265");

        handle.stop().expect("bug: server should stop cleanly");
        handle.stopped().await;
    }

    fn contract_entrypoint_id(
        interface_name: &str,
        version_str: &str,
        method: &str,
    ) -> ContractEntrypointID {
        let mut hasher = Sha256::new();
        hasher.update(interface_name.as_bytes());
        hasher.update(major_version(version_str).as_bytes());
        hasher.update(method.as_bytes());
        let hash = hasher.finalize();
        let mut id = [0u8; 8];
        id.copy_from_slice(&hash[..8]);
        ContractEntrypointID::from(id)
    }

    fn major_version(version: &str) -> &str {
        let normalized = version.split_once('+').map_or(version, |(base, _)| base);
        let normalized = normalized
            .split_once('-')
            .map_or(normalized, |(base, _)| base);

        let mut parts = normalized.split('.');
        let major = parts.next().unwrap_or(normalized);
        let minor = parts.next().unwrap_or("0");
        let patch = parts.next().unwrap_or("0");

        if major != "0" {
            major
        } else if minor != "0" {
            &normalized[..major.len() + 1 + minor.len()]
        } else if patch != "0" {
            &normalized[..major.len() + 1 + minor.len() + 1 + patch.len()]
        } else {
            normalized
        }
    }
}
