//! hogd-dev indexer-backed read plane adapter.
//!
//! [`HogdDevReadBackend`] implements [`ReadPlane`] against hogd-dev's indexer
//! x23 endpoint. `transaction_receipt` and `contract_events` are pure indexer
//! calls; `transaction_status` runs the indexer-and-scheduler probe the gateway
//! runs, from `thruput_hogd_backend::status`, so both answer alike.
//! Latest-state storage reads use hogd-dev's `DbGateway` endpoint and can also be
//! served by [`HogdDevStorageReadBackend`] when the indexer is absent.
//!
//! The scheduler gateway client is shared from the chain/submit backend — no
//! second pool is created.

use std::{fmt, net::SocketAddr};

use async_trait::async_trait;
use thruput_db_gateway::{DbGateway_DbGateway1_Client, DbGatewayPayload};
use thruput_hogd_backend::{
    db_gateway as shared_db_gateway, indexer as shared_indexer, intro, receipt as shared_receipt,
    scheduler as shared_scheduler, status as shared_status,
};
use thruput_indexer::{IndexerBlockExplorer_IndexerBlockExplorer1_Client, indexer::TxInfo};
use thruput_json_rpc_server::{ReadPlane, RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::protocol_token::protocol_from_text;
use thruput_json_rpc_types::{
    ContractAddress,
    address_auth::AddressAuthDataResponse,
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{
        AddressTransactionQuery as RpcAddressTransactionQuery, TransactionRefPage,
        TransactionStatus, TxHash,
    },
};
use thruput_scheduler::sched_gateway::SchedGateway_SchedGateway1_Client;
use thruput_x23_transport::{connect, connect_with_config, hogd_large_message_config};
use thruput_xdr::types::{AddressAndKey, StorageKey};

use crate::{
    NodeSupervisorError,
    request_deadline::within_request_deadline,
    service::{DB_GATEWAY, INDEXER, SCHEDULER_GATEWAY},
};

const HOGD_DEV_POOL_SIZE: usize = 2;

/// Indexer-backed read plane for hogd-dev.
///
/// Implements the `ReadPlane` trait: receipts and events come from the indexer;
/// scheduler outcome lookup for `transaction_status` borrows the shared scheduler
/// client from the chain/submit backend.
#[derive(Clone)]
pub(crate) struct HogdDevReadBackend {
    indexer: IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    db_gateway: Option<DbGateway_DbGateway1_Client>,
    scheduler_gateway: SchedGateway_SchedGateway1_Client,
}

impl fmt::Debug for HogdDevReadBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevReadBackend").finish_non_exhaustive()
    }
}

impl HogdDevReadBackend {
    pub(crate) fn db_gateway_client(&self) -> Option<&DbGateway_DbGateway1_Client> {
        self.db_gateway.as_ref()
    }

    /// Connect to hogd-dev's `IndexerBlockExplorer` endpoint and wire the
    /// shared `SchedGateway` client for pending-tx probes.
    ///
    /// Fails hard if the `IndexerBlockExplorer` endpoint is unreachable: the supervisor
    /// should not start if its child's `IndexerBlockExplorer` endpoint is down.
    pub(crate) async fn connect(
        endpoint: SocketAddr,
        gateway_endpoint: Option<SocketAddr>,
        scheduler_gateway: SchedGateway_SchedGateway1_Client,
        protocol_version: &str,
        expected_protocol: Option<&str>,
    ) -> Result<Self, NodeSupervisorError> {
        let indexer = connect_with_config(
            "hogd-dev indexer",
            &endpoint.to_string(),
            HOGD_DEV_POOL_SIZE,
            hogd_large_message_config(),
            IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::IndexerConnect { endpoint, source })?;

        // Probe the indexer with an intro handshake to verify the endpoint
        // is actually the expected indexer service and protocol version.
        // A wrong endpoint (e.g. wrong port, wrong service) would be reported
        // as full startup success but fail on every subsequent read call.
        tracing::debug!(%endpoint, "probing hogd-dev indexer with intro handshake");
        let protocol = protocol_from_text(protocol_version)?;
        let intro_outcome = indexer
            .indexer_block_explorer_intro(&protocol)
            .await
            .map_err(|e| NodeSupervisorError::IndexerRpc {
                op: "indexer_block_explorer_intro",
                error: e.to_string(),
            })?;

        let intro = intro::indexer_block_explorer(intro_outcome);
        if !intro.accepted() {
            return Err(NodeSupervisorError::intro_rejected(
                intro,
                protocol_version,
                expected_protocol,
            ));
        }

        let db_gateway = if let Some(endpoint) = gateway_endpoint {
            Some(connect_db_gateway(endpoint, protocol_version, expected_protocol).await?)
        } else {
            tracing::warn!(
                "hogd-dev did not advertise a DbGateway endpoint; getStorageAt is unavailable"
            );
            None
        };

        Ok(Self {
            indexer,
            db_gateway,
            scheduler_gateway,
        })
    }

