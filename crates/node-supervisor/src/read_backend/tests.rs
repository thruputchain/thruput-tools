use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::{collections::HashMap, net::SocketAddr, time::Duration};

use async_trait::async_trait;
use rpc_driver::{FixedSerialPool, ProgramError, RpcServer, ServiceMap};
use thruput_db_gateway::AddressAndKeyBatch;
use thruput_db_gateway::{
    DbGateway_DbGateway1, DbGateway_DbGateway1_Client, DbGateway_DbGateway1_Dispatcher,
    DbGatewayIntroOutcome, DbGatewayItemResult, DbGatewayPayload, DbGatewayResult,
};
use thruput_indexer::{
    AddressTransactionQuery, EventPage as IdxEventPage, EventPageResult,
    IndexerBlockExplorer_IndexerBlockExplorer1, IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
    TransactionPage as IdxTransactionPage, TransactionPageResult, TransactionQuery,
    TransactionRefPage, TransactionRefPageResult, TxInfo, TxInfoResult,
};
use thruput_json_rpc_server::{ReadPlane, RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::protocol_token::protocol_text;
use thruput_json_rpc_types::{
    address_auth::{AuthPublicKey, AuthSigner},
    events::{EventFilterList, EventQuery},
    hex::Hex0x,
    transaction::{TransactionStatus, TxHash},
};
use thruput_scheduler::{
    ExpectedDBTip, ParallelSignedTransactionBatch, PendingTxCount, RPCError,
    SchedGatewayIntroOutcome, SchedGatewaySubmitOutcome, SchedGatewaySubmitOutcomeBatch,
    SchedulerTransactionOutcomeBatch,
    sched_gateway::{
        SchedGateway_SchedGateway1, SchedGateway_SchedGateway1_Client,
        SchedGateway_SchedGateway1_Dispatcher,
    },
};
use thruput_scp::network::ProtocolCommit as Protocol;
use thruput_xdr::address_auth::{AddressAuthData, PublicKeyConfig, PublicKeyConfigOpt};
use thruput_xdr::event::IdentifiedEvent as IdxIdentifiedEvent;
use thruput_xdr::transaction::{
    Invocation, OptSignedTransactionBatch, SignedTransaction, Transaction, TransactionHashBatch,
    Uniquifier,
};
use thruput_xdr::types::BlockHeight as IdxBlockHeight;
use thruput_xdr::types::{Ed25519PublicKey, PublicKey, StorageKey};
use thruput_xdr::{
    PROTOCOL_VERSION,
    event::TxEmittedEvent,
    exec_result::ExecResult,
    storage_item::WireStorageItem,
    transaction::TransactionHash,
    types::{AddressAndKey, ContractAddress, SHA256Hash},
};
use tokio::net::TcpListener;
use xdr_common::{BytesM, VecM};

use super::{HogdDevReadBackend, shared_db_gateway};

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
    Outcome(thruput_scheduler::SchedulerTransactionOutcome),
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
    use thruput_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
    use thruput_xdr::contract::ContractEntrypointID;
    use thruput_xdr::types::GasAmount;
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
fn dummy_proposed_transaction() -> thruput_xdr::transaction::ProposedTransaction {
    use thruput_xdr::transaction::{NondeterministicProposalData, ProposedTransaction};
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
            FakeSchedulerMode::NoPending => Ok(VecM(hashes.0.into_iter().map(|_| None).collect())),
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
            FakeSchedulerMode::RpcFail => Err(ProgramError::msg("fake scheduler lookup rpc-fail")),
        }
    }

    async fn schedgateway_lookup_transaction_outcomes(
        &self,
        hashes: TransactionHashBatch,
    ) -> Result<SchedulerTransactionOutcomeBatch, ProgramError> {
        use thruput_scheduler::{
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
                    error_source: SchedulerTransactionErrorSource::SchedulerTransactionErrorNone,
                    error_code: 0,
                    height: 42.into(),
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
        use thruput_xdr::{event::TxEventLog, transaction::InvocationFrameOutput};
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
        use thruput_xdr::types::ContractAddress;

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
                outcome: thruput_indexer::TxInfoOutcome::TxInfoNotFound.0,
            }),
        }
    }

    async fn get_block_by_height(
        &self,
        _height: thruput_xdr::types::BlockHeight,
    ) -> Result<thruput_indexer::BlockLookupResult, ProgramError> {
        Err(ProgramError::msg("not implemented in fake indexer"))
    }

    async fn get_events(
        &self,
        arg0: thruput_indexer::EventQuery,
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
        _query: thruput_indexer::BlockTransactionQuery,
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
    /// Answers a lone `AuthData` key with registered auth data; any other
    /// key is a per-item failure, so a misrouted read cannot pass.
    AuthData,
}

