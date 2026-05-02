use std::{
    collections::HashMap,
    env, fs,
    io::{self, Write},
    net::{SocketAddr, TcpListener as StdTcpListener},
    process::{self, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use async_trait::async_trait;
use groundhog_indexer::{
    BlockHeight, EventPage, EventQuery, IdentifiedEvent,
    IndexerBlockExplorer_IndexerBlockExplorer1,
    IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
    Protocol, SignedTransactionHash, TxInfo, TxInfoResult,
};
use groundhog_scheduler::{
    BlockHash, CoordinatorRegistrationOutcome, DbSchedResult, DbTip, GenesisConfiguration,
    Nomination, OptSignedTransactionBatch, ParallelSignedTransactionBatch, SchedulerCommitOutcome,
    SchedulerGetTipResult, SchedulerNominationResult, SchedulerValidationResult,
    SignedTransactionHashBatch, ValidatorID,
    sched_coord::{SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Dispatcher},
};
use groundhog_xdr::types::SHA256Hash;
use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use tokio::{net::TcpListener, runtime::Runtime};
use xdr_common::{BytesM, VecM};

const MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";
/// Optional path the fake writes its PID to before printing readiness.
/// Tests use this to verify cleanup (poll `kill(pid, 0)` until ESRCH).
const PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";
static TERMINATED: AtomicBool = AtomicBool::new(false);

// --- Well-known canned tx hashes used across fake-indexer tests ---

/// First well-known committed tx hash (all 0xAA bytes).
pub const CANNED_TX_HASH_A: [u8; 32] = [0xAAu8; 32];
/// Second well-known committed tx hash (all 0xBB bytes).
pub const CANNED_TX_HASH_B: [u8; 32] = [0xBBu8; 32];
/// A hash that is *not* in the committed set (all 0xFF bytes).
pub const CANNED_TX_HASH_UNKNOWN: [u8; 32] = [0xFFu8; 32];
/// Block height for `CANNED_TX_HASH_A`.
pub const CANNED_HEIGHT_A: i64 = 5;
/// Block height for `CANNED_TX_HASH_B`.
pub const CANNED_HEIGHT_B: i64 = 10;
/// Cursor value the fake uses between page 1 and page 2 of `get_events`.
pub const CANNED_EVENT_CURSOR: &[u8] = b"cursor-page-2";

extern "C" fn handle_sigterm(_: i32) {
    TERMINATED.store(true, Ordering::SeqCst);
}

fn main() {
    match env::var(MODE_ENV).as_deref() {
        Ok("ready") => run_ready(),
        Ok("dead-scheduler") => run_dead_scheduler(),
        Ok("scheduler-get-tip-error") => run_ready_with_scheduler(FakeSchedulerMode::GetTipError),
        Ok("scheduler-load-rejected") => run_ready_with_scheduler(FakeSchedulerMode::LoadRejected),
        Ok("scheduler-load-no-effect") => {
            run_ready_with_scheduler(FakeSchedulerMode::LoadNoEffect);
        }
        Ok("ready-then-exit") => run_ready_then_exit(),
        Ok("spawn-helper-ignore-term") => run_with_helper(),
        Ok("helper-ignore-term") => run_ignore_term_loop(),
        Ok("ignore-term") => run_ignore_term(),
        Ok("exit-before-ready") => process::exit(9),
        Ok("timeout") => {
            eprintln!(r#"{{"level":"info","message":"waiting"}}"#);
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Ok("oversized-readiness") => {
            io::stdout()
                .write_all(&vec![b'x'; 70_000])
                .expect("bug: fake child stdout should be writable");
            io::stdout()
                .flush()
                .expect("bug: fake child stdout should flush");
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        Ok("exit-7") => process::exit(7),
        Ok("hang") => loop {
            thread::sleep(Duration::from_secs(60));
        },
        Ok("ready-with-indexer") => run_with_indexer(FakeIndexerMode::Normal),
        Ok("indexer-connect-fail") => run_indexer_connect_fail(),
        Ok("indexer-rpc-fail") => run_with_indexer(FakeIndexerMode::RpcFail),
        _ => {}
    }
}

fn run_ready_then_exit() {
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!("{}", readiness_scheduler_only(scheduler.addr));
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready-then-exit"}}"#);
    // Flush stdout/stderr so the supervisor sees readiness before the
    // process disappears.
    let _ = io::stdout().flush();
    let _ = io::stderr().flush();
    thread::sleep(Duration::from_millis(500));
    process::exit(0);
}

fn run_ready() {
    run_ready_with_scheduler(FakeSchedulerMode::Normal);
}

fn run_ready_with_scheduler(mode: FakeSchedulerMode) {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(mode);
    println!("{}", readiness_scheduler_only(scheduler.addr));
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

fn run_dead_scheduler() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    println!("{}", readiness_scheduler_only(dead_loopback_addr()));

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term() {
    ignore_sigterm();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!("{}", readiness_scheduler_only(scheduler.addr));
    run_ignore_term_loop();
}

#[allow(clippy::zombie_processes)]
fn run_with_helper() {
    install_sigterm_handler();
    let current_exe = env::current_exe().expect("bug: fake child binary path should resolve");
    // The parent intentionally exits without waiting so the supervisor test can
    // prove process-group SIGKILL reaches descendants after leader exit.
    let _helper = Command::new(current_exe)
        .env(MODE_ENV, "helper-ignore-term")
        .spawn()
        .expect("bug: helper process should spawn");

    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!("{}", readiness_scheduler_only(scheduler.addr));

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term_loop() {
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

/// Start scheduler + indexer on Normal/RpcFail mode, announce both in readiness.
fn run_with_indexer(indexer_mode: FakeIndexerMode) {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    let indexer = start_fake_indexer(indexer_mode);
    println!("{}", readiness_full(scheduler.addr, indexer.addr));
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

/// Bind the indexer port, advertise it in the readiness line, then drop the
/// listener so incoming connections get ECONNREFUSED. Goal: "advertised but
/// unreachable" scenario.
fn run_indexer_connect_fail() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    // Grab an ephemeral addr and immediately close the listener.
    let dead_indexer_addr = dead_loopback_addr();
    println!("{}", readiness_full(scheduler.addr, dead_indexer_addr));
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn write_pid_file_if_requested() {
    if let Ok(path) = env::var(PID_FILE_ENV) {
        fs::write(&path, process::id().to_string())
            .expect("bug: fake child should be able to write its pid file");
    }
}

fn install_sigterm_handler() {
    // SAFETY: installs a process-local handler that only stores to an atomic.
    #[allow(function_casts_as_integer)]
    let handler = handle_sigterm as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
    }
}

fn ignore_sigterm() {
    // SAFETY: sets SIGTERM disposition to ignore for this fake child process.
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
}

/// Readiness JSON with only the scheduler endpoint (v1 reduced contract).
fn readiness_scheduler_only(scheduler: SocketAddr) -> String {
    format!(
        r#"{{"status":"ready","network_id":"groundhog-local","endpoints":{{"scheduler":"{scheduler}"}}}}"#
    )
}

/// Readiness JSON with both scheduler and read (indexer) endpoints.
fn readiness_full(scheduler: SocketAddr, read: SocketAddr) -> String {
    format!(
        r#"{{"status":"ready","network_id":"groundhog-local","endpoints":{{"scheduler":"{scheduler}","read":"{read}"}}}}"#
    )
}

fn dead_loopback_addr() -> SocketAddr {
    let listener =
        StdTcpListener::bind("127.0.0.1:0").expect("bug: dead scheduler port should bind");
    let addr = listener
        .local_addr()
        .expect("bug: dead scheduler listener should have local addr");
    drop(listener);
    addr
}

// ---------------------------------------------------------------------------
// Fake scheduler
// ---------------------------------------------------------------------------

struct FakeSchedulerRuntime {
    _runtime: Runtime,
    addr: SocketAddr,
}

fn start_fake_scheduler(mode: FakeSchedulerMode) -> FakeSchedulerRuntime {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("bug: fake scheduler runtime should build");
    let addr = runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: fake scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("bug: fake scheduler should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
            Box::new(FakeScheduler::new(mode)),
        )));
        let server = RpcServer::new(listener, services);
        let _server_handle = server.start().await;
        addr
    });
    FakeSchedulerRuntime {
        _runtime: runtime,
        addr,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeSchedulerMode {
    Normal,
    GetTipError,
    LoadRejected,
    LoadNoEffect,
}

#[derive(Debug)]
struct FakeScheduler {
    tip: Arc<Mutex<Option<DbTip>>>,
    mode: FakeSchedulerMode,
}

impl FakeScheduler {
    fn new(mode: FakeSchedulerMode) -> Self {
        Self {
            tip: Arc::new(Mutex::new(None)),
            mode,
        }
    }

    fn genesis_tip() -> DbTip {
        DbTip {
            height: 0.into(),
            block_hash: BlockHash::from(SHA256Hash::from([0x42; 32])),
            events_hash: SHA256Hash::from([0x43; 32]),
            db_hash: [0u8; 2048].into(),
            id: 0,
        }
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
        batch: ParallelSignedTransactionBatch,
    ) -> Result<(), ProgramError> {
        if batch.segment_tokens.0 != vec![1] {
            return Err(ProgramError::msg(
                "fake scheduler expected one token segment for single transaction submit",
            ));
        }
        if batch.segment_bytes.0.len() != 1 {
            return Err(ProgramError::msg(
                "fake scheduler expected one byte segment for single transaction submit",
            ));
        }
        if batch.segment_bytes.0[0] as usize != batch.xdr.0.len() {
            return Err(ProgramError::msg(
                "fake scheduler byte segment length did not match xdr payload",
            ));
        }
        if batch.xdr.0.is_empty() {
            return Err(ProgramError::msg(
                "fake scheduler expected non-empty signed transaction xdr",
            ));
        }
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
        _onto: DbTip,
    ) -> Result<SchedulerNominationResult, ProgramError> {
        Err(ProgramError::msg("not implemented in fake hogd-dev"))
    }

    async fn schedulercoord_validate(
        &self,
        _onto: DbTip,
        _nomination: Nomination,
    ) -> Result<SchedulerValidationResult, ProgramError> {
        Err(ProgramError::msg("not implemented in fake hogd-dev"))
    }

    async fn schedulercoord_commit_block(
        &self,
        _block_id: groundhog_scheduler::BlockID,
    ) -> Result<SchedulerCommitOutcome, ProgramError> {
        Err(ProgramError::msg("not implemented in fake hogd-dev"))
    }

    async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
        if self.mode == FakeSchedulerMode::GetTipError {
            return Err(ProgramError::msg("fake get_tip failure"));
        }

        match self
            .tip
            .lock()
            .expect("bug: fake tip mutex poisoned")
            .clone()
        {
            Some(db_tip) => Ok(SchedulerGetTipResult::SchedulerGetTipOK { db_tip }),
            None => Ok(SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock),
        }
    }

    async fn schedcoord_load_genesis_configuration(
        &self,
        _config: GenesisConfiguration,
    ) -> Result<DbSchedResult, ProgramError> {
        if self.mode == FakeSchedulerMode::LoadRejected {
            return Ok(DbSchedResult::DbSchedResultError);
        }
        if self.mode == FakeSchedulerMode::LoadNoEffect {
            return Ok(DbSchedResult::DbSchedResultOK);
        }

        *self.tip.lock().expect("bug: fake tip mutex poisoned") = Some(Self::genesis_tip());
        Ok(DbSchedResult::DbSchedResultOK)
    }

    async fn schedcoord_lookup_transactions(
        &self,
        _hashes: SignedTransactionHashBatch,
    ) -> Result<OptSignedTransactionBatch, ProgramError> {
        Ok(VecM(vec![]))
    }
}

// ---------------------------------------------------------------------------
// Fake indexer
// ---------------------------------------------------------------------------

struct FakeIndexerRuntime {
    _runtime: Runtime,
    addr: SocketAddr,
}

fn start_fake_indexer(mode: FakeIndexerMode) -> FakeIndexerRuntime {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("bug: fake indexer runtime should build");
    let addr = runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: fake indexer should bind");
        let addr = listener
            .local_addr()
            .expect("bug: fake indexer should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(FakeIndexer::new(
                mode,
            ))),
        ));
        let server = RpcServer::new(listener, services);
        let _server_handle = server.start().await;
        addr
    });
    FakeIndexerRuntime {
        _runtime: runtime,
        addr,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeIndexerMode {
    /// Normal operation: serve canned committed set and event log.
    Normal,
    /// Every RPC method returns a `ProgramError`.
    RpcFail,
}

/// Committed tx set: maps `SignedTransactionHash` bytes to `TxInfo`.
type CommittedSet = HashMap<[u8; 32], TxInfo>;

/// Canned event pages served by the fake in Normal mode.
type EventPages = Vec<(Vec<IdentifiedEvent>, Vec<u8>)>;

#[derive(Debug)]
struct FakeIndexer {
    mode: FakeIndexerMode,
    committed: CommittedSet,
    /// Sequential event pages. Each entry is (events, `next_cursor`).
    /// An empty `next_cursor` signals the last page.
    event_pages: EventPages,
}

impl FakeIndexer {
    fn new(mode: FakeIndexerMode) -> Self {
        let committed = Self::build_committed_set();
        let event_pages = Self::build_event_pages();
        Self {
            mode,
            committed,
            event_pages,
        }
    }

    fn build_committed_set() -> CommittedSet {
        use groundhog_xdr::{event::TxEventLog, exec_result::ExecResult};

        let mut map = HashMap::new();

        let tx_a = TxInfo {
            height: BlockHeight::from(CANNED_HEIGHT_A),
            events: TxEventLog {
                proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                exec_result: ExecResult::ExecResultOK,
                events: VecM(vec![]),
            },
        };
        map.insert(CANNED_TX_HASH_A, tx_a);

        let tx_b = TxInfo {
            height: BlockHeight::from(CANNED_HEIGHT_B),
            events: TxEventLog {
                proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
                exec_result: ExecResult::ExecResultOK,
                events: VecM(vec![]),
            },
        };
        map.insert(CANNED_TX_HASH_B, tx_b);

        map
    }

    fn build_event_pages() -> EventPages {
        use groundhog_xdr::{event::TxEmittedEvent, types::ContractAddress};

        // Page 1: two events, non-empty cursor pointing to page 2.
        let event1 = IdentifiedEvent {
            proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
            block_height: BlockHeight::from(CANNED_HEIGHT_A),
            sequence_number: 0,
            emitted_event: TxEmittedEvent {
                contract_address: ContractAddress::from([0x01u8; 32]),
                num_topics: 0,
                serialized_event: BytesM(vec![]),
            },
        };
        let event2 = IdentifiedEvent {
            proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
            block_height: BlockHeight::from(CANNED_HEIGHT_A),
            sequence_number: 1,
            emitted_event: TxEmittedEvent {
                contract_address: ContractAddress::from([0x01u8; 32]),
                num_topics: 0,
                serialized_event: BytesM(vec![]),
            },
        };

        // Page 2: one event, empty cursor (stream exhausted).
        let event3 = IdentifiedEvent {
            proposed_tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
            block_height: BlockHeight::from(CANNED_HEIGHT_B),
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

    /// Find the page to return based on the request cursor.
    /// Returns an error if the cursor does not match any known page continuation.
    fn resolve_event_page(
        &self,
        cursor: &[u8],
    ) -> Result<&(Vec<IdentifiedEvent>, Vec<u8>), ProgramError> {
        if cursor.is_empty() {
            // Initial call: return page 0.
            Ok(&self.event_pages[0])
        } else {
            // Continuation: find the page whose cursor matches the incoming cursor.
            self.event_pages
                .iter()
                .enumerate()
                .find(|(_, (_, next))| !next.is_empty() && next.as_slice() == cursor)
                .map(|(i, _)| &self.event_pages[i + 1])
                .ok_or_else(|| {
                    ProgramError::msg(
                        "unknown event cursor; fake indexer only handles canned dataset",
                    )
                })
        }
    }
}

#[async_trait]
impl IndexerBlockExplorer_IndexerBlockExplorer1 for FakeIndexer {
    #[tracing::instrument(skip_all)]
    async fn indexer_block_explorer_intro(
        &self,
        _arg0: Protocol,
    ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
    }

    #[tracing::instrument(skip_all)]
    async fn get_transaction_info(
        &self,
        arg0: SignedTransactionHash,
    ) -> Result<TxInfoResult, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        let key: [u8; 32] = arg0.into_inner();
        match self.committed.get(&key) {
            Some(info) => Ok(TxInfoResult::TxInfoFound { info: info.clone() }),
            None => Ok(TxInfoResult::Default {
                outcome: groundhog_indexer::TxInfoOutcome::TxInfoNotFound.0,
            }),
        }
    }

    #[tracing::instrument(skip_all)]
    async fn get_events(&self, arg0: EventQuery) -> Result<EventPage, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        let (events, next_cursor) = self.resolve_event_page(&arg0.cursor.0)?;
        Ok(EventPage {
            events: VecM(events.clone()),
            cursor: BytesM(next_cursor.clone()),
            latest_block: BlockHeight::from(CANNED_HEIGHT_B),
            oldest_block: BlockHeight::from(0_i64),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests for the fake indexer — run as part of node-supervisor's test suite.
// The tests live here (inside the example file) so they share the type
// definitions above. They are compiled when the file is built as a test
// harness via `cargo test --example fake_hogd_dev` or, more naturally, they
// are placed in a companion integration test that spawns the binary. The
// inline approach below avoids an extra file.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, time::Duration};

    use groundhog_indexer::{
        BlockAnchor, EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1_Client, IterationOrder,
        TxInfoResult,
    };
    use groundhog_xdr::transaction::SignedTransactionHash;
    use groundhog_xdr::types::SHA256Hash;
    use rpc_driver::{FixedSerialPool, RpcError};
    use tokio::net::TcpListener;
    use xdr_common::{BytesM, VecM};

    use super::{
        CANNED_EVENT_CURSOR, CANNED_HEIGHT_A, CANNED_HEIGHT_B, CANNED_TX_HASH_A, CANNED_TX_HASH_B,
        CANNED_TX_HASH_UNKNOWN, FakeIndexer, FakeIndexerMode,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, RpcServer, ServiceMap,
    };

    async fn start_test_indexer(
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

    async fn make_client(addr: SocketAddr) -> IndexerBlockExplorer_IndexerBlockExplorer1_Client {
        use std::sync::Arc;
        let pool = Arc::new(
            FixedSerialPool::connect(&addr, 1)
                .await
                .expect("bug: test client should connect"),
        );
        IndexerBlockExplorer_IndexerBlockExplorer1_Client::new(pool)
    }

    fn known_hash(bytes: [u8; 32]) -> SignedTransactionHash {
        SHA256Hash::from(bytes)
    }

    fn empty_event_query() -> EventQuery {
        EventQuery {
            filters: VecM(vec![]),
            order: IterationOrder::IterationOrderAscending,
            anchor: BlockAnchor::AnchorAtBeginning,
            cursor: BytesM(vec![]),
            limit: 10,
        }
    }

    fn event_query_with_cursor(cursor: &[u8]) -> EventQuery {
        EventQuery {
            filters: VecM(vec![]),
            order: IterationOrder::IterationOrderAscending,
            anchor: BlockAnchor::AnchorAtBeginning,
            cursor: BytesM(cursor.to_vec()),
            limit: 10,
        }
    }

    // --- get_transaction_info tests ---

    #[tokio::test]
    async fn get_tx_info_known_hash_a_returns_found_with_correct_height() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;
        let hash = known_hash(CANNED_TX_HASH_A);

        // Act
        let result = client
            .get_transaction_info(&hash)
            .await
            .expect("bug: get_transaction_info should succeed");

        // Assert
        match result {
            TxInfoResult::TxInfoFound { info } => {
                assert_eq!(
                    *info.height, CANNED_HEIGHT_A,
                    "TxInfoFound height should match CANNED_HEIGHT_A"
                );
            }
            other => panic!("expected TxInfoFound for hash A, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_tx_info_known_hash_b_returns_found_with_correct_height() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;
        let hash = known_hash(CANNED_TX_HASH_B);

        // Act
        let result = client
            .get_transaction_info(&hash)
            .await
            .expect("bug: get_transaction_info should succeed");

        // Assert
        match result {
            TxInfoResult::TxInfoFound { info } => {
                assert_eq!(
                    *info.height, CANNED_HEIGHT_B,
                    "TxInfoFound height should match CANNED_HEIGHT_B"
                );
            }
            other => panic!("expected TxInfoFound for hash B, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_tx_info_unknown_hash_returns_not_found() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;
        let hash = known_hash(CANNED_TX_HASH_UNKNOWN);

        // Act
        let result = client
            .get_transaction_info(&hash)
            .await
            .expect("bug: get_transaction_info should succeed");

        // Assert
        assert!(
            matches!(
                result,
                TxInfoResult::Default { outcome }
                    if outcome == groundhog_indexer::TxInfoOutcome::TxInfoNotFound.0
            ),
            "expected TxInfoNotFound for unknown hash, got {result:?}"
        );
    }

    // --- get_events pagination tests ---

    #[tokio::test]
    async fn get_events_first_page_returns_two_events_and_cursor() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;

        // Act
        let page = client
            .get_events(&empty_event_query())
            .await
            .expect("bug: get_events should succeed");

        // Assert
        assert_eq!(page.events.0.len(), 2, "first page should have 2 events");
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
    async fn get_events_second_page_returns_one_event_and_empty_cursor() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;

        // Act
        let page = client
            .get_events(&event_query_with_cursor(CANNED_EVENT_CURSOR))
            .await
            .expect("bug: get_events page 2 should succeed");

        // Assert
        assert_eq!(page.events.0.len(), 1, "second page should have 1 event");
        assert!(
            page.cursor.0.is_empty(),
            "second page cursor should be empty (stream exhausted)"
        );
    }

    #[tokio::test]
    async fn get_events_multi_page_walk_collects_all_three_events() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;

        // Act: walk all pages
        let page1 = client
            .get_events(&empty_event_query())
            .await
            .expect("bug: page 1 should succeed");
        let page2 = client
            .get_events(&event_query_with_cursor(page1.cursor.0.as_slice()))
            .await
            .expect("bug: page 2 should succeed");

        // Assert: all three events collected across two pages
        let total = page1.events.0.len() + page2.events.0.len();
        assert_eq!(total, 3, "multi-page walk should yield 3 events total");

        // Verify first page events reference hash A
        for event in &page1.events.0 {
            assert_eq!(
                *event.proposed_tx_hash, CANNED_TX_HASH_A,
                "page 1 events should reference tx hash A"
            );
        }
        // Verify second page event references hash B
        assert_eq!(
            *page2.events.0[0].proposed_tx_hash, CANNED_TX_HASH_B,
            "page 2 event should reference tx hash B"
        );
    }

    #[tokio::test]
    async fn get_events_unknown_cursor_returns_rpc_error() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::Normal).await;
        let client = make_client(addr).await;
        let unknown_cursor = b"unknown-cursor-value";

        // Act: attempt to fetch with a cursor that doesn't match any known page
        let result = client
            .get_events(&event_query_with_cursor(unknown_cursor))
            .await;

        // Assert: should return an RPC error, not silently return the last page
        assert!(
            matches!(result, Err(RpcError::SystemError)),
            "unknown cursor should return RpcError::SystemError, got {result:?}"
        );
    }

    // --- RpcFail mode tests ---

    #[tokio::test]
    async fn rpc_fail_mode_get_tx_info_returns_rpc_error() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::RpcFail).await;
        let client = make_client(addr).await;
        let hash = known_hash(CANNED_TX_HASH_A);

        // Act
        let result = client.get_transaction_info(&hash).await;

        // Assert
        assert!(
            matches!(result, Err(RpcError::SystemError)),
            "rpc-fail mode should return RpcError::SystemError, got {result:?}"
        );
    }

    #[tokio::test]
    async fn rpc_fail_mode_get_events_returns_rpc_error() {
        // Arrange
        let (addr, _handle) = start_test_indexer(FakeIndexerMode::RpcFail).await;
        let client = make_client(addr).await;

        // Act
        let result = client.get_events(&empty_event_query()).await;

        // Assert
        assert!(
            matches!(result, Err(RpcError::SystemError)),
            "rpc-fail mode should return RpcError::SystemError, got {result:?}"
        );
    }

    // --- Connect-fail mode test (against a dead port) ---

    #[tokio::test]
    async fn connect_to_dead_indexer_port_fails() {
        // Grab an ephemeral port and immediately drop the listener so the port
        // is closed. Connecting to it should fail.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: ephemeral bind should succeed");
        let addr = listener
            .local_addr()
            .expect("bug: bound listener should have local addr");
        drop(listener);

        // Act: try to connect
        let result =
            tokio::time::timeout(Duration::from_secs(5), FixedSerialPool::connect(&addr, 1))
                .await
                .expect("bug: connection attempt should return promptly");

        // Assert
        assert!(
            result.is_err(),
            "connecting to a dead port should fail, got Ok"
        );
    }
}