    /// Ask the indexer what it knows about one transaction.
    async fn tx_lookup(&self, tx_hash: TxHash) -> Result<shared_indexer::TxLookup, RpcServerError> {
        let hash = shared_receipt::tx_hash_to_wire(tx_hash);
        let result = self
            .indexer
            .get_transaction_info(&hash)
            .await
            .map_err(|source| RpcServerError::BackendUnavailable(format!("{INDEXER}: {source}")))?;
        shared_indexer::tx_lookup(INDEXER, result)
    }
}

/// The supervisor's two status lookups. Neither is metered or bounded here:
/// the caller's request deadline bounds the whole probe.
impl shared_status::StatusProbe for HogdDevReadBackend {
    async fn scheduler_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionStatus>, RpcServerError> {
        let hash = shared_receipt::tx_hash_to_wire(tx_hash);
        let results = self
            .scheduler_gateway
            .schedgateway_lookup_transaction_outcomes(std::slice::from_ref(&hash))
            .await
            .map_err(|error| {
                RpcServerError::BackendUnavailable(format!("{SCHEDULER_GATEWAY}: {error}"))
            })?;
        shared_scheduler::transaction_outcome(SCHEDULER_GATEWAY, &results)
    }

    async fn indexed_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionStatus>, RpcServerError> {
        Ok(shared_indexer::committed_status(
            self.tx_lookup(tx_hash).await?,
        ))
    }
}

/// DbGateway-only read plane for hogd-dev readiness payloads that expose
/// latest-state storage reads without the indexer.
#[derive(Clone)]
pub(crate) struct HogdDevStorageReadBackend {
    db_gateway: DbGateway_DbGateway1_Client,
}

impl fmt::Debug for HogdDevStorageReadBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevStorageReadBackend")
            .finish_non_exhaustive()
    }
}

impl HogdDevStorageReadBackend {
    pub(crate) fn db_gateway_client(&self) -> &DbGateway_DbGateway1_Client {
        &self.db_gateway
    }

    pub(crate) async fn connect(
        endpoint: SocketAddr,
        protocol_version: &str,
        expected_protocol: Option<&str>,
    ) -> Result<Self, NodeSupervisorError> {
        Ok(Self {
            db_gateway: connect_db_gateway(endpoint, protocol_version, expected_protocol).await?,
        })
    }
}