struct FakeDbGateway {
    mode: FakeDbGatewayMode,
    requests: Arc<Mutex<Vec<AddressAndKeyBatch>>>,
}

#[async_trait]
impl DbGateway_DbGateway1 for FakeDbGateway {
    async fn dbgateway_get_config(
        &self,
    ) -> Result<thruput_db_gateway::DbGatewayConfigResult, rpc_driver::ProgramError> {
        Ok(thruput_db_gateway::DbGatewayConfigResult::Default {
            error: thruput_scheduler::RPCError::kRPCUnimplemented.0,
        })
    }
    async fn dbgateway_intro(
        &self,
        _arg0: Protocol,
    ) -> Result<DbGatewayIntroOutcome, ProgramError> {
        Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
    }

    async fn dbgateway_get_items(
        &self,
        arg0: AddressAndKeyBatch,
    ) -> Result<DbGatewayResult, ProgramError> {
        self.requests
            .lock()
            .expect("bug: request log lock should not be poisoned")
            .push(arg0.clone());
        match self.mode {
            FakeDbGatewayMode::AuthData => {
                let item = match arg0.0.as_slice() {
                    [request] if request.key == StorageKey::AuthData => {
                        DbGatewayItemResult::kRPCNone {
                            item: WireStorageItem::kItemTypeAuthData {
                                auth_data: canned_auth_data(),
                            },
                        }
                    }
                    _ => DbGatewayItemResult::Default {
                        error: RPCError::kRPCUnrecoverable.0,
                    },
                };
                Ok(DbGatewayResult::kRPCNone {
                    payload: DbGatewayPayload {
                        block_height: IdxBlockHeight::from(CANNED_STORAGE_HEIGHT),
                        items: VecM(vec![item]),
                    },
                })
            }
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
    let (addr, handle, _) = start_recording_fake_db_gateway(mode).await;
    (addr, handle)
}

/// Like [`start_fake_db_gateway`], also returning every request batch it decoded.
async fn start_recording_fake_db_gateway(
    mode: FakeDbGatewayMode,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
    Arc<Mutex<Vec<AddressAndKeyBatch>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: test DbGateway should bind");
    let addr = listener
        .local_addr()
        .expect("bug: test DbGateway should have local addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut services = ServiceMap::new();
    services.add_service(Box::new(DbGateway_DbGateway1_Dispatcher::new(Box::new(
        FakeDbGateway {
            mode,
            requests: Arc::clone(&requests),
        },
    ))));
    let server = RpcServer::new(listener, services);
    let handle = server.start().await;
    (addr, handle, requests)
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
    TxHash(thruput_json_rpc_types::hex::Hex0x(SHA256Hash::from(bytes)))
}

fn canned_storage_address() -> thruput_json_rpc_types::ContractAddress {
    Hex0x(ContractAddress::from([0u8; 32]))
}

fn canned_failure_diagnostic() -> thruput_trace::TxFailureDiagnostic {
    thruput_trace::TxFailureDiagnostic {
        attempt_height: IdxBlockHeight::from(CANNED_FAILURE_HEIGHT),
        cause: thruput_trace::TxFailureCause::TxFailureDbConflict {
            conflict: thruput_trace::DbConflictDiagnostic {
                addrkey: thruput_xdr::types::AddressAndKey {
                    address: ContractAddress::from(CANNED_FAILURE_ADDRESS),
                    key: thruput_xdr::types::StorageKey::FreeNativeTokenBalance,
                },
                code: thruput_trace::DbConflictCode::ExclusiveLockDenied,
            },
        },
    }
}

fn empty_event_query() -> EventQuery {
    EventQuery {
        filters: EventFilterList(VecM(vec![])),
        order: thruput_json_rpc_types::events::IterationOrder::Ascending,
        anchor: thruput_json_rpc_types::events::BlockAnchor::AtBeginning,
        cursor: thruput_json_rpc_types::events::PaginationCursor(vec![]),
        limit: 10,
    }
}

fn event_query_with_cursor(cursor: &[u8]) -> EventQuery {
    EventQuery {
        filters: EventFilterList(VecM(vec![])),
        order: thruput_json_rpc_types::events::IterationOrder::Ascending,
        anchor: thruput_json_rpc_types::events::BlockAnchor::AtBeginning,
        cursor: thruput_json_rpc_types::events::PaginationCursor(cursor.to_vec()),
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
    let (gateway_addr, _gateway, requests) =
        start_recording_fake_db_gateway(FakeDbGatewayMode::Found).await;
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
    let expected = AddressAndKey {
        address: canned_storage_address().0,
        key: shared_db_gateway::contract_data_key(CANNED_STORAGE_KEY),
    };
    assert_eq!(
        *requests
            .lock()
            .expect("bug: request log lock should not be poisoned"),
        [VecM(vec![expected])],
        "DbGateway should receive exactly the requested key"
    );
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
    let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::ItemDefaultError).await;
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

fn canned_auth_data() -> AddressAuthData {
    let mut public_keys: [PublicKeyConfigOpt; 21] =
        std::array::from_fn(|_| PublicKeyConfigOpt::new(None));
    public_keys[1] = PublicKeyConfigOpt::new(Some(PublicKeyConfig {
        key: PublicKey::Ed25519 {
            ed25519: Ed25519PublicKey::from([0x5a; 32]),
        },
        max_weight: 2,
    }));
    AddressAuthData {
        public_keys,
        required_weight: 2,
        last_written_height: IdxBlockHeight::from(1),
    }
}

#[tokio::test]
async fn read_address_auth_data_projects_registered_signers_at_snapshot_height() {
    // Arrange
    let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
    let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
    let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::AuthData).await;
    let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

    // Act
    let read = backend
        .read_address_auth_data(RpcRequestContext::unbounded(), canned_storage_address())
        .await
        .expect("bug: auth data read should succeed");

    // Assert
    assert_eq!(read.address, canned_storage_address());
    assert_eq!(*read.height, CANNED_STORAGE_HEIGHT);
    let auth_data = read.auth_data.expect("bug: canned address has auth data");
    assert_eq!(auth_data.required_weight, 2);
    assert_eq!(
        auth_data.signers,
        vec![AuthSigner {
            key_id: 1,
            public_key: AuthPublicKey::Ed25519 {
                key: Hex0x(Ed25519PublicKey::from([0x5a; 32])),
            },
            max_weight: 2,
        }]
    );
}

#[tokio::test]
async fn read_address_auth_data_not_found_is_unconfigured_at_snapshot_height() {
    // Arrange
    let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
    let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
    let (gateway_addr, _gateway) = start_fake_db_gateway(FakeDbGatewayMode::NotFound).await;
    let backend = make_read_backend_with_gateway(idx_addr, sched_addr, gateway_addr).await;

    // Act
    let read = backend
        .read_address_auth_data(RpcRequestContext::unbounded(), canned_storage_address())
        .await
        .expect("bug: an address without auth data is a successful read");

    // Assert
    assert_eq!(*read.height, CANNED_STORAGE_HEIGHT);
    assert!(read.auth_data.is_none());
}

#[tokio::test]
async fn read_address_auth_data_without_gateway_surfaces_backend_unavailable() {
    // Arrange
    let (idx_addr, _idx) = start_fake_indexer(FakeIndexerMode::Normal).await;
    let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::NoPending).await;
    let backend = make_read_backend(idx_addr, sched_addr).await;

    // Act
    let result = backend
        .read_address_auth_data(RpcRequestContext::unbounded(), canned_storage_address())
        .await;

    // Assert
    assert!(
        matches!(result, Err(RpcServerError::BackendUnavailable(_))),
        "missing DbGateway should be BackendUnavailable, got {result:?}"
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
    use thruput_scheduler::{
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
            height: 42.into(),
        };
        let (sched_addr, _sched) = start_fake_scheduler(FakeSchedulerMode::Outcome(outcome)).await;
        let backend = make_read_backend(idx_addr, sched_addr).await;
        let status = backend
            .transaction_status(RpcRequestContext::unbounded(), canned_tx_hash([0x99; 32]))
            .await
            .unwrap();
        let value = serde_json::to_value(status).unwrap();
        assert_eq!(value["status"], tag);
        if tag == "committed" {
            assert_eq!(
                value,
                serde_json::json!({"status": "committed", "height": "42"})
            );
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
        start_counting_fake_scheduler(FakeSchedulerMode::NoPending).await;
    let backend = make_read_backend(idx_addr, sched_addr).await;
    let tx_hash = canned_tx_hash(CANNED_TX_HASH_FAILED);

    // Act
    let status = backend
        .transaction_status(RpcRequestContext::unbounded(), tx_hash)
        .await
        .expect("bug: status check should succeed");

    assert_eq!(lookups.load(Ordering::SeqCst), 1);

    // Assert
    match status {
        TransactionStatus::RecentFailure {
            attempt_height,
            cause:
                thruput_json_rpc_types::transaction::TransactionFailureCause::DbConflict { conflict },
        } => {
            assert_eq!(*attempt_height, CANNED_FAILURE_HEIGHT);
            assert_eq!(
                conflict.address.0,
                ContractAddress::from(CANNED_FAILURE_ADDRESS)
            );
            assert_eq!(
                conflict.code,
                thruput_json_rpc_types::transaction::ConflictCode::ExclusiveLock
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
            Err(thruput_json_rpc_server::RpcServerError::BackendUnavailable(
                _
            ))
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
