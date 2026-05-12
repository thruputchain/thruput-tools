//! hogd-dev indexer-backed read plane adapter.
//!
//! [`HogdDevReadBackend`] implements [`ReadPlane`] against hogd-dev's indexer
//! x23 endpoint. `transaction_receipt` and `contract_events` are pure indexer
//! calls; `transaction_status` does the composite scheduler-then-indexer probe
//! that the gateway uses (scheduler for `Pending`, indexer for `Committed`).
//! Latest-state storage reads use hogd-dev's `DbGateway` endpoint and can also be
//! served by [`HogdDevStorageReadBackend`] when the indexer is absent.
//!
//! The scheduler client is shared from [`HogdDevSchedulerBackend`] — no
//! second pool is created.

use std::{fmt, net::SocketAddr};

use async_trait::async_trait;
use groundhog_db_gateway::{
    AddressAndKey, AddressAndKeyBatch, DbGateway_DbGateway1_Client, DbGatewayIntroOutcome,
    DbGatewayItemResult, DbGatewayResult, Protocol as DbGatewayProtocol, RPCError, StorageKey,
    StorageKeyContractData,
};
use groundhog_indexer::{
    IndexerBlockExplorer_IndexerBlockExplorer1_Client, IndexerBlockExplorerIntroOutcome, Protocol,
    TxInfoOutcome, TxInfoResult,
};
use groundhog_rpc_api_types::{
    ContractAddress,
    events::{EventPage, EventQuery, IdentifiedEvent},
    hex::{Hex0x, HexBytes, Topic},
    receipt::{ExecStatus, TransactionReceipt},
    state::{StorageItemInfo, StorageItemRead},
    transaction::{TransactionStatus, TxHash},
};
use groundhog_rpc_server::{ReadPlane, RpcServerError};
use groundhog_scheduler::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use groundhog_x23_transport::connect;
use groundhog_xdr::{
    event::TxEmittedEvent,
    exec_result::ExecResult,
    transaction::{InvocationFrameOutput, SignedTransactionHash},
};
use xdr_common::VecM;

use crate::{NodeSupervisorError, rpc_backend::HogdDevSchedulerBackend};

const HOGD_DEV_POOL_SIZE: usize = 2;
const TOPIC_SIZE: usize = 32;
const MAX_TOPICS: u32 = 3;

/// Indexer-backed read plane for hogd-dev.
///
/// Implements the `ReadPlane` trait: receipts and events come from the indexer;
/// pending-tx detection for `transaction_status` borrows the shared scheduler
/// client from the chain/submit backend.
#[derive(Clone)]
pub(crate) struct HogdDevReadBackend {
    indexer: IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    db_gateway: Option<DbGateway_DbGateway1_Client>,
    scheduler: SchedulerCoord_SchedulerCoord1_Client,
}

impl fmt::Debug for HogdDevReadBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HogdDevReadBackend").finish_non_exhaustive()
    }
}

impl HogdDevReadBackend {
    /// Connect to hogd-dev's indexer endpoint and wire the shared scheduler
    /// client for pending-tx probes.
    ///
    /// Fails hard if the indexer endpoint is unreachable — the supervisor
    /// should not start if its child's read endpoint is down.
    pub(crate) async fn connect(
        endpoint: SocketAddr,
        gateway_endpoint: Option<SocketAddr>,
        scheduler_backend: &HogdDevSchedulerBackend,
        protocol_version: &str,
    ) -> Result<Self, NodeSupervisorError> {
        let indexer = connect(
            "hogd-dev indexer",
            &endpoint.to_string(),
            HOGD_DEV_POOL_SIZE,
            IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::IndexerConnect { endpoint, source })?;

        // Probe the indexer with an intro handshake to verify the endpoint
        // is actually the expected indexer service and protocol version.
        // A wrong endpoint (e.g. wrong port, wrong service) would be reported
        // as full startup success but fail on every subsequent read call.
        tracing::debug!(%endpoint, "probing hogd-dev indexer with intro handshake");
        let protocol: Protocol = xdr_common::StringM(protocol_version.as_bytes().to_vec());
        let intro_outcome = indexer
            .indexer_block_explorer_intro(&protocol)
            .await
            .map_err(|e| NodeSupervisorError::IndexerRpc {
                op: "indexer_block_explorer_intro",
                error: e.to_string(),
            })?;

        if intro_outcome != IndexerBlockExplorerIntroOutcome::IntroSuccess {
            return Err(NodeSupervisorError::IndexerIntroRejected {
                outcome: intro_outcome.0,
            });
        }

        let db_gateway = if let Some(endpoint) = gateway_endpoint {
            Some(connect_db_gateway(endpoint, protocol_version).await?)
        } else {
            tracing::warn!(
                "hogd-dev did not advertise a gateway endpoint; getStorageAt is unavailable"
            );
            None
        };

        Ok(Self {
            indexer,
            db_gateway,
            scheduler: scheduler_backend.scheduler_client().clone(),
        })
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
    pub(crate) async fn connect(
        endpoint: SocketAddr,
        protocol_version: &str,
    ) -> Result<Self, NodeSupervisorError> {
        Ok(Self {
            db_gateway: connect_db_gateway(endpoint, protocol_version).await?,
        })
    }
}

#[async_trait]
impl ReadPlane for HogdDevStorageReadBackend {
    async fn transaction_receipt(
        &self,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionReceipt".into()))
    }

    async fn transaction_status(
        &self,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented("transactionStatus".into()))
    }

