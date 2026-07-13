//! Handle onto the network a scenario runs against.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use groundhog_json_rpc_client::{GroundhogApiClient, HttpClient, HttpClientBuilder};
use groundhog_json_rpc_types::block::ChainTip;
use groundhog_json_rpc_types::events::{EventPage, EventQuery};
use groundhog_json_rpc_types::hex::{Hex0x, HexBytes};
use groundhog_json_rpc_types::receipt::TransactionReceipt;
use groundhog_json_rpc_types::runtime_manifest::RuntimeManifest;
use groundhog_json_rpc_types::state::StorageItemRead;
use groundhog_json_rpc_types::transaction::TxHash;
use groundhog_json_rpc_types::view::{ViewRequest, ViewResponse};
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::ContractAddress;

use crate::error::{Result, TestkitError};

const RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const RECEIPT_POLL_ATTEMPTS: usize = 40; // ~10 seconds at the interval above.

/// A cheap, cloneable handle onto the target network.
///
/// Chain verbs return [`TestkitError::Offline`] when the scenario has no live
/// target. [`Network::client`] exposes the underlying typed JSON-RPC client as
/// an escape hatch for methods this handle does not wrap.
#[derive(Clone)]
pub struct Network {
    inner: Arc<NetworkInner>,
}

struct NetworkInner {
    rpc_url: Option<String>,
    client: Option<HttpClient>,
    deploy_entrypoint: Option<ContractEntrypointID>,
    do_action_entrypoint: Option<ContractEntrypointID>,
}

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Network")
            .field("rpc_url", &self.inner.rpc_url)
            .field("online", &self.inner.client.is_some())
            .finish_non_exhaustive()
    }
}

impl Network {
    /// Connect to a live target and resolve the dev-wallet entrypoints.
    pub(crate) fn connect(rpc_url: &str, manifest: &RuntimeManifest) -> Result<Self> {
        let client = HttpClientBuilder::default()
            .build(rpc_url)
            .map_err(|e| TestkitError::rpc("connect", e))?;
        let deploy_entrypoint = manifest
            .bootstrap
            .dev_wallet
            .deploy_entrypoint_id()
            .map_err(|source| TestkitError::Manifest {
                field: "bootstrap.devWallet.deployEntrypoint",
                detail: source.to_string(),
            })?;
        let do_action_entrypoint = manifest
            .bootstrap
            .dev_wallet
            .do_action_entrypoint_id()
            .map_err(|source| TestkitError::Manifest {
                field: "bootstrap.devWallet.doActionEntrypoint",
                detail: source.to_string(),
            })?;
        Ok(Self {
            inner: Arc::new(NetworkInner {
                rpc_url: Some(rpc_url.to_owned()),
                client: Some(client),
                deploy_entrypoint: Some(deploy_entrypoint),
                do_action_entrypoint: Some(do_action_entrypoint),
            }),
        })
    }

    /// A handle with no live target; every chain verb reports [`TestkitError::Offline`].
    pub(crate) fn offline() -> Self {
        Self {
            inner: Arc::new(NetworkInner {
                rpc_url: None,
                client: None,
                deploy_entrypoint: None,
                do_action_entrypoint: None,
            }),
        }
    }

    /// The public JSON-RPC URL, when the target has one.
    pub fn rpc_url(&self) -> Option<&str> {
        self.inner.rpc_url.as_deref()
    }

    /// The underlying typed JSON-RPC client, for methods this handle does not wrap.
    pub fn client(&self) -> Option<&HttpClient> {
        self.inner.client.as_ref()
    }

    /// Current chain tip.
    pub async fn chain_tip(&self) -> Result<ChainTip> {
        self.require_client("chainTip")?
            .chain_tip()
            .await
            .map_err(|e| TestkitError::rpc("chainTip", e))
    }

    /// Submit a pre-signed `0x`-hex transaction and return its hash.
    pub async fn submit_raw(&self, tx_hex: String) -> Result<TxHash> {
        self.require_client("sendRawTransaction")?
            .send_raw_transaction(tx_hex)
            .await
            .map_err(|e| TestkitError::rpc("sendRawTransaction", e))
    }

    /// Fetch a transaction receipt if it is already available.
    pub async fn receipt(&self, tx_hash: TxHash) -> Result<Option<TransactionReceipt>> {
        self.require_client("getTransactionReceipt")?
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|e| TestkitError::rpc("getTransactionReceipt", e))
    }

    /// Poll for a receipt until it appears or the polling budget elapses.
    pub async fn await_receipt(&self, tx_hash: TxHash) -> Result<TransactionReceipt> {
        for _ in 0..RECEIPT_POLL_ATTEMPTS {
            if let Some(receipt) = self.receipt(tx_hash).await? {
                return Ok(receipt);
            }
            tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
        }
        Err(TestkitError::ReceiptTimeout {
            tx_hash: format!("{tx_hash:?}"),
        })
    }

    /// Read one latest-committed storage item at `address`/`key`.
    pub async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: &[u8],
    ) -> Result<StorageItemRead> {
        self.require_client("getStorageAt")?
            .get_storage_at(Hex0x(address), HexBytes(key.to_vec()))
            .await
            .map_err(|e| TestkitError::rpc("getStorageAt", e))
    }

    /// Simulate a contract call through `groundhog_view`.
    ///
    /// `caller` sets the simulated `msg.sender`; pass an agent's own address to
    /// read state keyed by that caller.
    pub async fn view_call(
        &self,
        contract: ContractAddress,
        entrypoint: [u8; 8],
        calldata: &[u8],
        caller: Option<ContractAddress>,
    ) -> Result<ViewResponse> {
        let request = ViewRequest {
            contract: Hex0x(contract),
            entrypoint: HexBytes(entrypoint.to_vec()),
            calldata: HexBytes(calldata.to_vec()),
            caller: caller.map(Hex0x),
        };
        self.require_client("view")?
            .view(request)
            .await
            .map_err(|e| TestkitError::rpc("view", e))
    }

    /// Query contract events.
    pub async fn get_events(&self, query: EventQuery) -> Result<EventPage> {
        self.require_client("getEvents")?
            .get_events(query)
            .await
            .map_err(|e| TestkitError::rpc("getEvents", e))
    }

    pub(crate) fn deploy_entrypoint(&self) -> Result<ContractEntrypointID> {
        self.inner.deploy_entrypoint.ok_or(TestkitError::Offline {
            operation: "deploy",
        })
    }

    pub(crate) fn do_action_entrypoint(&self) -> Result<ContractEntrypointID> {
        self.inner
            .do_action_entrypoint
            .ok_or(TestkitError::Offline { operation: "call" })
    }

    fn require_client(&self, operation: &'static str) -> Result<&HttpClient> {
        self.inner
            .client
            .as_ref()
            .ok_or(TestkitError::Offline { operation })
    }
}
