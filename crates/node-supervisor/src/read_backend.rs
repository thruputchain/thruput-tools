//! hogd-dev indexer-backed read plane adapter.
//!
//! [`HogdDevReadBackend`] implements [`ReadPlane`] against hogd-dev's indexer
//! x23 endpoint. `transaction_receipt` and `contract_events` are pure indexer
//! calls; `transaction_status` runs the indexer-and-scheduler probe the gateway
//! runs, from `groundhog_hogd_backend::status`, so both answer alike.
//! Latest-state storage reads use hogd-dev's `DbGateway` endpoint and can also be
//! served by [`HogdDevStorageReadBackend`] when the indexer is absent.
//!
//! The scheduler gateway client is shared from the chain/submit backend — no
//! second pool is created.

use std::{fmt, net::SocketAddr};

use async_trait::async_trait;
use groundhog_db_gateway::DbGateway_DbGateway1_Client;
use groundhog_hogd_backend::{
    db_gateway as shared_db_gateway, indexer as shared_indexer, intro, receipt as shared_receipt,
    scheduler as shared_scheduler, status as shared_status,
};
use groundhog_indexer::{IndexerBlockExplorer_IndexerBlockExplorer1_Client, indexer::TxInfo};
use groundhog_json_rpc_server::{ReadPlane, RpcRequestContext, RpcServerError};
use groundhog_json_rpc_types::protocol_token::protocol_from_text;
use groundhog_json_rpc_types::{
    ContractAddress,
    events::{EventPage, EventQuery},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{
        AddressTransactionQuery as RpcAddressTransactionQuery, TransactionRefPage,
        TransactionStatus, TxHash,
    },
};
use groundhog_scheduler::sched_gateway::SchedGateway_SchedGateway1_Client;
use groundhog_x23_transport::{connect, connect_with_config, hogd_large_message_config};
use groundhog_xdr::{transaction::TransactionHash, types::StorageKey};
use xdr_common::VecM;

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
        let batch: VecM<TransactionHash> = VecM(vec![shared_receipt::tx_hash_to_wire(tx_hash)]);
        let results = self
            .scheduler_gateway
            .schedgateway_lookup_transaction_outcomes(&batch)
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
    /// `groundhog_hogd_backend::status::transaction_status`. The whole
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
            let wire_query: groundhog_indexer::indexer::EventQuery = query.into();
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
            let wire_query: groundhog_indexer::indexer::AddressTransactionQuery = query.into();
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

