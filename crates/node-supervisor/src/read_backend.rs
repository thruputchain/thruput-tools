//! hogd-dev indexer-backed read adapters.
//!
//! [`HogdDevReadBackend`] serves native receipts, status and history from
//! hogd-dev's indexer x23 endpoint, with scheduler lookups for status. It also
//! answers local funding status through `thruput_hogd_backend::status`.
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
use thruput_indexer::IndexerBlockExplorer_IndexerBlockExplorer1_Client;
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};

use thruput_json_rpc_types::protocol_token::protocol_from_text;
use thruput_json_rpc_types::{
    ContractAddress,
    transaction::{TransactionStatus, TxHash},
};
use thruput_scheduler::sched_gateway::SchedGateway_SchedGateway1_Client;
use thruput_x23_transport::{connect, connect_with_config, hogd_large_message_config};
use thruput_xdr::types::{AddressAndKey, StorageKey};

use crate::airdrop::FundingStatusPlane;
use crate::{
    NodeSupervisorError,
    request_deadline::within_request_deadline,
    service::{DB_GATEWAY, INDEXER, SCHEDULER_GATEWAY},
};

const HOGD_DEV_POOL_SIZE: usize = 2;

/// Indexer-backed read plane for hogd-dev.
///
/// Receipts and history come from the indexer; status lookups borrow the
/// shared scheduler client from the chain/submit backend.
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
    /// Share the validated indexer connection with the chain adapter.
    pub(crate) fn indexer_client(&self) -> &IndexerBlockExplorer_IndexerBlockExplorer1_Client {
        &self.indexer
    }

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
        shared_indexer::tx_lookup(INDEXER, tx_hash, self.transaction_info(tx_hash).await?)
    }

    /// Raw indexer response for projections that retain native evidence.
    async fn transaction_info(
        &self,
        tx_hash: TxHash,
    ) -> Result<thruput_indexer::indexer::TxInfoResult, RpcServerError> {
        let hash = shared_receipt::tx_hash_to_wire(tx_hash);
        self.indexer
            .get_transaction_info(&hash)
            .await
            .map_err(|source| RpcServerError::BackendUnavailable(format!("{INDEXER}: {source}")))
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
impl FundingStatusPlane for HogdDevStorageReadBackend {
    async fn funding_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }
}

#[async_trait]
impl FundingStatusPlane for HogdDevReadBackend {
    /// Status by the probe order in
    /// `thruput_hogd_backend::status::transaction_status`. The whole probe
    /// runs under the caller's request deadline.
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn funding_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        within_request_deadline(context, shared_status::transaction_status(self, tx_hash)).await
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

mod native;