    async fn contract_events(&self, _query: EventQuery) -> Result<EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }

    /// Latest-state storage read.
    ///
    /// Returns the hogd-dev `DbGateway1` snapshot block height whether the
    /// item is present or absent, matching the public gateway contract.
    #[tracing::instrument(skip_all, fields(?address, ?key))]
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        get_storage_at_from_db_gateway(&self.db_gateway, address, key).await
    }
}

#[async_trait]
impl ReadPlane for HogdDevReadBackend {
    /// Return a committed transaction receipt from the indexer, or `None` if
    /// the transaction is not (yet) committed.
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        let hash = tx_hash_to_signed(tx_hash);
        let result = self
            .indexer
            .get_transaction_info(&hash)
            .await
            .map_err(|source| {
                RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {source}"))
            })?;

        match result {
            TxInfoResult::TxInfoFound { info } => {
                let events = info
                    .events
                    .events
                    .iter()
                    .filter_map(parse_emitted_event)
                    .collect();

                let return_data = match &info.events.root_output {
                    InvocationFrameOutput::Return { return_buffer } => {
                        Some(HexBytes(return_buffer.0.clone()))
                    }
                    InvocationFrameOutput::Abort { abort_buffer } => {
                        Some(HexBytes(abort_buffer.0.clone()))
                    }
                    _ => None,
                };

                Ok(Some(TransactionReceipt {
                    tx_hash,
                    height: info.height,
                    status: map_exec_result(info.events.exec_result),
                    events,
                    return_data,
                }))
            }
            TxInfoResult::Default { outcome } if outcome == TxInfoOutcome::TxInfoNotFound.0 => {
                Ok(None)
            }
            TxInfoResult::Default { outcome } => {
                tracing::warn!(%outcome, "unexpected TxInfoResult outcome from hogd-dev indexer");
                Err(RpcServerError::BackendUnavailable(format!(
                    "hogd-dev indexer returned unexpected outcome: {outcome}"
                )))
            }
            _ => {
                tracing::warn!("unknown TxInfoResult variant from hogd-dev indexer");
                Err(RpcServerError::BackendUnavailable(
                    "hogd-dev indexer returned unknown TxInfoResult variant".into(),
                ))
            }
        }
    }

    /// Return the semantic transaction status.
    ///
    /// Probe order:
    /// 1. Scheduler mempool → `Pending` if found.
    /// 2. Indexer committed set → `Committed { height }` if found.
    /// 3. Neither → `Unknown`.
    ///
    /// Degraded mode: if the scheduler is unreachable, log a warning and skip
    /// the pending check. Indexer failure is a hard error — without it we
    /// cannot report committed state.
    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn transaction_status(
        &self,
        tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        let hash = tx_hash_to_signed(tx_hash);

        // 1. Check scheduler (best-effort — failure degrades to indexer-only).
        let batch: VecM<SignedTransactionHash> = VecM(vec![hash]);
        match self.scheduler.schedcoord_lookup_transactions(&batch).await {
            Ok(results) => {
                if results.iter().next().is_some_and(Option::is_some) {
                    return Ok(TransactionStatus::Pending);
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "hogd-dev scheduler unreachable, cannot detect pending txs"
                );
            }
        }

        // 2. Check indexer (authoritative for committed state).
        match self.indexer.get_transaction_info(&hash).await {
            Ok(TxInfoResult::TxInfoFound { info }) => {
                return Ok(TransactionStatus::Committed {
                    height: info.height,
                });
            }
            Ok(TxInfoResult::Default { outcome }) if outcome == TxInfoOutcome::TxInfoNotFound.0 => {
            }
            Ok(TxInfoResult::Default { outcome }) => {
                tracing::warn!(%outcome, "unexpected TxInfoResult outcome from hogd-dev indexer");
                return Err(RpcServerError::BackendUnavailable(format!(
                    "hogd-dev indexer returned unexpected outcome: {outcome}"
                )));
            }
            Ok(_) => {
                tracing::warn!("unknown TxInfoResult variant from hogd-dev indexer");
                return Err(RpcServerError::BackendUnavailable(
                    "hogd-dev indexer returned unknown TxInfoResult variant".into(),
                ));
            }
            Err(e) => {
                return Err(RpcServerError::BackendUnavailable(format!(
                    "hogd-dev indexer: {e}"
                )));
            }
        }

        Ok(TransactionStatus::Unknown)
    }

    /// Return one page of contract events matching `query`.
    ///
    /// Forwards the paginated query to the indexer. Malformed events (bad
    /// `num_topics` or truncated payload) are dropped with a warning; the
    /// pagination cursor is not affected.
    #[tracing::instrument(skip_all, fields(limit = query.limit))]
    async fn contract_events(&self, query: EventQuery) -> Result<EventPage, RpcServerError> {
        let wire_query: groundhog_indexer::indexer::EventQuery = query.into();
        let page = self
            .indexer
            .get_events(&wire_query)
            .await
            .map_err(|source| {
                RpcServerError::BackendUnavailable(format!("hogd-dev indexer: {source}"))
            })?;

        let mut events: Vec<IdentifiedEvent> = Vec::with_capacity(page.events.0.len());
        for identified in &page.events.0 {
            if let Some(event) = parse_emitted_event(&identified.emitted_event) {
                events.push(IdentifiedEvent {
                    tx_hash: TxHash(Hex0x(identified.proposed_tx_hash)),
                    block_height: identified.block_height.clone(),
                    sequence_number: identified.sequence_number,
                    event,
                });
            } else {
                tracing::warn!(
                    tx_hash = ?identified.proposed_tx_hash,
                    block_height = ?identified.block_height,
                    sequence_number = identified.sequence_number,
                    "dropped malformed event from hogd-dev indexer",
                );
            }
        }

        Ok(EventPage {
            events,
            cursor: page.cursor.into(),
            latest_block: page.latest_block,
            oldest_block: page.oldest_block,
        })
    }

    /// Latest-state storage read.
    ///
    /// Returns the hogd-dev `DbGateway1` snapshot block height whether the
    /// item is present or absent, matching the public gateway contract.
    #[tracing::instrument(skip_all, fields(?address, ?key))]
    async fn get_storage_at(
        &self,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        let db_gateway = self
            .db_gateway
            .as_ref()
            .ok_or_else(|| RpcServerError::BackendUnavailable("db_gateway not connected".into()))?;
        get_storage_at_from_db_gateway(db_gateway, address, key).await
    }
}