async fn get_item_from_db_gateway(
    db_gateway: &DbGateway_DbGateway1_Client,
    address: ContractAddress,
    key: StorageKey,
) -> Result<StorageItemRead, RpcServerError> {
    let batch = shared_db_gateway::single_key_batch(address, key);
    let outer = db_gateway
        .dbgateway_get_items(&batch)
        .await
        .map_err(|source| RpcServerError::BackendUnavailable(format!("{DB_GATEWAY}: {source}")))?;
    let payload = shared_db_gateway::payload(DB_GATEWAY, outer)?;
    shared_db_gateway::single_item_read(DB_GATEWAY, payload)
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
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::{collections::HashMap, net::SocketAddr, time::Duration};

    use async_trait::async_trait;
    use groundhog_db_gateway::AddressAndKeyBatch;
    use groundhog_db_gateway::{
        DbGateway_DbGateway1, DbGateway_DbGateway1_Client, DbGateway_DbGateway1_Dispatcher,
        DbGatewayIntroOutcome, DbGatewayItemResult, DbGatewayPayload, DbGatewayResult,
    };
    use groundhog_indexer::{
        AddressTransactionQuery, EventPage as IdxEventPage, EventPageResult,
        IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
        TransactionPage as IdxTransactionPage, TransactionPageResult, TransactionQuery,
        TransactionRefPage, TransactionRefPageResult, TxInfo, TxInfoResult,
    };
    use groundhog_json_rpc_server::{ReadPlane, RpcRequestContext, RpcServerError};
    use groundhog_json_rpc_types::protocol_token::protocol_text;
    use groundhog_json_rpc_types::{
        events::{EventFilterList, EventQuery},
        hex::Hex0x,
        transaction::{TransactionStatus, TxHash},
    };
    use groundhog_scheduler::{
        ExpectedDBTip, ParallelSignedTransactionBatch, PendingTxCount, RPCError,
        SchedGatewayIntroOutcome, SchedGatewaySubmitOutcome, SchedGatewaySubmitOutcomeBatch,
        SchedulerTransactionOutcomeBatch,
        sched_gateway::{
            SchedGateway_SchedGateway1, SchedGateway_SchedGateway1_Client,
            SchedGateway_SchedGateway1_Dispatcher,
        },
    };
    use groundhog_scp::network::ProtocolCommit as Protocol;
    use groundhog_xdr::event::IdentifiedEvent as IdxIdentifiedEvent;
    use groundhog_xdr::transaction::{
        Invocation, OptSignedTransactionBatch, SignedTransaction, Transaction,
        TransactionHashBatch, Uniquifier,
    };
    use groundhog_xdr::types::BlockHeight as IdxBlockHeight;
    use groundhog_xdr::{
        PROTOCOL_VERSION,
        event::TxEmittedEvent,
        exec_result::ExecResult,
        storage_item::WireStorageItem,
        transaction::TransactionHash,
        types::{ContractAddress, SHA256Hash},
    };
    use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
    use tokio::net::TcpListener;
    use xdr_common::{BytesM, VecM};

    use super::HogdDevReadBackend;

    // These constants mirror the fake_hogd_dev fixture. Defined here so the
    // unit tests in this file have no dependency on the example binary's types.
    const CANNED_TX_HASH_A: [u8; 32] = [0xAAu8; 32];
    const CANNED_TX_HASH_B: [u8; 32] = [0xBBu8; 32];
    const CANNED_TX_HASH_FAILED: [u8; 32] = [0xCCu8; 32];
    const CANNED_TX_HASH_UNKNOWN: [u8; 32] = [0xFFu8; 32];
    const CANNED_HEIGHT_A: i64 = 5;
    const CANNED_HEIGHT_B: i64 = 10;
    const CANNED_GAS_USED_A: u64 = 1_500;
    const CANNED_GAS_USED_B: u64 = 2_500;
    const CANNED_FAILURE_HEIGHT: i64 = 11;
    const CANNED_FAILURE_ADDRESS: [u8; 32] = [0x0Du8; 32];
    const CANNED_EVENT_CURSOR: &[u8] = b"cursor-page-2";
    const CANNED_STORAGE_KEY: [u8; 32] = [0x42u8; 32];
    const CANNED_STORAGE_VALUE: &[u8] = b"local-node-storage";
    const CANNED_STORAGE_HEIGHT: i64 = 12;

    // -----------------------------------------------------------------------
    // Fake scheduler
    // -----------------------------------------------------------------------

    /// Controls what `schedgateway_lookup_tx` returns.
    #[derive(Debug, Clone)]
    enum FakeSchedulerMode {
        /// Returns one `None` per queried hash — nothing pending.
        NoPending,
        Outcome(groundhog_scheduler::SchedulerTransactionOutcome),
        /// Returns one `Some(dummy_tx)` per queried hash — all pending.
        AllPending,
        /// Returns an empty batch — no per-hash answer at all.
        EmptyBatch,
        /// Returns more slots than hashes — an invalid cardinality response.
        ExtraSlot,
        /// RPC call itself fails.
        RpcFail,
    }

    struct FakeScheduler {
        mode: FakeSchedulerMode,
        /// How many `schedgateway_lookup_tx` calls the fake has served.
        lookups: Arc<AtomicUsize>,
    }

    /// Build a minimal dummy `SignedTransaction` suitable for testing the
    /// pending-detection path. The transaction is not valid on-chain; it only
    /// needs to be `Some(...)` so `Option::is_some()` returns true.
    fn dummy_signed_transaction() -> SignedTransaction {
        use groundhog_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
        use groundhog_xdr::contract::ContractEntrypointID;
        use groundhog_xdr::types::GasAmount;
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

    /// Wrap [`dummy_signed_transaction`] with empty nondeterministic
    /// proposal data so it can stand in for a `TxInfo.tx` body in tests.
    fn dummy_proposed_transaction() -> groundhog_xdr::transaction::ProposedTransaction {
        use groundhog_xdr::transaction::{NondeterministicProposalData, ProposedTransaction};
        ProposedTransaction {
            signed_tx: dummy_signed_transaction(),
            nondeterministic_proposal_data: NondeterministicProposalData {
                nondeterministic_bits: BytesM(vec![]),
                nondeterministic_bit_count: 0,
                observed_queue_entry_sequence: VecM(vec![]),
            },
        }
    }

    #[async_trait]
    impl SchedGateway_SchedGateway1 for FakeScheduler {
        async fn schedgateway_intro(
            &self,
            _protocol: Protocol,
        ) -> Result<SchedGatewayIntroOutcome, ProgramError> {
            Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess)
        }

        async fn schedgateway_submit_tx_batch(
            &self,
            batch: ParallelSignedTransactionBatch,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, ProgramError> {
            Ok(VecM(vec![
                SchedGatewaySubmitOutcome::SchedGatewaySubmitAdmitted;
                batch.segment_tokens.0.len()
            ]))
        }

        async fn schedgateway_get_tip(&self) -> Result<ExpectedDBTip, ProgramError> {
            Ok(ExpectedDBTip::Default {
                error: RPCError::kRPCNotFound.0,
            })
        }

        async fn schedgateway_lookup_tx(
            &self,
            hashes: TransactionHashBatch,
        ) -> Result<OptSignedTransactionBatch, ProgramError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            match &self.mode {
                FakeSchedulerMode::Outcome(_) => {
                    panic!("old lookup must not be used for retained outcomes")
                }
                FakeSchedulerMode::NoPending => {
                    Ok(VecM(hashes.0.into_iter().map(|_| None).collect()))
                }
                FakeSchedulerMode::AllPending => {
                    // Return one Some(dummy_tx) per queried hash to signal pending.
                    let results = hashes
                        .0
                        .into_iter()
                        .map(|_| Some(dummy_signed_transaction()))
                        .collect();
                    Ok(VecM(results))
                }
                FakeSchedulerMode::EmptyBatch => Ok(VecM(vec![])),
                FakeSchedulerMode::ExtraSlot => Ok(VecM(vec![None, None])),
                FakeSchedulerMode::RpcFail => {
                    Err(ProgramError::msg("fake scheduler lookup rpc-fail"))
                }
            }
        }

        async fn schedgateway_lookup_transaction_outcomes(
            &self,
            hashes: TransactionHashBatch,
        ) -> Result<SchedulerTransactionOutcomeBatch, ProgramError> {
            use groundhog_scheduler::{
                SchedulerTransactionErrorSource, SchedulerTransactionOutcome,
                SchedulerTransactionStatus,
            };
            if let FakeSchedulerMode::Outcome(outcome) = &self.mode {
                self.lookups.fetch_add(1, Ordering::SeqCst);
                return Ok(VecM(vec![outcome.clone(); hashes.0.len()]));
            }
            let results = self.schedgateway_lookup_tx(hashes).await?;
            Ok(VecM(
                results
                    .0
                    .into_iter()
                    .map(|tx| SchedulerTransactionOutcome {
                        status: if tx.is_some() {
                            SchedulerTransactionStatus::SchedulerTransactionPending
                        } else {
                            SchedulerTransactionStatus::SchedulerTransactionNotFound
                        },
                        error_source:
                            SchedulerTransactionErrorSource::SchedulerTransactionErrorNone,
                        error_code: 0,
                    })
                    .collect(),
            ))
        }

        async fn schedgateway_pending_tx_count(&self) -> Result<PendingTxCount, ProgramError> {
            Ok(0)
        }
    }

    async fn start_fake_scheduler(
        mode: FakeSchedulerMode,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let (addr, handle, _) = start_counting_fake_scheduler(mode).await;
        (addr, handle)
    }

    /// Like [`start_fake_scheduler`], also returning the lookup call counter.
    async fn start_counting_fake_scheduler(
        mode: FakeSchedulerMode,
    ) -> (
        SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
        Arc<AtomicUsize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: test scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("bug: test scheduler should have local addr");
        let lookups = Arc::new(AtomicUsize::new(0));
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedGateway_SchedGateway1_Dispatcher::new(
            Box::new(FakeScheduler {
                mode,
                lookups: lookups.clone(),
            }),
        )));
        let server = RpcServer::new(listener, services);
        let handle = server.start().await;
        (addr, handle, lookups)
    }

    // -----------------------------------------------------------------------
    // Fake indexer
    // -----------------------------------------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeIndexerMode {
        Normal,
        RpcFail,
        Slow,
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
                tx: dummy_proposed_transaction(),
                events: TxEventLog {
                    tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                    exec_result: ExecResult::ExecResultOK,
                    root_output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![]),
                    },
                    gas_consumed: CANNED_GAS_USED_A,
                    events: VecM(vec![]),
                },
            };
            map.insert(CANNED_TX_HASH_A, tx_a);

            let tx_b = TxInfo {
                height: IdxBlockHeight::from(CANNED_HEIGHT_B),
                tx: dummy_proposed_transaction(),
                events: TxEventLog {
                    tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
                    exec_result: ExecResult::ExecResultOK,
                    root_output: InvocationFrameOutput::Return {
                        return_buffer: BytesM(vec![]),
                    },
                    gas_consumed: CANNED_GAS_USED_B,
                    events: VecM(vec![]),
                },
            };
            map.insert(CANNED_TX_HASH_B, tx_b);

            map
        }

        fn build_event_pages() -> EventPages {
            use groundhog_xdr::types::ContractAddress;

            let event1 = IdxIdentifiedEvent {
                tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                block_height: IdxBlockHeight::from(CANNED_HEIGHT_A),
                sequence_number: 0,
                emitted_event: TxEmittedEvent {
                    contract_address: ContractAddress::from([0x01u8; 32]),
                    num_topics: 0,
                    serialized_event: BytesM(vec![]),
                },
            };
            let event2 = IdxIdentifiedEvent {
                tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                block_height: IdxBlockHeight::from(CANNED_HEIGHT_A),
                sequence_number: 1,
                emitted_event: TxEmittedEvent {
                    contract_address: ContractAddress::from([0x01u8; 32]),
                    num_topics: 0,
                    serialized_event: BytesM(vec![]),
                },
            };
            let event3 = IdxIdentifiedEvent {
                tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
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
            arg0: TransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            if self.mode == FakeIndexerMode::Slow {
                tokio::time::sleep(Duration::from_mins(1)).await;
            }
            let key: [u8; 32] = arg0.into_inner();
            if key == CANNED_TX_HASH_FAILED {
                return Ok(TxInfoResult::TxInfoRecentFailure {
                    recent_failure: canned_failure_diagnostic(),
                });
            }
            match self.committed.get(&key) {
                Some(info) => Ok(TxInfoResult::TxInfoFound { info: info.clone() }),
                None => Ok(TxInfoResult::Default {
                    outcome: groundhog_indexer::TxInfoOutcome::TxInfoNotFound.0,
                }),
            }
        }

        async fn get_block_by_height(
            &self,
            _height: groundhog_xdr::types::BlockHeight,
        ) -> Result<groundhog_indexer::BlockLookupResult, ProgramError> {
            Err(ProgramError::msg("not implemented in fake indexer"))
        }

        async fn get_events(
            &self,
            arg0: groundhog_indexer::EventQuery,
        ) -> Result<EventPageResult, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            let (events, next_cursor) = self.resolve_event_page(&arg0.cursor.0)?;
            Ok(EventPageResult::IndexerQueryOk {
                page: IdxEventPage {
                    events: VecM(events.clone()),
                    cursor: BytesM(next_cursor.clone()),
                    latest_block: IdxBlockHeight::from(CANNED_HEIGHT_B),
                    oldest_block: IdxBlockHeight::from(0_i64),
                },
            })
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<TransactionPageResult, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            // No canned transaction-list dataset; report an exhausted stream.
            Ok(TransactionPageResult::IndexerQueryOk {
                page: IdxTransactionPage {
                    transactions: VecM(vec![]),
                    cursor: BytesM(vec![]),
                    latest_block: IdxBlockHeight::from(CANNED_HEIGHT_B),
                    oldest_block: IdxBlockHeight::from(0_i64),
                },
            })
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: AddressTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            if self.mode == FakeIndexerMode::RpcFail {
                return Err(ProgramError::msg("fake indexer rpc-fail"));
            }
            // No canned per-address dataset; report an exhausted stream.
            Ok(TransactionRefPageResult::IndexerQueryOk {
                page: TransactionRefPage {
                    refs: VecM(vec![]),
                    cursor: BytesM(vec![]),
                    latest_block: IdxBlockHeight::from(CANNED_HEIGHT_B),
                    oldest_block: IdxBlockHeight::from(0_i64),
                },
            })
        }

        async fn get_transactions_by_block(
            &self,
            _query: groundhog_indexer::BlockTransactionQuery,
        ) -> Result<TransactionRefPageResult, ProgramError> {
            Err(ProgramError::msg("not implemented in fake indexer"))
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
            _arg0: Protocol,
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

    async fn make_scheduler_client(addr: SocketAddr) -> SchedGateway_SchedGateway1_Client {
        use std::sync::Arc;
        let pool = Arc::new(
            FixedSerialPool::connect(&addr, 1)
                .await
                .expect("bug: test scheduler client should connect"),
        );
        SchedGateway_SchedGateway1_Client::new(pool)
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
        let scheduler_gateway = make_scheduler_client(scheduler_addr).await;
        HogdDevReadBackend {
            indexer,
            db_gateway: None,
            scheduler_gateway,
        }
    }

    async fn make_read_backend_with_gateway(
        indexer_addr: SocketAddr,
        scheduler_addr: SocketAddr,
        db_gateway_addr: SocketAddr,
    ) -> HogdDevReadBackend {
        let indexer = make_indexer_client(indexer_addr).await;
        let scheduler_gateway = make_scheduler_client(scheduler_addr).await;
        let db_gateway = make_db_gateway_client(db_gateway_addr).await;
        HogdDevReadBackend {
            indexer,
            db_gateway: Some(db_gateway),
            scheduler_gateway,
        }
    }

    fn canned_tx_hash(bytes: [u8; 32]) -> TxHash {
        TxHash(groundhog_json_rpc_types::hex::Hex0x(SHA256Hash::from(
            bytes,
        )))
    }

    fn canned_storage_address() -> groundhog_json_rpc_types::ContractAddress {
        Hex0x(ContractAddress::from([0u8; 32]))
    }

    fn canned_failure_diagnostic() -> groundhog_trace::TxFailureDiagnostic {
        groundhog_trace::TxFailureDiagnostic {
            attempt_height: IdxBlockHeight::from(CANNED_FAILURE_HEIGHT),
            cause: groundhog_trace::TxFailureCause::TxFailureDbConflict {
                conflict: groundhog_trace::DbConflictDiagnostic {
                    addrkey: groundhog_xdr::types::AddressAndKey {
                        address: ContractAddress::from(CANNED_FAILURE_ADDRESS),
                        key: groundhog_xdr::types::StorageKey::FreeNativeTokenBalance,
                    },
                    code: groundhog_trace::DbConflictCode::ExclusiveLockDenied,
                },
            },
        }
    }

    fn empty_event_query() -> EventQuery {
        EventQuery {
            filters: EventFilterList(VecM(vec![])),
            order: groundhog_json_rpc_types::events::IterationOrder::Ascending,
            anchor: groundhog_json_rpc_types::events::BlockAnchor::AtBeginning,
            cursor: groundhog_json_rpc_types::events::PaginationCursor(vec![]),
            limit: 10,
        }
    }

    fn event_query_with_cursor(cursor: &[u8]) -> EventQuery {
        EventQuery {
            filters: EventFilterList(VecM(vec![])),
            order: groundhog_json_rpc_types::events::IterationOrder::Ascending,
            anchor: groundhog_json_rpc_types::events::BlockAnchor::AtBeginning,
            cursor: groundhog_json_rpc_types::events::PaginationCursor(cursor.to_vec()),
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
            .transaction_receipt(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: receipt lookup should succeed");

        // Assert
        let receipt = result.expect("bug: receipt should be Some for known tx hash");
        assert_eq!(
            *receipt.height, CANNED_HEIGHT_A,
            "receipt height should match CANNED_HEIGHT_A"
        );
        assert_eq!(
            receipt.gas_used, CANNED_GAS_USED_A,
            "receipt should carry the gas the event log committed"
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
            .transaction_receipt(RpcRequestContext::unbounded(), tx_hash)
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
        let result = backend
            .transaction_receipt(RpcRequestContext::unbounded(), tx_hash)
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "indexer rpc-fail should surface BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn transaction_receipt_respects_request_deadline() {
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Slow).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        let result = backend
            .transaction_receipt(
                RpcRequestContext::new(Some(Duration::from_millis(10))),
                tx_hash,
            )
            .await;

        assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .get_storage_at(
                RpcRequestContext::unbounded(),
                canned_storage_address(),
                CANNED_STORAGE_KEY,
            )
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
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
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
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: status check should succeed");

        // Assert
        assert!(
            matches!(status, TransactionStatus::Committed { ref height } if **height == CANNED_HEIGHT_A),
            "expected Committed({CANNED_HEIGHT_A}), got {status:?}",
        );
    }

    #[tokio::test]
    async fn transaction_status_scheduler_retained_outcomes() {
        use groundhog_scheduler::{
            SchedulerTransactionErrorSource as Source, SchedulerTransactionOutcome,
            SchedulerTransactionStatus as Status,
        };
        for (status, tag) in [
            (Status::SchedulerTransactionSucceeded, "committed"),
            (Status::SchedulerTransactionRetry, "retry"),
            (Status::SchedulerTransactionDropped, "dropped"),
        ] {
            let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
            let outcome = SchedulerTransactionOutcome {
                status,
                error_source: Source::SchedulerTransactionErrorExecutor,
                error_code: -4,
            };
            let (sched_addr, _sched) =
                start_fake_scheduler(FakeSchedulerMode::Outcome(outcome)).await;
            let backend = make_read_backend(idx_addr, sched_addr).await;
            let status = backend
                .transaction_status(RpcRequestContext::unbounded(), canned_tx_hash([0x99; 32]))
                .await
                .unwrap();
            let value = serde_json::to_value(status).unwrap();
            assert_eq!(value["status"], tag);
            if tag == "committed" {
                assert_eq!(value["height"], "0");
            } else {
                assert_eq!(
                    value["error"],
                    serde_json::json!({"source":"executor", "code":-4})
                );
            }
        }
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
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: status check should succeed");

        // Assert
        assert!(
            matches!(status, TransactionStatus::Unknown),
            "expected Unknown, got {status:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_recent_failure_returns_diagnostic() {
        // Arrange: scheduler says not pending; indexer holds a recent
        // nondeterministic-failure diagnostic for the hash.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched, lookups) =
            start_counting_fake_scheduler(FakeSchedulerMode::AllPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_FAILED);

        // Act
        let status = backend
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: status check should succeed");

        assert_eq!(lookups.load(Ordering::SeqCst), 0);

        // Assert
        match status {
            TransactionStatus::RecentFailure {
                attempt_height,
                cause:
                    groundhog_json_rpc_types::transaction::TransactionFailureCause::DbConflict {
                        conflict,
                    },
            } => {
                assert_eq!(*attempt_height, CANNED_FAILURE_HEIGHT);
                assert_eq!(
                    conflict.address.0,
                    ContractAddress::from(CANNED_FAILURE_ADDRESS)
                );
                assert_eq!(
                    conflict.code,
                    groundhog_json_rpc_types::transaction::ConflictCode::ExclusiveLock
                );
            }
            other => panic!("expected RecentFailure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transaction_receipt_recent_failure_returns_none() {
        // Arrange: the indexer reports a recent nondeterministic failure,
        // which is not a committed record and must not become an error.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_FAILED);

        // Act
        let receipt = backend
            .transaction_receipt(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: receipt lookup should succeed");

        // Assert
        assert!(receipt.is_none(), "expected None, got {receipt:?}");
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
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await
            .expect("bug: status check should succeed even with scheduler failure");

        // Assert: scheduler failure is non-fatal; we still get the indexer answer.
        assert!(
            matches!(status, TransactionStatus::Committed { ref height } if **height == CANNED_HEIGHT_B),
            "expected Committed({CANNED_HEIGHT_B}) despite scheduler failure, got {status:?}",
        );
    }

    #[tokio::test]
    async fn transaction_status_scheduler_rpc_fail_and_indexer_miss_surfaces_backend_unavailable() {
        // Arrange: scheduler RPC fails, and the indexer has no record. The
        // transaction may still be pending in the unreachable mempool, so a
        // definite `Unknown` would be a false "not anywhere" claim.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::RpcFail).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let result = backend
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "unconsulted mempool with indexer miss should surface BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_empty_lookup_batch_and_indexer_miss_surfaces_backend_unavailable() {
        // Arrange: the scheduler lookup succeeds but returns no slot for the
        // queried hash, so there is no per-transaction mempool answer.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::EmptyBatch).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let result = backend
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
            ),
            "lookup with no per-tx answer should surface BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_extra_lookup_slot_and_indexer_miss_surfaces_backend_unavailable() {
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::ExtraSlot).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        let result = backend
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await;

        assert!(
            matches!(result, Err(RpcServerError::BackendUnavailable(_))),
            "lookup with an extra per-tx answer should surface BackendUnavailable, got {result:?}"
        );
    }

    #[tokio::test]
    async fn transaction_status_slow_indexer_exhausts_deadline_before_mempool_is_asked() {
        // Arrange: the indexer is asked first and never answers within the
        // request deadline; the scheduler would report the tx pending.
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Slow).await;
        let (sched_addr, _sched, lookups) =
            start_counting_fake_scheduler(FakeSchedulerMode::AllPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let result = backend
            .transaction_status(
                RpcRequestContext::new(Some(Duration::from_millis(10))),
                tx_hash,
            )
            .await;

        // Assert: the deadline is spent on the indexer, so the pending answer
        // is never reached. All stages share one deadline.
        assert!(
            matches!(result, Err(RpcServerError::BackendTimeout)),
            "expected BackendTimeout, got {result:?}"
        );
        assert_eq!(lookups.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn transaction_status_indexer_rpc_fail_surfaces_backend_unavailable() {
        // Arrange
        let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::RpcFail).await;
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let tx_hash = canned_tx_hash(CANNED_TX_HASH_A);

        // Act
        let result = backend
            .transaction_status(RpcRequestContext::unbounded(), tx_hash)
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
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
            .contract_events(RpcRequestContext::unbounded(), empty_event_query())
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
            .contract_events(
                RpcRequestContext::unbounded(),
                event_query_with_cursor(CANNED_EVENT_CURSOR),
            )
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
            .contract_events(RpcRequestContext::unbounded(), empty_event_query())
            .await
            .expect("bug: page 1 should succeed");
        let page2 = backend
            .contract_events(
                RpcRequestContext::unbounded(),
                event_query_with_cursor(page1.cursor.0.as_slice()),
            )
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
            .contract_events(
                RpcRequestContext::unbounded(),
                event_query_with_cursor(unknown_cursor),
            )
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
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
        let result = backend
            .contract_events(RpcRequestContext::unbounded(), empty_event_query())
            .await;

        // Assert
        assert!(
            matches!(
                result,
                Err(groundhog_json_rpc_server::RpcServerError::BackendUnavailable(_))
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
        let scheduler_gateway = make_scheduler_client(sched_addr).await;

        // Act
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            HogdDevReadBackend::connect(
                dead_addr,
                None,
                scheduler_gateway,
                &protocol_text(&PROTOCOL_VERSION),
                None,
            ),
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