#[async_trait]
impl ReadPlane for HogdDevStorageReadBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionReceipt".into()))
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }

    async fn contract_events(
        &self,
        _context: RpcRequestContext,
        _query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }

    async fn transactions_by_address(
        &self,
        _context: RpcRequestContext,
        _query: RpcAddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionsByAddress".into(),
        ))
    }

    /// Latest-state storage read.
    ///
    /// Returns the hogd-dev `DbGateway1` snapshot block height whether the
    /// item is present or absent, matching the public gateway contract.
    #[tracing::instrument(skip_all, fields(?address, ?key))]
    async fn get_storage_at(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        within_request_deadline(
            context,
            get_storage_at_from_db_gateway(&self.db_gateway, address, key),
        )
        .await
    }

    async fn read_native_hog_balance(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        within_request_deadline(
            context,
            get_native_hog_balance_from_db_gateway(&self.db_gateway, address),
        )
        .await
    }

    async fn read_address_auth_data(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        within_request_deadline(
            context,
            get_address_auth_data_from_db_gateway(&self.db_gateway, address),
        )
        .await
    }
}

#[async_trait]
impl ReadPlane for HogdDevReadBackend {
    /// Return a committed transaction receipt from the indexer, or `None` if
    /// the transaction is not (yet) committed.
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        within_request_deadline(context, async {
            match self.tx_lookup(tx_hash).await? {
                shared_indexer::TxLookup::Found(info) => Ok(Some(receipt(tx_hash, *info))),
                // A recent nondeterministic failure is not a committed record;
                // receipts exist only for committed transactions. The diagnostic
                // surfaces through `transaction_status`.
                shared_indexer::TxLookup::RecentFailure(_) | shared_indexer::TxLookup::NotFound => {
                    Ok(None)
                }
            }
        })
        .await
    }

    /// Status by the probe order shared with the gateway; see
    /// `thruput_hogd_backend::status::transaction_status`. The whole
    /// probe runs under the caller's request deadline.
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        within_request_deadline(context, shared_status::transaction_status(self, tx_hash)).await
    }

    /// Return one page of contract events matching `query`.
    ///
    /// Forwards the paginated query to the indexer. Malformed events (bad
    /// `num_topics` or truncated payload) are dropped with a warning; the
    /// pagination cursor is not affected.
    #[tracing::instrument(skip_all, fields(limit = query.limit))]
    async fn contract_events(
        &self,
        context: RpcRequestContext,
        query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        within_request_deadline(context, async {
            let wire_query: thruput_indexer::indexer::EventQuery = query.into();
            let result = self
                .indexer
                .get_events(&wire_query)
                .await
                .map_err(|source| {
                    RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {source}"))
                })?;
            let page = shared_indexer::event_page(INDEXER, result)?;
            let decoded = shared_indexer::public_event_page(page);
            for dropped in &decoded.dropped {
                tracing::warn!(
                    tx_hash = ?dropped.tx_hash,
                    block_height = ?dropped.block_height,
                    sequence_number = dropped.sequence_number,
                    "dropped malformed event from hogd-dev indexer",
                );
            }
            Ok(decoded.page)
        })
        .await
    }

    /// Return one page of transaction refs from the history of the
    /// query's address, forwarded to the indexer.
    #[tracing::instrument(skip_all, fields(limit = query.limit))]
    async fn transactions_by_address(
        &self,
        context: RpcRequestContext,
        query: RpcAddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        within_request_deadline(context, async {
            let wire_query: thruput_indexer::indexer::AddressTransactionQuery = query.into();
            let result = self
                .indexer
                .get_transactions_by_address(&wire_query)
                .await
                .map_err(|source| {
                    RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {source}"))
                })?;
            let page = shared_indexer::transaction_ref_page(INDEXER, result)?;
            Ok(page.into())
        })
        .await
    }

    /// Latest-state storage read.
    ///
    /// Returns the hogd-dev `DbGateway1` snapshot block height whether the
    /// item is present or absent, matching the public gateway contract.
    #[tracing::instrument(skip_all, fields(?address, ?key))]
    async fn get_storage_at(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        let db_gateway = self
            .db_gateway
            .as_ref()
            .ok_or_else(|| RpcServerError::BackendUnavailable("db_gateway not connected".into()))?;
        within_request_deadline(
            context,
            get_storage_at_from_db_gateway(db_gateway, address, key),
        )
        .await
    }

    async fn read_native_hog_balance(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        let db_gateway = self
            .db_gateway
            .as_ref()
            .ok_or_else(|| RpcServerError::BackendUnavailable("db_gateway not connected".into()))?;
        within_request_deadline(
            context,
            get_native_hog_balance_from_db_gateway(db_gateway, address),
        )
        .await
    }

    async fn read_address_auth_data(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        let db_gateway = self
            .db_gateway
            .as_ref()
            .ok_or_else(|| RpcServerError::BackendUnavailable("db_gateway not connected".into()))?;
        within_request_deadline(
            context,
            get_address_auth_data_from_db_gateway(db_gateway, address),
        )
        .await
    }
}