async fn connect_db_gateway(
    endpoint: SocketAddr,
    protocol_version: &str,
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

    let protocol: DbGatewayProtocol = xdr_common::StringM(protocol_version.as_bytes().to_vec());
    let intro_outcome =
        client
            .dbgateway_intro(&protocol)
            .await
            .map_err(|e| NodeSupervisorError::DbGatewayRpc {
                op: "dbgateway_intro",
                error: e.to_string(),
            })?;
    if intro_outcome != DbGatewayIntroOutcome::DbGatewayIntroSuccess {
        return Err(NodeSupervisorError::DbGatewayIntroRejected {
            outcome: intro_outcome.0,
        });
    }

    Ok(client)
}

async fn get_storage_at_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
    key: [u8; 32],
) -> Result<StorageItemRead, RpcServerError> {
    let batch: AddressAndKeyBatch = VecM(vec![AddressAndKey {
        address: address.0,
        key: StorageKey::ContractData {
            contract_data_key: StorageKeyContractData::from(key),
        },
    }]);
    let outer = db_gateway
        .dbgateway_get_items(&batch)
        .await
        .map_err(|source| {
            RpcServerError::BackendUnavailable(format!("hogd-dev DbGateway: {source}"))
        })?;
    let payload = match outer {
        DbGatewayResult::kRPCNone { payload } => payload,
        DbGatewayResult::Default { error } => {
            return Err(RpcServerError::BackendUnavailable(format!(
                "hogd-dev DbGateway: {}",
                RPCError(error)
            )));
        }
        unexpected => {
            tracing::warn!(
                ?unexpected,
                "hogd-dev DbGateway returned unsupported outer result variant"
            );
            return Err(RpcServerError::BackendUnavailable(
                "hogd-dev DbGateway returned an unsupported result variant; regenerate the \
                 DbGateway client for this protocol"
                    .into(),
            ));
        }
    };

    let item = payload.items.0.into_iter().next().ok_or_else(|| {
        RpcServerError::BackendUnavailable(
            "hogd-dev DbGateway response had fewer items than the batch".into(),
        )
    })?;
    // `payload.block_height` is the freshness boundary for the whole
    // batch, including kRPCNotFound item misses.
    match item {
        DbGatewayItemResult::kRPCNone { item } => Ok(StorageItemRead {
            block_height: payload.block_height,
            item: Some(StorageItemInfo::from(item)),
        }),
        DbGatewayItemResult::Default { error } => {
            let err = RPCError(error);
            if err == RPCError::kRPCNotFound {
                Ok(StorageItemRead {
                    block_height: payload.block_height,
                    item: None,
                })
            } else {
                Err(RpcServerError::BackendUnavailable(format!(
                    "hogd-dev DbGateway item: {err}"
                )))
            }
        }
        unexpected => {
            tracing::warn!(
                ?unexpected,
                "hogd-dev DbGateway returned unsupported per-item result variant"
            );
            Err(RpcServerError::BackendUnavailable(
                "hogd-dev DbGateway returned an unsupported per-item result variant; \
                 regenerate the DbGateway client for this protocol"
                    .into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert the RPC-layer `TxHash` to the wire `SignedTransactionHash`.
fn tx_hash_to_signed(tx_hash: TxHash) -> SignedTransactionHash {
    SignedTransactionHash::from(tx_hash.0.0.into_inner())
}

/// Map hogd's `ExecResult` (XDR int enum) to the API-types `ExecStatus`.
///
/// Mirrors `gateway/src/handlers/receipt.rs::map_exec_result` exactly.
/// TODO: share with gateway once a common helper crate exists.
fn map_exec_result(r: ExecResult) -> ExecStatus {
    if r == ExecResult::ExecResultOK {
        ExecStatus::Ok
    } else if r == ExecResult::ExecResultTxRootFrameReverted {
        ExecStatus::Reverted { message: None }
    } else if r == ExecResult::ExecResultTxDeterministicError {
        ExecStatus::DeterministicError {
            kind: "unknown".into(),
        }
    } else if r == ExecResult::ExecResultTxNonDeterministicError {
        ExecStatus::NonDeterministicError
    } else if r == ExecResult::ExecResultFailedGasSponsorship {
        ExecStatus::FailedGasSponsorship
    } else {
        ExecStatus::Other
    }
}

/// Parse a hogd `TxEmittedEvent` into an `EmittedEvent` with split topics and data.
///
/// Returns `None` if the event is malformed (`num_topics` > 3, or
/// `serialized_event` is too short for the declared number of topics).
///
/// Mirrors `gateway/src/handlers/parse_event.rs::parse_emitted_event` exactly.
/// TODO: share with gateway once a common helper crate exists.
fn parse_emitted_event(
    raw: &TxEmittedEvent,
) -> Option<groundhog_rpc_api_types::events::EmittedEvent> {
    let num_topics = raw.num_topics;
    if num_topics > MAX_TOPICS {
        tracing::warn!(num_topics, "event has too many topics, skipping");
        return None;
    }

    let bytes: &[u8] = raw.serialized_event.as_ref();
    let topics_len = num_topics as usize * TOPIC_SIZE;
    if bytes.len() < topics_len {
        tracing::warn!(
            num_topics,
            event_len = bytes.len(),
            "serialized_event too short for declared topics, skipping"
        );
        return None;
    }

    let mut topics = Vec::with_capacity(num_topics as usize);
    for i in 0..num_topics as usize {
        let start = i * TOPIC_SIZE;
        let mut topic = [0u8; TOPIC_SIZE];
        topic.copy_from_slice(&bytes[start..start + TOPIC_SIZE]);
        topics.push(Topic(topic));
    }

    let data = bytes[topics_len..].to_vec();

    Some(groundhog_rpc_api_types::events::EmittedEvent {
        contract_address: Hex0x(raw.contract_address),
        topics,
        data: HexBytes(data),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::SocketAddr, time::Duration};

    use async_trait::async_trait;
    use groundhog_db_gateway::{
        AddressAndKeyBatch, DbGateway_DbGateway1, DbGateway_DbGateway1_Client,
        DbGateway_DbGateway1_Dispatcher, DbGatewayIntroOutcome, DbGatewayItemResult,
        DbGatewayPayload, DbGatewayResult, Protocol as DbGatewayProtocol, RPCError,
    };
    use groundhog_indexer::{
        BlockHeight as IdxBlockHeight, EventPage as IdxEventPage,
        IdentifiedEvent as IdxIdentifiedEvent, IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
        Protocol, TxInfo, TxInfoResult,
    };
    use groundhog_rpc_api_types::{
        events::{EventFilterList, EventQuery},
        hex::Hex0x,
        transaction::{TransactionStatus, TxHash},
    };
    use groundhog_rpc_server::{ReadPlane, RpcServerError};
    use groundhog_scheduler::{
        BlockID, CoordinatorRegistrationOutcome, DbSchedResult, GenesisConfiguration, Invocation,
        Nomination, OptSignedTransactionBatch, ParallelSignedTransactionBatch,
        SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerNominationResult,
        SchedulerValidationResult, SignedTransaction, SignedTransactionHashBatch, Transaction,
        Uniquifier, ValidatorID,
        sched_coord::{
            SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Client,
            SchedulerCoord_SchedulerCoord1_Dispatcher,
        },
    };
    use groundhog_xdr::{
        PROTOCOL_VERSION,
        event::TxEmittedEvent,
        exec_result::ExecResult,
        storage_item::WireStorageItem,
        transaction::SignedTransactionHash,
        types::{ContractAddress, SHA256Hash},
    };
    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;
    use xdr_common::{BytesM, VecM};

    use crate::rpc_backend::HogdDevSchedulerBackend;

    use super::HogdDevReadBackend;

    // These constants mirror the fake_hogd_dev fixture. Defined here so the
    // unit tests in this file have no dependency on the example binary's types.
    const CANNED_TX_HASH_A: [u8; 32] = [0xAAu8; 32];
    const CANNED_TX_HASH_B: [u8; 32] = [0xBBu8; 32];
    const CANNED_TX_HASH_UNKNOWN: [u8; 32] = [0xFFu8; 32];
    const CANNED_HEIGHT_A: i64 = 5;
    const CANNED_HEIGHT_B: i64 = 10;
    const CANNED_EVENT_CURSOR: &[u8] = b"cursor-page-2";
    const CANNED_STORAGE_KEY: [u8; 32] = [0x42u8; 32];
    const CANNED_STORAGE_VALUE: &[u8] = b"local-node-storage";
    const CANNED_STORAGE_HEIGHT: i64 = 12;

    // -----------------------------------------------------------------------
    // Fake scheduler
    // -----------------------------------------------------------------------

    /// Controls what `schedcoord_lookup_transactions` returns.
    #[derive(Debug, Clone, Copy)]
    enum FakeSchedulerMode {
        /// Returns empty batch — nothing pending.
        NoPending,
        /// Returns one `Some(dummy_tx)` per queried hash — all pending.
        AllPending,
        /// RPC call itself fails.
        RpcFail,
    }

    struct FakeScheduler {
        mode: FakeSchedulerMode,
    }

    /// Build a minimal dummy `SignedTransaction` suitable for testing the
    /// pending-detection path. The transaction is not valid on-chain; it only
    /// needs to be `Some(...)` so `Option::is_some()` returns true.
    fn dummy_signed_transaction() -> SignedTransaction {
        use groundhog_scheduler::{ContractEntrypointID, GasAmount};
        use groundhog_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
        SignedTransaction {
            tx: Transaction {
                invocation: Invocation {
                    invoked_address: ContractAddress::from([0u8; 32]),
                    entrypoint: ContractEntrypointID::from([0u8; 8]),
                    gas_limit: GasAmount::from(0u64),
                    calldata: BytesM(vec![]),
                },
                uniquifier: Uniquifier {
                    block_expiry_height: IdxBlockHeight::from(0i64),
                    nonce: 0,
                },
                contracts_to_deploy: VecM(vec![]),
                auth_claim: AuthClaim::kSourceAddressAuthData {
                    source_address_claim: AuthClaim_source_address_claim {
                        allowed_keys: 0,
                        required_weight: 0,
                    },
                },
            },
            signatures: VecM(vec![]),
        }
    }

    #[async_trait]
    impl SchedulerCoord_SchedulerCoord1 for FakeScheduler {
        async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
            Ok(())
        }

        async fn schedulercoord_intro_coord(
            &self,
            _card: groundhog_scheduler::CoordCard,
        ) -> Result<CoordinatorRegistrationOutcome, ProgramError> {
            Ok(CoordinatorRegistrationOutcome::CoordinatorRegistrationOK)
        }

        async fn schedulercoord_submit_unvetted(
            &self,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            Ok(())
        }

        async fn schedulercoord_submit_vetted(
            &self,
            _validator_id: ValidatorID,
            _batch: ParallelSignedTransactionBatch,
        ) -> Result<(), ProgramError> {
            Ok(())
        }

        async fn schedulercoord_nominate(
            &self,
            _onto: groundhog_scheduler::DbTip,
        ) -> Result<SchedulerNominationResult, ProgramError> {
            Err(ProgramError::msg("not implemented in fake"))
        }

        async fn schedulercoord_validate(
            &self,
            _onto: groundhog_scheduler::DbTip,
            _nomination: Nomination,
        ) -> Result<SchedulerValidationResult, ProgramError> {
            Err(ProgramError::msg("not implemented in fake"))
        }

        async fn schedulercoord_commit_block(
            &self,
            _block_id: BlockID,
        ) -> Result<SchedulerCommitOutcome, ProgramError> {
            Err(ProgramError::msg("not implemented in fake"))
        }

        async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
            Ok(SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock)
        }

        async fn schedcoord_load_genesis_configuration(
            &self,
            _config: GenesisConfiguration,
        ) -> Result<DbSchedResult, ProgramError> {
            Ok(DbSchedResult::DbSchedResultOK)
        }

        async fn schedcoord_lookup_transactions(
            &self,
            hashes: SignedTransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            match self.mode {
                FakeSchedulerMode::NoPending => Ok(VecM(vec![])),
                FakeSchedulerMode::AllPending => {
                    // Return one Some(dummy_tx) per queried hash to signal pending.
                    let results = hashes
                        .0
                        .into_iter()
                        .map(|_| Some(dummy_signed_transaction()))
                        .collect();
                    Ok(VecM(results))
                }
                FakeSchedulerMode::RpcFail => {
                    Err(ProgramError::msg("fake scheduler lookup rpc-fail"))
                }
            }
        }
    }

    async fn start_fake_scheduler(
        mode: FakeSchedulerMode,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test scheduler should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
            Box::new(FakeScheduler { mode }),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    // -----------------------------------------------------------------------
    // Fake indexer
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeIndexerMode {
        Normal,
        RpcFail,
    }

    type CommittedSet = HashMap<[u8; 32], TxInfo>;
    type EventPages = Vec<(Vec<IdxIdentifiedEvent>, Vec<u8>)>;

    struct FakeIndexer {
        mode: FakeIndexerMode,
        committed: CommittedSet,
        event_pages: EventPages,
    }

    impl FakeIndexer {
        fn new(mode: FakeIndexerMode) -> Self {
            Self {
                mode,
                committed: Self::build_committed_set(),
                event_pages: Self::build_event_pages(),
            }
        }

        fn build_committed_set() -> CommittedSet {
            use groundhog_xdr::{event::TxEventLog, transaction::InvocationFrameOutput};
            use xdr_common::BytesM;

            let mut map = HashMap::new();

            let tx_a = TxInfo {
                height: IdxBlockHeight::from(CANNED_HEIGHT_A),
                events: TxEventLog {
                    proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                    exec_result: ExecResult::ExecResultOK,
                    root_output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![]),
                    },
                    events: VecM(vec![]),
                },
            };
            map.insert(CANNED_TX_HASH_A, tx_a);

            let tx_b = TxInfo {
                height: IdxBlockHeight::from(CANNED_HEIGHT_B),
                events: TxEventLog {
                    proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
                    exec_result: ExecResult::ExecResultOK,
                    root_output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![]),
                    },
                    events: VecM(vec![]),
                },
            };
            map.insert(CANNED_TX_HASH_B, tx_b);

            map
        }

        fn build_event_pages() -> EventPages {
            use groundhog_xdr::types::ContractAddress;

            let event1 = IdxIdentifiedEvent {
                proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                block_height: IdxBlockHeight::from(CANNED_HEIGHT_A),
                sequence_number: 0,
                emitted_event: TxEmittedEvent {
                    contract_address: ContractAddress::from([0x01u8; 32]),
                    num_topics: 0,
                    serialized_event: BytesM(vec![]),
                },
            };
            let event2 = IdxIdentifiedEvent {
                proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                block_height: IdxBlockHeight::from(CANNED_HEIGHT_A),
                sequence_number: 1,
                emitted_event: TxEmittedEvent {
                    contract_address: ContractAddress::from([0x01u8; 32]),
                    num_topics: 0,
                    serialized_event: BytesM(vec![]),
                },
            };
            let event3 = IdxIdentifiedEvent {
                proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
                block_height: IdxBlockHeight::from(CANNED_HEIGHT_B),
                sequence_number: 0,
                emitted_event: TxEmittedEvent {
                    contract_address: ContractAddress::from([0x02u8; 32]),
                    num_topics: 0,
                    serialized_event: BytesM(vec![]),
                },
            };

            vec![
                (vec![event1, event2], CANNED_EVENT_CURSOR.to_vec()),
                (vec![event3], vec![]),
            ]
        }

        fn resolve_event_page(
            &self,
            cursor: &[u8],
        ) -> Result<&(Vec<IdxIdentifiedEvent>, Vec<u8>), ProgramError> {
            if cursor.is_empty() {
                Ok(&self.event_pages[0])
            } else {
                self.event_pages
                    .iter()
                    .enumerate()
                    .find(|(_, (_, next))| !next.is_empty() && next.as_slice() == cursor)
                    .map(|(i, _)| &self.event_pages[i + 1])
                    .ok_or_else(|| {
                        ProgramError::msg(
                            "unknown event cursor; test indexer only handles canned dataset",
                        )
                    })
            }
        }
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for FakeIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _arg0: Protocol,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            arg0: SignedTransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            let key: [u8; 32] = arg0.into_inner();
            match self.committed.get(&key) {
                Some(info) => Ok(TxInfoResult::TxInfoFound { info: info.clone() }),
                None => Ok(TxInfoResult::Default {
                    outcome: groundhog_indexer::TxInfoOutcome::TxInfoNotFound.0,
                }),
            }
        }

        async fn get_events(
            &self,
            arg0: groundhog_indexer::EventQuery,
        ) -> Result<IdxEventPage, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            let (events, next_cursor) = self.resolve_event_page(&arg0.cursor.0)?;
            Ok(IdxEventPage {
                events: VecM(events.clone()),
                cursor: BytesM(next_cursor.clone()),
                latest_block: IdxBlockHeight::from(CANNED_HEIGHT_B),
                oldest_block: IdxBlockHeight::from(0_i64),
            })
        }
    }

    async fn start_fake_indexer(
        mode: FakeIndexerMode,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test indexer should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test indexer should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(FakeIndexer::new(
                mode,
            ))),
        ));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    // -----------------------------------------------------------------------
    // Fake DbGateway
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeDbGatewayMode {
        Found,
        NotFound,
        RpcFail,
        OuterDefault,
        EmptyPayload,
        ItemDefaultError,
    }

    struct FakeDbGateway {
        mode: FakeDbGatewayMode,
    }

    #[async_trait]
    impl DbGateway_DbGateway1 for FakeDbGateway {
        async fn dbgateway_intro(
            &self,
            _arg0: DbGatewayProtocol,
        ) -> Result<DbGatewayIntroOutcome, ProgramError> {
            Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
        }

        async fn dbgateway_get_items(
            &self,
            _arg0: AddressAndKeyBatch,
        ) -> Result<DbGatewayResult, ProgramError> {
            match self.mode {
                FakeDbGatewayMode::Found => Ok(DbGatewayResult::kRPCNone {
                    payload: DbGatewayPayload {
                        block_height: IdxBlockHeight::from(CANNED_STORAGE_HEIGHT),
                        items: VecM(vec![DbGatewayItemResult::kRPCNone {
                            item: WireStorageItem::kItemTypeBytestring {
                                bytestring: BytesM(CANNED_STORAGE_VALUE.to_vec()),
                            },
                        }]),
                    },
                }),
                FakeDbGatewayMode::NotFound => Ok(DbGatewayResult::kRPCNone {
                    payload: DbGatewayPayload {
                        block_height: IdxBlockHeight::from(CANNED_STORAGE_HEIGHT),
                        items: VecM(vec![DbGatewayItemResult::Default {
                            error: RPCError::kRPCNotFound.0,
                        }]),
                    },
                }),
                FakeDbGatewayMode::RpcFail => Err(ProgramError::msg("fake DbGateway rpc-fail")),
                FakeDbGatewayMode::OuterDefault => Ok(DbGatewayResult::Default {
                    error: RPCError::kRPCUnrecoverable.0,
                }),
                FakeDbGatewayMode::EmptyPayload => Ok(DbGatewayResult::kRPCNone {
                    payload: DbGatewayPayload {
                        block_height: IdxBlockHeight::from(CANNED_STORAGE_HEIGHT),
                        items: VecM(vec![]),
                    },
                }),
                FakeDbGatewayMode::ItemDefaultError => Ok(DbGatewayResult::kRPCNone {
                    payload: DbGatewayPayload {
                        block_height: IdxBlockHeight::from(CANNED_STORAGE_HEIGHT),
                        items: VecM(vec![DbGatewayItemResult::Default {
                            error: RPCError::kRPCUnrecoverable.0,
                        }]),
                    },
                }),
            }
        }
    }

    async fn start_fake_db_gateway(
        mode: FakeDbGatewayMode,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test DbGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test DbGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(DbGateway_DbGateway1_Dispatcher::new(Box::new(
            FakeDbGateway { mode },
        ))));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle)
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    async fn make_scheduler_client(addr: SocketAddr) -> SchedulerCoord_SchedulerCoord1_Client {
        use std::sync::Arc;
        let pool = Arc::new(
            FixedSerialPool::connect(&addr, 1)
                .await
                .expect("bug: test scheduler client should connect"),
        );
        SchedulerCoord_SchedulerCoord1_Client::new(pool)
    }

    async fn make_indexer_client(
        addr: SocketAddr,
    ) -> IndexerBlockExplorer_IndexerBlockExplorer1_Client {
        use std::sync::Arc;
        let pool = Arc::new(
            FixedSerialPool::connect(&addr, 1)
                .await
                .expect("bug: test indexer client should connect"),
        );
        IndexerBlockExplorer_IndexerBlockExplorer1_Client::new(pool)
    }

    async fn make_db_gateway_client(addr: SocketAddr) -> DbGateway_DbGateway1_Client {
        use std::sync::Arc;
        let pool = Arc::new(
            FixedSerialPool::connect(&addr, 1)
                .await
                .expect("bug: test DbGateway client should connect"),
        );
        DbGateway_DbGateway1_Client::new(pool)
    }

    async fn make_read_backend(
        indexer_addr: SocketAddr,
        scheduler_addr: SocketAddr,
    ) -> HogdDevReadBackend {
        let indexer = make_indexer_client(indexer_addr).await;
        let scheduler = make_scheduler_client(scheduler_addr).await;
        HogdDevReadBackend {
            indexer,
            db_gateway: None,
            scheduler,
        }
    }

    async fn make_read_backend_with_gateway(
        indexer_addr: SocketAddr,
        scheduler_addr: SocketAddr,
        db_gateway_addr: SocketAddr,
    ) -> HogdDevReadBackend {
        let indexer = make_indexer_client(indexer_addr).await;
        let scheduler = make_scheduler_client(scheduler_addr).await;
        let db_gateway = make_db_gateway_client(db_gateway_addr).await;
        HogdDevReadBackend {
            indexer,
            db_gateway: Some(db_gateway),
            scheduler,
        }
    }

    fn canned_tx_hash(bytes: [u8; 32]) -> TxHash {
        TxHash(groundhog_rpc_api_types::hex::Hex0x(SHA256Hash::from(bytes)))
    }

    fn canned_storage_address() -> groundhog_rpc_api_types::ContractAddress {
        Hex0x(ContractAddress::from([0u8; 32]))
    }

    fn empty_event_query() -> EventQuery {
        EventQuery {
            filters: EventFilterList(VecM(vec![])),
            order: groundhog_rpc_api_types::events::IterationOrder::Ascending,
            anchor: groundhog_rpc_api_types::events::BlockAnchor::AtBeginning,
            cursor: groundhog_rpc_api_types::events::PaginationCursor(vec![]),
            limit: 10,
        }
    }

    fn event_query_with_cursor(cursor: &[u8]) -> EventQuery {
        EventQuery {
            filters: EventFilterList(VecM(vec![])),
            order: groundhog_rpc_api_types::events::IterationOrder::Ascending,
            anchor: groundhog_rpc_api_types::events::BlockAnchor::AtBeginning,
            cursor: groundhog_rpc_api_types::events::PaginationCursor(cursor.to_vec()),
            limit: 10,
        }
    }

    // -----------------------------------------------------------------------
    // transaction_receipt tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn transaction_receipt_known_hash_returns_some_with_correct_height() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        // Act
        let result = backend
            .transaction_receipt(tx_hash)
            .await
            .expect("bug: receipt lookup should succeed");

        // Assert
        let receipt = result.expect("bug: receipt should be Some for known tx hash");
        assert_eq!(
            *receipt.height, CANNED_HEIGHT_A,
            "receipt height should match CANNED_HEIGHT_A"
        );
    }

    #[tokio::test]
    async fn transaction_receipt_unknown_hash_returns_none() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let result = backend
            .transaction_receipt(tx_hash)
            .await
            .expect("bug: receipt lookup should succeed");

        // Assert
        assert!(
            result.is_none(),
            "receipt should be None for unknown tx hash"
        );
    }

    #[tokio::test]
    async fn transaction_receipt_indexer_rpc_fail_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::RpcFail).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        // Act
        let result = backend.transaction_receipt(tx_hash).await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "indexer rpc-fail should surface BackendUnavailable, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // get_storage_at tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_storage_at_without_gateway_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;

        // Act
        let result = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await;

        // Assert
        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "missing DbGateway should be BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_storage_at_found_returns_item_with_snapshot_height() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::Found).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let read = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await
            .expect("bug: storage read should succeed");

        // Assert
        assert_eq!(*read.block_height, CANNED_STORAGE_HEIGHT);
        let item = read.item.expect("bug: canned key should return an item");
        let item_json = serde_json::to_value(item).expect("bug: storage item should serialize");
        assert_eq!(item_json["kind"], "bytestring");
        assert_eq!(
            item_json["bytes"],
            format!("0x{}", hex::encode(CANNED_STORAGE_VALUE))
        );
    }

    #[tokio::test]
    async fn get_storage_at_not_found_returns_none_with_snapshot_height() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::NotFound).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let read = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await
            .expect("bug: storage miss should still succeed");

        // Assert
        assert_eq!(*read.block_height, CANNED_STORAGE_HEIGHT);
        assert!(
            read.item.is_none(),
            "kRPCNotFound should map to an absent storage item"
        );
    }

    #[tokio::test]
    async fn get_storage_at_gateway_rpc_fail_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::RpcFail).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let result = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await;

        // Assert
        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "DbGateway RPC failure should be BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_storage_at_outer_default_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::OuterDefault).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let result = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await;

        // Assert
        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "outer Default should be BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_storage_at_empty_payload_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::EmptyPayload).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let result = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await;

        // Assert
        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "empty payload should be BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_storage_at_item_default_error_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let (gateway_addr, _gateway) =
            start_fake_db_gateway(FakeDbGatewayMode::ItemDefaultError).await;
        let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

        // Act
        let result = backend
            .get_storage_at(canned_storage_address(), CANNED_STORAGE_KEY)
            .await;

        // Assert
        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "per-item non-not-found Default should be BackendUnavailable, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // transaction_status tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn transaction_status_pending_in_scheduler_returns_pending() {
        // Arrange: scheduler says all queried hashes are pending.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::AllPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        // Use an unknown hash — scheduler says it's pending regardless.
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let status = backend
            .transaction_status(tx_hash)
            .await
            .expect("bug: status check should succeed");

        // Assert
        assert!(
            matches!(status, TransactionStatus::Pending),
            "expected Pending, got {status:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_committed_not_pending_returns_committed() {
        // Arrange: scheduler says not pending; indexer has the tx committed.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        // Act
        let status = backend
            .transaction_status(tx_hash)
            .await
            .expect("bug: status check should succeed");

        // Assert
        assert!(
            matches!(status, TransactionStatus::Committed { ref height } if **height == CANNED_HEIGHT_A),
            "expected Committed({CANNED_HEIGHT_A}), got {status:?}",
        );
    }

    #[tokio::test]
    async fn transaction_status_in_neither_returns_unknown() {
        // Arrange: scheduler says not pending; indexer doesn't have it.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let status = backend
            .transaction_status(tx_hash)
            .await
            .expect("bug: status check should succeed");

        // Assert
        assert!(
            matches!(status, TransactionStatus::Unknown),
            "expected Unknown, got {status:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_scheduler_rpc_fail_still_returns_committed_from_indexer() {
        // Arrange: scheduler RPC fails (degraded mode); indexer has the tx.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::RpcFail).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_B);

        // Act
        let status = backend
            .transaction_status(tx_hash)
            .await
            .expect("bug: status check should succeed even with scheduler failure");

        // Assert: scheduler failure is non-fatal; we still get the indexer answer.
        assert!(
            matches!(status, TransactionStatus::Committed { ref height } if **height == CANNED_HEIGHT_B),
            "expected Committed({CANNED_HEIGHT_B}) despite scheduler failure, got {status:?}",
        );
    }

    #[tokio::test]
    async fn transaction_status_indexer_rpc_fail_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::RpcFail).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        // Act
        let result = backend.transaction_status(tx_hash).await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "indexer rpc-fail should surface BackendUnavailable, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // contract_events tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn contract_events_first_page_returns_two_events_and_cursor() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;

        // Act
        let page = backend
            .contract_events(empty_event_query())
            .await
            .expect("bug: first page should succeed");

        // Assert
        assert_eq!(page.events.len(), 2, "first page should have 2 events");
        assert_eq!(
            page.cursor.0.as_slice(),
            CANNED_EVENT_CURSOR,
            "first page cursor should match CANNED_EVENT_CURSOR"
        );
        assert_eq!(
            *page.latest_block, CANNED_HEIGHT_B,
            "latest_block should be CANNED_HEIGHT_B"
        );
    }

    #[tokio::test]
    async fn contract_events_second_page_returns_one_event_and_empty_cursor() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;

        // Act
        let page = backend
            .contract_events(event_query_with_cursor(CANNED_EVENT_CURSOR))
            .await
            .expect("bug: second page should succeed");

        // Assert
        assert_eq!(page.events.len(), 1, "second page should have 1 event");
        assert!(
            page.cursor.0.is_empty(),
            "second page cursor should be empty (stream exhausted)"
        );
    }

    #[tokio::test]
    async fn contract_events_multi_page_walk_collects_all_three_events() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;

        // Act
        let page1 = backend
            .contract_events(empty_event_query())
            .await
            .expect("bug: page 1 should succeed");
        let page2 = backend
            .contract_events(event_query_with_cursor(page1.cursor.0.as_slice()))
            .await
            .expect("bug: page 2 should succeed");

        // Assert
        let total = page1.events.len() + page2.events.len();
        assert_eq!(total, 3, "multi-page walk should yield 3 events total");
    }

    #[tokio::test]
    async fn contract_events_unknown_cursor_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let unknown_cursor = b"unknown-cursor";

        // Act: the fake indexer returns a ProgramError for unknown cursors,
        // which the client propagates as an RpcError → BackendUnavailable.
        let result = backend
            .contract_events(event_query_with_cursor(unknown_cursor))
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "unknown cursor should surface BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn contract_events_indexer_rpc_fail_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::RpcFail).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;

        // Act
        let result = backend.contract_events(empty_event_query()).await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "indexer rpc-fail should surface BackendUnavailable, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // IndexerConnect failure test
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn connect_to_dead_indexer_port_fails_startup_probe() {
        // Arrange: grab a port and immediately drop the listener so it's closed.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: ephemeral bind should succeed");
        let dead_addr = listener
            .local_addr()
            .expect("bug: bound listener should have local addr");
        drop(listener);

        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let scheduler_client = make_scheduler_client(sched_addr).await;
        let sched_backend = HogdDevSchedulerBackend::_new_for_test(scheduler_client);

        // Act
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            HogdDevReadBackend::connect(dead_addr, None, &sched_backend, PROTOCOL_VERSION),
        )
        .await
        .expect("bug: connect to dead port should return promptly");

        // Assert
        assert!(
            matches!(
                result,
                Err(crate::NodeSupervisorError::IndexerConnect { .. }
                    | crate::NodeSupervisorError::IndexerRpc {
                        op: "indexer_block_explorer_intro",
                        ..
                    })
            ),
            "expected indexer startup failure, got {result:?}"
        );
    }
}