async fn connect_db_gateway(
    endpoint: SocketAddr,
    protocol_version: &str,
    expected_protocol: Option<&str>,
) -> Result<DbGateway_DbGateway1_Client, NodeSupervisorError> {
    tracing::debug!(%endpoint, "connecting to hogd-dev DbGateway");
    let client = connect(
        "hogd-dev DbGateway",
        &endpoint.to_string(),
        HOGD_DEV_POOL_SIZE,
        DbGateway_DbGateway1_Client::new,
    )
    .await
    .map_err(|source| NodeSupervisorError::DbGatewayConnect { endpoint, source })?;

    let protocol = protocol_from_text(protocol_version)?;
    let intro_outcome =
        client
            .dbgateway_intro(&protocol)
            .await
            .map_err(|e| NodeSupervisorError::DbGatewayRpc {
                op: "dbgateway_intro",
                error: e.to_string(),
            })?;
    let intro = intro::db_gateway(intro_outcome);
    if !intro.accepted() {
        return Err(NodeSupervisorError::intro_rejected(
            intro,
            protocol_version,
            expected_protocol,
        ));
    }

    Ok(client)
}

async fn get_storage_at_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
    key: [u8; 32],
) -> Result<StorageItemRead, RpcServerError> {
    get_item_from_db_gateway(
        db_gateway,
        address,
        shared_db_gateway::contract_data_key(key),
    )
    .await
}

async fn get_native_hog_balance_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
) -> Result<StorageItemRead, RpcServerError> {
    get_item_from_db_gateway(db_gateway, address, StorageKey::FreeNativeTokenBalance).await
}

async fn get_address_auth_data_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
) -> Result<AddressAuthDataResponse, RpcServerError> {
    let payload = get_payload_from_db_gateway(db_gateway, address, StorageKey::AuthData).await?;
    let (block_height, item) = shared_db_gateway::single_item(DB_GATEWAY, payload)?;
    shared_db_gateway::address_auth_data_read(DB_GATEWAY, address, block_height, item)
}

async fn get_item_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
    key: StorageKey,
) -> Result<StorageItemRead, RpcServerError> {
    let payload = get_payload_from_db_gateway(db_gateway, address, key).await?;
    shared_db_gateway::single_item_read(DB_GATEWAY, payload)
}

async fn get_payload_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
    key: StorageKey,
) -> Result<DbGatewayPayload, RpcServerError> {
    let request = AddressAndKey {
        address: address.0,
        key,
    };
    let outer = db_gateway
        .dbgateway_get_items(std::slice::from_ref(&request))
        .await
        .map_err(|source| RpcServerError::BackendUnavailable(format!("{DB_GATEWAY}: {source}")))?;
    shared_db_gateway::payload(DB_GATEWAY, outer)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a public receipt from the indexer's committed record.
fn receipt(tx_hash: TxHash, info: TxInfo) -> TransactionReceipt {
    let decoded = shared_receipt::transaction_receipt(tx_hash, info.height, info.events);
    if decoded.dropped_events > 0 {
        tracing::warn!(
            dropped = decoded.dropped_events,
            ?tx_hash,
            "dropped malformed events from hogd-dev indexer receipt",
        );
    }
    decoded.receipt
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
