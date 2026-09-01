use std::{
    collections::HashMap,
    env, fs,
    io::{self, Write},
    net::{SocketAddr, TcpListener as StdTcpListener},
    path::PathBuf,
    process::{self, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use async_trait::async_trait;
use groundhog_db_gateway::AddressAndKeyBatch;
use groundhog_db_gateway::{
    DbGateway_DbGateway1, DbGateway_DbGateway1_Dispatcher, DbGatewayIntroOutcome,
    DbGatewayItemResult, DbGatewayPayload, DbGatewayResult,
};
use groundhog_indexer::{
    AddressTransactionQuery, EventPage, EventPageResult, EventQuery,
    IndexerBlockExplorer_IndexerBlockExplorer1,
    IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
    TransactionPage, TransactionPageResult, TransactionQuery, TransactionRefPage,
    TransactionRefPageResult, TxInfo, TxInfoResult,
};
use groundhog_json_rpc_types::hex::Hex0x;
use groundhog_json_rpc_types::protocol_token::protocol_text;
use groundhog_scheduler::{
    BlockID, ConsensusValue, CoordinatorRegistrationOutcome, DbSchedResult, DbTip, ExpectedDBTip,
    ParallelBatch, ParallelNondeterministicProposalDataBatch, ParallelSignedTransactionBatch,
    ParallelTransactionHashBatch, PendingTxCount, Proposal, ProposalTxs, ProposalTxsHash, RPCError,
    SchedGatewayIntroOutcome, SchedGatewaySubmitOutcome, SchedGatewaySubmitOutcomeBatch,
    SchedulerCommitOutcome, SchedulerGetTipResult, SchedulerProposalResult, SchedulerProposedBlock,
    SchedulerValidationResult,
    sched_coord::{SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Dispatcher},
    sched_gateway::{SchedGateway_SchedGateway1, SchedGateway_SchedGateway1_Dispatcher},
};
use groundhog_xdr::PROTOCOL_VERSION;
use groundhog_xdr::contract::{ContractType, DeployedContractHash};
use groundhog_xdr::event::IdentifiedEvent;
use groundhog_xdr::genesis::GenesisConfiguration;
use groundhog_xdr::header::{ProposalContext, ProposalHeader};
use groundhog_xdr::network::ProtocolCommit;
use groundhog_xdr::network_config::NetworkConfig;
use groundhog_xdr::storage_item::WireStorageItem;
use groundhog_xdr::transaction::{
    OptSignedTransactionBatch, TransactionHash, TransactionHashBatch,
};
use groundhog_xdr::types::{
    BlockHeight, ConfigHash, ConsensusValueHash, Ed25519PublicKey, EventsHash, ExtensionPoint,
    NetworkHash, Protocol, SHA256Hash, StorageKey, StorageShortHash, TimePoint, TxsHash,
};
use groundhog_xdr::validator::ValidatorID;
use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, runtime::Runtime};
use xdr_common::{BytesM, VecM};

const MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";
/// Optional path the fake writes its PID to before printing readiness.
/// Tests use this to verify cleanup (poll `kill(pid, 0)` until ESRCH).
const PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";
const COMMIT_ENTERED_FILE_ENV: &str = "GROUNDHOG_FAKE_COMMIT_ENTERED_FILE";
const GRACEFUL_SHUTDOWN_FILE_ENV: &str = "GROUNDHOG_FAKE_GRACEFUL_SHUTDOWN_FILE";
const DERIVATION_SCHEME: &str = "groundhog-bootstrap-account-v1";
static TERMINATED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
struct FakeStartupConfig {
    count: usize,
    seed: String,
    tokens: u64,
}

impl Default for FakeStartupConfig {
    fn default() -> Self {
        Self {
            count: groundhog_signing::DEFAULT_DEV_ACCOUNT_COUNT,
            seed: groundhog_signing::DEFAULT_DEV_ACCOUNT_SEED.to_string(),
            tokens: groundhog_node_supervisor::config::DEFAULT_HOGD_DEV_ACCOUNT_TOKENS,
        }
    }
}

impl FakeStartupConfig {
    fn from_args() -> Self {
        let mut config = Self::default();
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            let Some(value) = args.next() else {
                continue;
            };
            match arg.as_str() {
                "--bootstrap-manifest" => config = Self::from_bootstrap_manifest(&value),
                "--dev-accounts" => {
                    config.count = value.parse().expect("bug: fake dev account count parses");
                }
                "--dev-account-seed" => config.seed = value,
                "--dev-account-tokens" => {
                    config.tokens = value.parse().expect("bug: fake dev account tokens parse");
                }
                _ => {}
            }
        }
        config
    }

    fn from_bootstrap_manifest(path: &str) -> Self {
        let manifest: Value = serde_json::from_slice(
            &fs::read(path).expect("bug: fake bootstrap manifest should be readable"),
        )
        .expect("bug: fake bootstrap manifest should parse");
        assert_eq!(manifest["schemaVersion"], 2);
        assert_eq!(manifest["kind"], "groundhog-bootstrap");
        assert_eq!(manifest["bootstrapFaucet"]["index"], 0);
        assert_eq!(
            manifest["bootstrapFaucet"]["initialStorageTokens"],
            groundhog_node_supervisor::config::LOCAL_FAUCET_STORAGE_TOKENS.to_string()
        );
        let accounts = manifest
            .get("bootstrapAccounts")
            .and_then(Value::as_object)
            .expect("bug: fake bootstrap manifest has bootstrapAccounts");
        let count = accounts
            .get("count")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .expect("bug: fake bootstrap manifest count fits usize");
        assert_eq!(accounts["firstIndex"], 1);
        let seed = accounts
            .get("seed")
            .and_then(Value::as_str)
            .expect("bug: fake bootstrap manifest seed is a string")
            .to_string();
        let raw_tokens = accounts
            .get("initialStorageTokens")
            .and_then(Value::as_str)
            .expect("bug: fake bootstrap manifest token amount is a string");
        let tokens: u64 = raw_tokens
            .parse()
            .expect("bug: fake bootstrap manifest tokens parse");
        assert_eq!(
            raw_tokens,
            tokens.to_string(),
            "bug: bootstrap allocation should remain a raw decimal string"
        );
        Self {
            count,
            seed,
            tokens,
        }
    }
}

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
/// Storage key that returns `CANNED_STORAGE_VALUE` from the fake `DbGateway`.
pub const CANNED_STORAGE_KEY: [u8; 32] = [0x42u8; 32];
/// Storage value returned for `CANNED_STORAGE_KEY`.
pub const CANNED_STORAGE_VALUE: &[u8] = b"local-node-storage";
/// Snapshot height returned by the fake `DbGateway` for storage reads.
pub const CANNED_STORAGE_HEIGHT: i64 = 12;

extern "C" fn handle_sigterm(_: i32) {
    TERMINATED.store(true, Ordering::SeqCst);
}

fn main() {
    match env::var(MODE_ENV).as_deref() {
        Ok("ready") => run_ready(),
        Ok("dead-scheduler") => run_dead_scheduler(),
        Ok("scheduler-get-tip-error") => run_ready_with_scheduler(FakeSchedulerMode::GetTipError),
        Ok("scheduler-network-hash-mismatch") => {
            run_ready_with_scheduler(FakeSchedulerMode::NetworkHashMismatch);
        }
        Ok("scheduler-no-genesis") => run_ready_with_scheduler(FakeSchedulerMode::NoGenesis),
        Ok("automine") => run_ready_with_scheduler(FakeSchedulerMode::Automine),
        Ok("automine-slow-commit") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineSlowCommit);
        }
        Ok("automine-serialize-probe") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineSerializeProbe);
        }
        Ok("automine-hanging-commit") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineHangingCommit);
        }
        Ok("automine-commit-response-lost") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineCommitResponseLost);
        }
        Ok("automine-commit-error") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineCommitError);
        }
        Ok("automine-commit-rejected") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineCommitRejected);
        }
        Ok("automine-runtime-get-tip-error") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineRuntimeGetTipError);
        }
        Ok("automine-malformed-proposal") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineMalformedProposal);
        }
        Ok("automine-nominate-error") => {
            run_ready_with_scheduler(FakeSchedulerMode::AutomineNominateError);
        }
        Ok("ready-then-exit") => run_ready_then_exit(),
        Ok("spawn-helper-ignore-term") => run_with_helper(),
        Ok("spawn-helper-then-exit") => run_with_helper_then_exit(),
        Ok("helper-ignore-term") => run_ignore_term_loop(),
        Ok("ignore-term") => run_ignore_term(),
        Ok("exit-before-ready") => process::exit(9),
        Ok("timeout") => {
            eprintln!(r#"{{"level":"info","message":"waiting"}}"#);
            loop {
                thread::sleep(Duration::from_mins(1));
            }
        }
        Ok("timeout-record-term-ignore") => run_record_term_then_ignore(),
        Ok("oversized-readiness") => {
            io::stdout()
                .write_all(&vec![b'x'; 70_000])
                .expect("bug: fake child stdout should be writable");
            io::stdout()
                .flush()
                .expect("bug: fake child stdout should flush");
            loop {
                thread::sleep(Duration::from_mins(1));
            }
        }
        Ok("exit-7") => process::exit(7),
        Ok("hang") => loop {
            thread::sleep(Duration::from_mins(1));
        },
        Ok("ready-with-indexer") => run_with_indexer(FakeIndexerMode::Normal),
        Ok("ready-with-indexer-no-gateway") => run_with_indexer_no_gateway(FakeIndexerMode::Normal),
        Ok("ready-with-gateway") => run_with_db_gateway_no_indexer(FakeDbGatewayMode::Normal),
        Ok("indexer-connect-fail") => run_indexer_connect_fail(),
        Ok("indexer-intro-rejected") => run_with_indexer(FakeIndexerMode::IntroRejected),
        Ok("indexer-rpc-fail") => run_with_indexer(FakeIndexerMode::RpcFail),
        Ok("indexer-runtime-rpc-fail") => run_with_indexer(FakeIndexerMode::RuntimeRpcFail),
        Ok("db-gateway-connect-fail") => run_db_gateway_connect_fail(),
        Ok("db-gateway-rpc-fail") => {
            run_with_indexer_and_db_gateway(FakeIndexerMode::Normal, FakeDbGatewayMode::RpcFail);
        }
        Ok("db-gateway-intro-rejected") => run_with_indexer_and_db_gateway(
            FakeIndexerMode::Normal,
            FakeDbGatewayMode::IntroRejected,
        ),
        _ => {}
    }
}

fn run_ready_then_exit() {
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!(
        "{}",
        readiness_scheduler_only(scheduler.addr, scheduler.gateway_addr)
    );
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
    println!(
        "{}",
        readiness_scheduler_only(scheduler.addr, scheduler.gateway_addr)
    );
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
    write_graceful_shutdown_file_if_requested();
}

fn run_dead_scheduler() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    println!(
        "{}",
        readiness_scheduler_only(dead_loopback_addr(), dead_loopback_addr())
    );

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term() {
    ignore_sigterm();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!(
        "{}",
        readiness_scheduler_only(scheduler.addr, scheduler.gateway_addr)
    );
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
    println!(
        "{}",
        readiness_scheduler_only(scheduler.addr, scheduler.gateway_addr)
    );

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

#[allow(clippy::zombie_processes)]
fn run_with_helper_then_exit() {
    let current_exe = env::current_exe().expect("bug: fake child binary path should resolve");
    let _helper = Command::new(current_exe)
        .env(MODE_ENV, "helper-ignore-term")
        .spawn()
        .expect("bug: helper process should spawn");

    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!(
        "{}",
        readiness_scheduler_only(scheduler.addr, scheduler.gateway_addr)
    );
    let _ = io::stdout().flush();
    thread::sleep(Duration::from_millis(100));
}

fn run_ignore_term_loop() {
    loop {
        thread::sleep(Duration::from_mins(1));
    }
}

fn run_record_term_then_ignore() {
    install_sigterm_handler();
    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
    write_graceful_shutdown_file_if_requested();
    run_ignore_term_loop();
}

/// Start scheduler + indexer on Normal/RpcFail mode, announce both in readiness.
fn run_with_indexer(indexer_mode: FakeIndexerMode) {
    run_with_indexer_and_db_gateway(indexer_mode, FakeDbGatewayMode::Normal);
}

fn run_with_indexer_no_gateway(indexer_mode: FakeIndexerMode) {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    let indexer = start_fake_indexer(indexer_mode);
    println!(
        "{}",
        readiness_with_read(scheduler.addr, scheduler.gateway_addr, indexer.addr)
    );
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

fn run_with_db_gateway_no_indexer(db_gateway_mode: FakeDbGatewayMode) {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    let db_gateway = start_fake_db_gateway(db_gateway_mode);
    println!(
        "{}",
        readiness_with_gateway(scheduler.addr, scheduler.gateway_addr, db_gateway.addr)
    );
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

fn run_with_indexer_and_db_gateway(
    indexer_mode: FakeIndexerMode,
    db_gateway_mode: FakeDbGatewayMode,
) {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    let indexer = start_fake_indexer(indexer_mode);
    let db_gateway = start_fake_db_gateway(db_gateway_mode);
    println!(
        "{}",
        readiness_full(
            scheduler.addr,
            scheduler.gateway_addr,
            indexer.addr,
            db_gateway.addr,
        )
    );
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
    let db_gateway = start_fake_db_gateway(FakeDbGatewayMode::Normal);
    // Grab an ephemeral addr and immediately close the listener.
    let dead_indexer_addr = dead_loopback_addr();
    println!(
        "{}",
        readiness_full(
            scheduler.addr,
            scheduler.gateway_addr,
            dead_indexer_addr,
            db_gateway.addr,
        )
    );
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_db_gateway_connect_fail() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    let indexer = start_fake_indexer(FakeIndexerMode::Normal);
    let dead_db_gateway_addr = dead_loopback_addr();
    println!(
        "{}",
        readiness_full(
            scheduler.addr,
            scheduler.gateway_addr,
            indexer.addr,
            dead_db_gateway_addr,
        )
    );
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

fn write_graceful_shutdown_file_if_requested() {
    if let Ok(path) = env::var(GRACEFUL_SHUTDOWN_FILE_ENV) {
        fs::write(path, "graceful\n")
            .expect("bug: fake child should write its graceful shutdown marker");
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

/// Readiness JSON with only the required scheduler endpoints (reduced contract).
fn readiness_scheduler_only(scheduler: SocketAddr, scheduler_gateway: SocketAddr) -> String {
    readiness_pointer(&endpoints_json(
        scheduler,
        scheduler_gateway,
        None,
        None,
        None,
    ))
}

/// Readiness JSON with `SchedulerCoord` and `IndexerBlockExplorer`
/// endpoints, but no `DbGateway` endpoint.
fn readiness_with_read(
    scheduler: SocketAddr,
    scheduler_gateway: SocketAddr,
    read: SocketAddr,
) -> String {
    readiness_pointer(&endpoints_json(
        scheduler,
        scheduler_gateway,
        Some(read),
        None,
        None,
    ))
}

/// Readiness JSON with `SchedulerCoord` and `DbGateway` endpoints, but no indexer endpoint.
fn readiness_with_gateway(
    scheduler: SocketAddr,
    scheduler_gateway: SocketAddr,
    gateway: SocketAddr,
) -> String {
    readiness_pointer(&endpoints_json(
        scheduler,
        scheduler_gateway,
        None,
        Some(gateway),
        None,
    ))
}

/// Readiness JSON with `SchedulerCoord`, `IndexerBlockExplorer`, and
/// `DbGateway` endpoints.
fn readiness_full(
    scheduler: SocketAddr,
    scheduler_gateway: SocketAddr,
    read: SocketAddr,
    gateway: SocketAddr,
) -> String {
    readiness_pointer(&endpoints_json(
        scheduler,
        scheduler_gateway,
        Some(read),
        Some(gateway),
        None,
    ))
}

fn endpoints_json(
    scheduler: SocketAddr,
    scheduler_gateway: SocketAddr,
    indexer: Option<SocketAddr>,
    db_gateway: Option<SocketAddr>,
    view_gateway: Option<SocketAddr>,
) -> Value {
    let mut endpoints = serde_json::Map::new();
    endpoints.insert("scheduler".to_string(), json!(scheduler));
    endpoints.insert("schedulerGateway".to_string(), json!(scheduler_gateway));
    if let Some(indexer) = indexer {
        endpoints.insert("indexer".to_string(), json!(indexer));
    }
    if let Some(db_gateway) = db_gateway {
        endpoints.insert("dbGateway".to_string(), json!(db_gateway));
    }
    if let Some(view_gateway) = view_gateway {
        endpoints.insert("viewGateway".to_string(), json!(view_gateway));
    }
    Value::Object(endpoints)
}

fn readiness_pointer(endpoints: &Value) -> String {
    let config = FakeStartupConfig::from_args();
    let manifest = runtime_manifest(endpoints, &config);
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).expect("bug: runtime manifest should serialize");
    let manifest_hash = sha256_hex(&manifest_bytes);
    let manifest_path = write_runtime_manifest(&manifest_bytes);
    format!(
        r#"{{"schemaVersion":2,"status":"ready","manifestPath":"{}","manifestHash":"{}"}}"#,
        manifest_path.display(),
        manifest_hash,
    )
}

fn runtime_manifest(endpoints: &Value, config: &FakeStartupConfig) -> Value {
    let mut producer = serde_json::Map::new();
    producer.insert("binary".to_string(), json!("hog-devnode"));
    producer.insert("version".to_string(), json!(env!("CARGO_PKG_VERSION")));
    if let Some(commit_hash) = option_env!("GROUNDHOG_GIT_COMMIT") {
        producer.insert("commitHash".to_string(), json!(commit_hash));
    }

    json!({
        "schemaVersion": 2,
        "kind": "hogd-dev-runtime",
        "chainState": "fresh",
        "networkHash": "00".repeat(32),
        "protocol": protocol_text(&PROTOCOL_VERSION),
        "producer": Value::Object(producer),
        "endpoints": endpoints,
        "bootstrapAccountDerivation": {
            "scheme": DERIVATION_SCHEME,
            "seed": config.seed,
        },
        "bootstrapAccounts": bootstrap_accounts(config),
        "bootstrap": {
            "devWallet": {
                "contentHash": Hex0x(SHA256Hash::from([0x51; 32])),
                "deployedCodeHash": Hex0x(SHA256Hash::from([0x52; 32])),
                "artifactHint": "fake-dev_wallet.rv32a",
                "deployEntrypoint": "0xadac498222148e4f",
                "createWalletEntrypoint": "0x6372656174655761",
                "doActionEntrypoint": "0x646f416374696f6e",
            },
            "storageCostModel": storage_cost_model(),
        },
    })
}

fn bootstrap_accounts(config: &FakeStartupConfig) -> Vec<Value> {
    (0..=config.count)
        .map(|index| {
            let signer = groundhog_signing::derive_dev_account_signer(&config.seed, index);
            let mut address = [0x70_u8; 32];
            address[31] = u8::try_from(index).expect("bug: default account count fits in u8");
            json!({
                "index": u32::try_from(index).expect("bug: fake account index fits in u32"),
                "role": if index == 0 { "faucet" } else { "local_user" },
                "publicKey": Hex0x(Ed25519PublicKey::from(signer.verifying_key_bytes())),
                "walletAddress": Hex0x(groundhog_xdr::types::ContractAddress::from(address)),
                "initialStorageTokens": if index == 0 {
                    groundhog_node_supervisor::config::LOCAL_FAUCET_STORAGE_TOKENS
                } else {
                    config.tokens
                }.to_string(),
            })
        })
        .collect()
}

fn storage_cost_model() -> Value {
    let fields = StorageCostModelFields {
        base_data_item_cost: "12000",
        bytestring_cost_per_byte: "10",
        base_contract_deployment_cost: "12000",
        contract_deployment_cost_per_byte: "10",
        auth_per_key_cost: "1111",
        rcq_cost_per_entry: "8",
        replay_cache_cost_per_entry: "7",
        balance_cost: "1234",
    };
    json!({
        "hash": Hex0x(SHA256Hash::from(storage_cost_model_hash(&fields))),
        "baseDataItemCost": fields.base_data_item_cost,
        "bytestringCostPerByte": fields.bytestring_cost_per_byte,
        "baseContractDeploymentCost": fields.base_contract_deployment_cost,
        "contractDeploymentCostPerByte": fields.contract_deployment_cost_per_byte,
        "authPerKeyCost": fields.auth_per_key_cost,
        "rcqCostPerEntry": fields.rcq_cost_per_entry,
        "replayCacheCostPerEntry": fields.replay_cache_cost_per_entry,
        "balanceCost": fields.balance_cost,
    })
}

struct StorageCostModelFields<'a> {
    base_data_item_cost: &'a str,
    bytestring_cost_per_byte: &'a str,
    base_contract_deployment_cost: &'a str,
    contract_deployment_cost_per_byte: &'a str,
    auth_per_key_cost: &'a str,
    rcq_cost_per_entry: &'a str,
    replay_cache_cost_per_entry: &'a str,
    balance_cost: &'a str,
}

fn storage_cost_model_hash(fields: &StorageCostModelFields<'_>) -> [u8; 32] {
    let mut encoding = Vec::new();
    for item in [
        "groundhog-storage-cost-model-v1",
        fields.base_data_item_cost,
        fields.bytestring_cost_per_byte,
        fields.base_contract_deployment_cost,
        fields.contract_deployment_cost_per_byte,
        fields.auth_per_key_cost,
        fields.rcq_cost_per_entry,
        fields.replay_cache_cost_per_entry,
        fields.balance_cost,
    ] {
        encoding.extend_from_slice(item.as_bytes());
        encoding.push(0);
    }
    Sha256::digest(&encoding).into()
}

fn write_runtime_manifest(bytes: &[u8]) -> PathBuf {
    let dir = env::temp_dir().join(format!("hogd-dev-runtime-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir(&dir).expect("bug: fake runtime manifest dir should be created");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .expect("bug: fake runtime manifest dir permissions should be set");
    }
    let path = dir.join("runtime-manifest.json");
    fs::write(&path, bytes).expect("bug: fake runtime manifest should be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("bug: fake runtime manifest permissions should be set");
    }
    path
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(Sha256::digest(bytes)))
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
    gateway_addr: SocketAddr,
}

fn start_fake_scheduler(mode: FakeSchedulerMode) -> FakeSchedulerRuntime {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("bug: fake scheduler runtime should build");
    let (addr, gateway_addr) = runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: fake scheduler should bind");
        let addr = listener
            .local_addr()
            .expect("bug: fake scheduler should have local addr");
        let gateway_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: fake scheduler gateway should bind");
        let gateway_addr = gateway_listener
            .local_addr()
            .expect("bug: fake scheduler gateway should have local addr");
        let scheduler = FakeScheduler::new(mode);
        let mut services = ServiceMap::new();
        services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
            Box::new(scheduler.clone()),
        )));
        let server = RpcServer::new(listener, services);
        let _server_handle = server.start().await;
        let mut gateway_services = ServiceMap::new();
        gateway_services.add_service(Box::new(SchedGateway_SchedGateway1_Dispatcher::new(
            Box::new(FakeSchedulerGateway(scheduler)),
        )));
        let gateway_server = RpcServer::new(gateway_listener, gateway_services);
        let _gateway_server_handle = gateway_server.start().await;
        (addr, gateway_addr)
    });
    FakeSchedulerRuntime {
        _runtime: runtime,
        addr,
        gateway_addr,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeSchedulerMode {
    Normal,
    Automine,
    AutomineSlowCommit,
    AutomineSerializeProbe,
    AutomineHangingCommit,
    AutomineCommitResponseLost,
    AutomineCommitError,
    AutomineCommitRejected,
    AutomineRuntimeGetTipError,
    AutomineMalformedProposal,
    AutomineNominateError,
    GetTipError,
    NetworkHashMismatch,
    NoGenesis,
}

impl FakeSchedulerMode {
    fn supports_block_driver(self) -> bool {
        matches!(
            self,
            Self::Automine
                | Self::AutomineSlowCommit
                | Self::AutomineSerializeProbe
                | Self::AutomineHangingCommit
                | Self::AutomineCommitResponseLost
                | Self::AutomineCommitError
                | Self::AutomineCommitRejected
                | Self::AutomineRuntimeGetTipError
                | Self::AutomineMalformedProposal
                | Self::AutomineNominateError
        )
    }
}

#[derive(Debug, Clone)]
struct FakeScheduler {
    tip: Arc<Mutex<Option<DbTip>>>,
    pending_submits: Arc<AtomicU64>,
    commit_in_progress: Arc<AtomicBool>,
    get_tip_calls: Arc<AtomicU64>,
    candidate: Arc<Mutex<Option<(BlockID, ConsensusValue)>>>,
    mode: FakeSchedulerMode,
}

impl FakeScheduler {
    fn new(mode: FakeSchedulerMode) -> Self {
        let scheduler = Self {
            tip: Arc::new(Mutex::new(None)),
            pending_submits: Arc::new(AtomicU64::new(0)),
            commit_in_progress: Arc::new(AtomicBool::new(false)),
            get_tip_calls: Arc::new(AtomicU64::new(0)),
            candidate: Arc::new(Mutex::new(None)),
            mode,
        };
        if mode != FakeSchedulerMode::NoGenesis {
            *scheduler.tip.lock().expect("bug: fake tip mutex poisoned") =
                Some(scheduler.genesis_tip());
        }
        scheduler
    }

    fn genesis_tip(&self) -> DbTip {
        let network_hash = if self.mode == FakeSchedulerMode::NetworkHashMismatch {
            [0xff; 32]
        } else {
            [0; 32]
        };
        DbTip {
            id: 0,
            storage_long_hash: [0u8; 2048].into(),
            default_wallet: DeployedContractHash {
                hash: SHA256Hash::from([0; 32]),
                contract_size: 0,
                contract_type: ContractType::kNative,
            },
            network_config: NetworkConfig {
                ext: ExtensionPoint::_0,
                max_sha256_buffers: 0,
                max_cross_invoke_depth: 0,
            },
            value: ConsensusValue {
                protocol: Protocol::new(0),
                ext: ExtensionPoint::_0,
                prev_hash: ConsensusValueHash::new(SHA256Hash::from([0; 32])),
                proposal_header: fake_proposal_header_with_network_hash(
                    0,
                    ValidatorID::default(),
                    TimePoint::new(0),
                    network_hash,
                ),
                proposal_txs_hash: expected_fake_proposal_txs_hash(),
                upgrades: VecM::default(),
            },
        }
    }

    fn admit_batch(&self, batch: &ParallelSignedTransactionBatch) -> Result<(), ProgramError> {
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
        if self.mode.supports_block_driver() {
            self.pending_submits.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn fake_proposal_header(
    height: i64,
    proposer: ValidatorID,
    block_timepoint: TimePoint,
) -> ProposalHeader {
    fake_proposal_header_with_network_hash(height, proposer, block_timepoint, [0; 32])
}

fn fake_proposal_header_with_network_hash(
    height: i64,
    proposer: ValidatorID,
    block_timepoint: TimePoint,
    network_hash: [u8; 32],
) -> ProposalHeader {
    let byte = u8::try_from(height).unwrap_or(u8::MAX);
    ProposalHeader {
        ext: ExtensionPoint::_0,
        network_hash: NetworkHash::new(SHA256Hash::from(network_hash)),
        config_hash: ConfigHash::new(SHA256Hash::from([0; 32])),
        proposer,
        block_timepoint,
        block_height: BlockHeight::from(height),
        txs_hash: TxsHash::new(SHA256Hash::from([byte; 32])),
        storage_hash: StorageShortHash::new(SHA256Hash::from([byte; 32])),
        events_hash: EventsHash::new(SHA256Hash::from([byte; 32])),
    }
}

fn fake_proposal_txs() -> ProposalTxs {
    ProposalTxs {
        ext: ExtensionPoint::_0,
        hash: ParallelTransactionHashBatch::new(ParallelBatch {
            segment_tokens: VecM(vec![1, 2]),
            segment_bytes: VecM(vec![4, 8]),
            xdr: BytesM(vec![0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3]),
        }),
        nondeterministic_data: ParallelNondeterministicProposalDataBatch::new(ParallelBatch {
            segment_tokens: VecM(vec![1]),
            segment_bytes: VecM(vec![4]),
            xdr: BytesM(vec![0xde, 0xad, 0xbe, 0xef]),
        }),
    }
}

fn expected_fake_proposal_txs_hash() -> ProposalTxsHash {
    ProposalTxsHash::new(SHA256Hash::from([
        0x3a, 0x3c, 0xe7, 0x82, 0x83, 0x94, 0x38, 0x26, 0xac, 0x43, 0x29, 0x1f, 0xab, 0x79, 0xbd,
        0x17, 0xc4, 0xad, 0x1d, 0x1e, 0xf9, 0xe2, 0x2d, 0x8c, 0x72, 0x31, 0xa9, 0x7c, 0xb6, 0x68,
        0xdc, 0xd9,
    ]))
}

fn hash_xdr<T: xdrs::WriteXDR>(value: &T) -> SHA256Hash {
    let encoded = xdrs::txs::encode(value)
        .expect("bug: fake scheduler protocol values should encode successfully");
    SHA256Hash::from(<[u8; 32]>::from(Sha256::digest(encoded)))
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
        self.admit_batch(&batch)
    }

    async fn schedulercoord_submit_vetted(
        &self,
        _validator_id: ValidatorID,
        _batch: ParallelSignedTransactionBatch,
    ) -> Result<(), ProgramError> {
        Ok(())
    }

    async fn schedulercoord_propose_block(
        &self,
        onto: BlockID,
        proposal_context: ProposalContext,
    ) -> Result<SchedulerProposalResult, ProgramError> {
        if !self.mode.supports_block_driver() {
            return Err(ProgramError::msg("not implemented in fake hogd-dev"));
        }
        if self.mode == FakeSchedulerMode::AutomineNominateError {
            return Err(ProgramError::msg("fake proposal RPC failure"));
        }
        if self.pending_submits.load(Ordering::SeqCst) == 0 {
            return Ok(SchedulerProposalResult::SchedulerProposalError);
        }

        let tip = self
            .tip
            .lock()
            .expect("bug: fake tip mutex poisoned")
            .clone()
            .expect("bug: automine should only run after genesis load");
        let next_height = *tip.value.proposal_header.block_height + 1;
        let block_id = tip.id + 1;
        if onto != tip.id {
            return Ok(SchedulerProposalResult::SchedulerProposalError);
        }
        let mut transactions = fake_proposal_txs();
        if self.mode == FakeSchedulerMode::AutomineMalformedProposal {
            transactions.hash.segment_tokens.0.pop();
        }
        let header = fake_proposal_header(
            next_height,
            proposal_context.proposer,
            proposal_context.block_timepoint,
        );
        if self.mode != FakeSchedulerMode::AutomineMalformedProposal {
            let expected = ConsensusValue {
                protocol: proposal_context.protocol,
                ext: ExtensionPoint::_0,
                prev_hash: ConsensusValueHash::new(hash_xdr(&tip.value)),
                proposal_header: header.clone(),
                proposal_txs_hash: expected_fake_proposal_txs_hash(),
                upgrades: VecM::default(),
            };
            *self
                .candidate
                .lock()
                .expect("bug: fake candidate mutex poisoned") = Some((block_id, expected));
        }
        Ok(SchedulerProposalResult::SchedulerProposalOK {
            proposed_block: SchedulerProposedBlock {
                block_id,
                txs: ParallelSignedTransactionBatch::new(ParallelBatch {
                    segment_tokens: VecM(vec![1]),
                    segment_bytes: VecM(vec![1]),
                    xdr: BytesM(vec![0x01]),
                }),
                proposal: Proposal {
                    header,
                    transactions,
                },
            },
        })
    }

    async fn schedulercoord_validate_block(
        &self,
        _onto: BlockID,
        _proposal: Proposal,
    ) -> Result<SchedulerValidationResult, ProgramError> {
        Err(ProgramError::msg("not implemented in fake hogd-dev"))
    }

    async fn schedulercoord_commit_block(
        &self,
        block_id: BlockID,
        value: ConsensusValue,
    ) -> Result<SchedulerCommitOutcome, ProgramError> {
        if !self.mode.supports_block_driver() {
            return Err(ProgramError::msg("not implemented in fake hogd-dev"));
        }
        if self.mode == FakeSchedulerMode::AutomineSlowCommit {
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        if self.mode == FakeSchedulerMode::AutomineHangingCommit {
            if let Ok(path) = env::var(COMMIT_ENTERED_FILE_ENV) {
                fs::write(path, b"entered").map_err(|error| {
                    ProgramError::msg(format!("failed to write commit marker: {error}"))
                })?;
            }
            tokio::time::sleep(Duration::from_hours(1)).await;
        }
        if self.mode == FakeSchedulerMode::AutomineCommitError {
            return Err(ProgramError::msg("fake commit RPC failure"));
        }
        {
            let mut candidate = self
                .candidate
                .lock()
                .expect("bug: fake candidate mutex poisoned");
            if candidate.as_ref() != Some(&(block_id, value.clone())) {
                return Ok(SchedulerCommitOutcome::SchedulerCommitInvalidConsensusValue);
            }
            if self.mode == FakeSchedulerMode::AutomineCommitRejected {
                return Ok(SchedulerCommitOutcome::SchedulerCommitInvalidConsensusValue);
            }
            candidate.take();
        }
        if self.mode == FakeSchedulerMode::AutomineSerializeProbe {
            if self.commit_in_progress.swap(true, Ordering::SeqCst) {
                return Err(ProgramError::msg("overlapping fake scheduler commit"));
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
            let pending = self.pending_submits.fetch_sub(1, Ordering::SeqCst);
            self.commit_in_progress.store(false, Ordering::SeqCst);
            if pending == 0 {
                self.pending_submits.store(0, Ordering::SeqCst);
                return Ok(SchedulerCommitOutcome::SchedulerCommitBlockNotFound);
            }
        } else if self.pending_submits.swap(0, Ordering::SeqCst) == 0 {
            return Ok(SchedulerCommitOutcome::SchedulerCommitBlockNotFound);
        }

        let mut guard = self.tip.lock().expect("bug: fake tip mutex poisoned");
        let tip = guard
            .as_mut()
            .expect("bug: automine should only run after genesis load");
        tip.id = block_id;
        tip.value = value;
        if self.mode == FakeSchedulerMode::AutomineCommitResponseLost {
            return Err(ProgramError::msg("fake lost commit response"));
        }
        Ok(SchedulerCommitOutcome::SchedulerCommitOK)
    }

    async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
        if self.mode == FakeSchedulerMode::GetTipError {
            return Err(ProgramError::msg("fake get_tip failure"));
        }
        let call = self.get_tip_calls.fetch_add(1, Ordering::SeqCst);
        if self.mode == FakeSchedulerMode::AutomineRuntimeGetTipError && call >= 2 {
            return Err(ProgramError::msg("fake runtime get_tip failure"));
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
        *self.tip.lock().expect("bug: fake tip mutex poisoned") = Some(self.genesis_tip());
        Ok(DbSchedResult::DbSchedResultOK)
    }

    async fn schedulercoord_commit_genesis_ceremony(
        &self,
        _request: groundhog_scheduler::GenesisCeremonyRequest,
    ) -> Result<groundhog_scheduler::GenesisCeremonyResult, ProgramError> {
        Err(ProgramError::msg("not implemented in fake hogd-dev"))
    }

    async fn schedcoord_lookup_transactions(
        &self,
        hashes: TransactionHashBatch,
    ) -> Result<OptSignedTransactionBatch, ProgramError> {
        // One slot per queried hash: nothing is ever pending in the fake,
        // but each hash still gets an explicit per-transaction answer.
        Ok(VecM(hashes.0.into_iter().map(|_| None).collect()))
    }
}

struct FakeSchedulerGateway(FakeScheduler);

#[async_trait]
impl SchedGateway_SchedGateway1 for FakeSchedulerGateway {
    async fn schedgateway_intro(
        &self,
        _protocol: ProtocolCommit,
    ) -> Result<SchedGatewayIntroOutcome, ProgramError> {
        Ok(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess)
    }

    async fn schedgateway_submit_tx_batch(
        &self,
        batch: ParallelSignedTransactionBatch,
    ) -> Result<(), ProgramError> {
        self.0.admit_batch(&batch)
    }

    async fn schedgateway_submit_tx_batch_with_outcomes(
        &self,
        batch: ParallelSignedTransactionBatch,
    ) -> Result<SchedGatewaySubmitOutcomeBatch, ProgramError> {
        self.0.admit_batch(&batch)?;
        Ok(VecM(vec![
            SchedGatewaySubmitOutcome::SchedGatewaySubmitAccepted;
            batch.segment_tokens.0.len()
        ]))
    }

    async fn schedgateway_get_tip(&self) -> Result<ExpectedDBTip, ProgramError> {
        match self
            .0
            .tip
            .lock()
            .expect("bug: fake tip mutex poisoned")
            .clone()
        {
            Some(db_tip) => Ok(ExpectedDBTip::kRPCNone { db_tip }),
            None => Ok(ExpectedDBTip::Default {
                error: RPCError::kRPCNotFound.0,
            }),
        }
    }

    async fn schedgateway_lookup_tx(
        &self,
        hashes: TransactionHashBatch,
    ) -> Result<OptSignedTransactionBatch, ProgramError> {
        Ok(VecM(hashes.0.into_iter().map(|_| None).collect()))
    }

    async fn schedgateway_pending_tx_count(&self) -> Result<PendingTxCount, ProgramError> {
        i64::try_from(self.0.pending_submits.load(Ordering::SeqCst))
            .map_err(|_| ProgramError::msg("fake pending count overflow"))
    }
}

// ---------------------------------------------------------------------------
// Fake DbGateway
// ---------------------------------------------------------------------------

struct FakeDbGatewayRuntime {
    _runtime: Runtime,
    addr: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeDbGatewayMode {
    Normal,
    RpcFail,
    IntroRejected,
}

fn start_fake_db_gateway(mode: FakeDbGatewayMode) -> FakeDbGatewayRuntime {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("bug: fake DbGateway runtime should build");
    let addr = runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bug: fake DbGateway should bind");
        let addr = listener
            .local_addr()
            .expect("bug: fake DbGateway should have local addr");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(DbGateway_DbGateway1_Dispatcher::new(Box::new(
            FakeDbGateway { mode },
        ))));
        let server = RpcServer::new(listener, services);
        let _server_handle = server.start().await;
        addr
    });
    FakeDbGatewayRuntime {
        _runtime: runtime,
        addr,
    }
}

struct FakeDbGateway {
    mode: FakeDbGatewayMode,
}

#[async_trait]
impl DbGateway_DbGateway1 for FakeDbGateway {
    async fn dbgateway_intro(
        &self,
        _arg0: ProtocolCommit,
    ) -> Result<DbGatewayIntroOutcome, ProgramError> {
        match self.mode {
            FakeDbGatewayMode::Normal => {}
            FakeDbGatewayMode::RpcFail => return Err(ProgramError::msg("fake DbGateway rpc-fail")),
            FakeDbGatewayMode::IntroRejected => {
                return Ok(DbGatewayIntroOutcome::DbGatewayIntroMismatchedProtocol);
            }
        }
        Ok(DbGatewayIntroOutcome::DbGatewayIntroSuccess)
    }

    async fn dbgateway_get_items(
        &self,
        arg0: AddressAndKeyBatch,
    ) -> Result<DbGatewayResult, ProgramError> {
        let items = arg0
            .0
            .into_iter()
            .map(|request| match request.key {
                StorageKey::ContractData { contract_data_key }
                    if contract_data_key.as_ref() == CANNED_STORAGE_KEY.as_slice() =>
                {
                    DbGatewayItemResult::kRPCNone {
                        item: WireStorageItem::kItemTypeBytestring {
                            bytestring: BytesM(CANNED_STORAGE_VALUE.to_vec()),
                        },
                    }
                }
                _ => DbGatewayItemResult::Default {
                    error: RPCError::kRPCNotFound.0,
                },
            })
            .collect();
        Ok(DbGatewayResult::kRPCNone {
            payload: DbGatewayPayload {
                block_height: groundhog_xdr::types::BlockHeight::from(CANNED_STORAGE_HEIGHT),
                items: VecM(items),
            },
        })
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
    /// Intro returns a protocol mismatch outcome.
    IntroRejected,
    /// Every RPC method (including intro) returns a `ProgramError`.
    RpcFail,
    /// Intro succeeds but all data-plane RPCs (`get_transaction_info`,
    /// `get_events`) return a `ProgramError`. Used to test runtime indexer
    /// failures that occur after successful startup.
    RuntimeRpcFail,
}

/// Committed tx set: maps `TransactionHash` bytes to `TxInfo`.
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

    /// Build a minimal `ProposedTransaction` to stand in for a `TxInfo.tx`
    /// body. The transaction is not valid on-chain; it only needs to be
    /// present so the committed dataset round-trips through the indexer RPC.
    fn dummy_proposed_transaction() -> groundhog_xdr::transaction::ProposedTransaction {
        use groundhog_xdr::auth_claim::{AuthClaim, AuthClaim_source_address_claim};
        use groundhog_xdr::contract::ContractEntrypointID;
        use groundhog_xdr::transaction::{
            Invocation, NondeterministicProposalData, ProposedTransaction, SignedTransaction,
            Transaction, Uniquifier,
        };
        use groundhog_xdr::types::{ContractAddress, GasAmount};
        ProposedTransaction {
            signed_tx: SignedTransaction {
                tx: Transaction {
                    invocation: Invocation {
                        invoked_address: ContractAddress::from([0u8; 32]),
                        entrypoint: ContractEntrypointID::from([0u8; 8]),
                        gas_limit: GasAmount::from(0u64),
                        calldata: BytesM(vec![]),
                    },
                    uniquifier: Uniquifier {
                        block_expiry_height: BlockHeight::from(0i64),
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
            },
            nondeterministic_proposal_data: NondeterministicProposalData {
                nondeterministic_bits: BytesM(vec![]),
                nondeterministic_bit_count: 0,
                observed_queue_entry_sequence: VecM(vec![]),
            },
        }
    }

    fn build_committed_set() -> CommittedSet {
        use groundhog_xdr::{
            event::TxEventLog, exec_result::ExecResult, transaction::InvocationFrameOutput,
        };

        let mut map = HashMap::new();

        let tx_a = TxInfo {
            height: BlockHeight::from(CANNED_HEIGHT_A),
            tx: Self::dummy_proposed_transaction(),
            events: TxEventLog {
                tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
                exec_result: ExecResult::ExecResultOK,
                root_output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(vec![]),
                },
                events: VecM(vec![]),
            },
        };
        map.insert(CANNED_TX_HASH_A, tx_a);

        let tx_b = TxInfo {
            height: BlockHeight::from(CANNED_HEIGHT_B),
            tx: Self::dummy_proposed_transaction(),
            events: TxEventLog {
                tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
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
        use groundhog_xdr::{event::TxEmittedEvent, types::ContractAddress};

        // Page 1: two events, non-empty cursor pointing to page 2.
        let event1 = IdentifiedEvent {
            tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
            block_height: BlockHeight::from(CANNED_HEIGHT_A),
            sequence_number: 0,
            emitted_event: TxEmittedEvent {
                contract_address: ContractAddress::from([0x01u8; 32]),
                num_topics: 0,
                serialized_event: BytesM(vec![]),
            },
        };
        let event2 = IdentifiedEvent {
            tx_hash: SHA256Hash::from(CANNED_TX_HASH_A),
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
            tx_hash: SHA256Hash::from(CANNED_TX_HASH_B),
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
        _arg0: ProtocolCommit,
    ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        if self.mode == FakeIndexerMode::IntroRejected {
            return Ok(IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol);
        }
        // RuntimeRpcFail: intro succeeds, data-plane RPCs fail.
        Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
    }

    #[tracing::instrument(skip_all)]
    async fn get_transaction_info(
        &self,
        arg0: TransactionHash,
    ) -> Result<TxInfoResult, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail || self.mode == FakeIndexerMode::RuntimeRpcFail {
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
    async fn get_block_by_height(
        &self,
        _height: groundhog_xdr::types::BlockHeight,
    ) -> Result<groundhog_indexer::BlockLookupResult, ProgramError> {
        Err(ProgramError::msg("fake indexer does not serve blocks"))
    }

    #[tracing::instrument(skip_all)]
    async fn get_events(&self, arg0: EventQuery) -> Result<EventPageResult, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail || self.mode == FakeIndexerMode::RuntimeRpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        let (events, next_cursor) = self.resolve_event_page(&arg0.cursor.0)?;
        Ok(EventPageResult::IndexerQueryOk {
            page: EventPage {
                events: VecM(events.clone()),
                cursor: BytesM(next_cursor.clone()),
                latest_block: BlockHeight::from(CANNED_HEIGHT_B),
                oldest_block: BlockHeight::from(0_i64),
            },
        })
    }

    #[tracing::instrument(skip_all)]
    async fn get_transactions(
        &self,
        _arg0: TransactionQuery,
    ) -> Result<TransactionPageResult, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail || self.mode == FakeIndexerMode::RuntimeRpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        // No canned transaction-list dataset; report an exhausted stream.
        Ok(TransactionPageResult::IndexerQueryOk {
            page: TransactionPage {
                transactions: VecM(vec![]),
                cursor: BytesM(vec![]),
                latest_block: BlockHeight::from(CANNED_HEIGHT_B),
                oldest_block: BlockHeight::from(0_i64),
            },
        })
    }

    #[tracing::instrument(skip_all)]
    async fn get_transactions_by_address(
        &self,
        _arg0: AddressTransactionQuery,
    ) -> Result<TransactionRefPageResult, ProgramError> {
        if self.mode == FakeIndexerMode::RpcFail || self.mode == FakeIndexerMode::RuntimeRpcFail {
            return Err(ProgramError::msg("fake indexer rpc-fail mode"));
        }
        // No canned per-address dataset; report an exhausted stream.
        Ok(TransactionRefPageResult::IndexerQueryOk {
            page: TransactionRefPage {
                refs: VecM(vec![]),
                cursor: BytesM(vec![]),
                latest_block: BlockHeight::from(CANNED_HEIGHT_B),
                oldest_block: BlockHeight::from(0_i64),
            },
        })
    }

    #[tracing::instrument(skip_all)]
    async fn get_transactions_by_block(
        &self,
        _query: groundhog_indexer::BlockTransactionQuery,
    ) -> Result<TransactionRefPageResult, ProgramError> {
        Err(ProgramError::msg(
            "fake indexer does not serve transactions by block",
        ))
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
    use groundhog_xdr::transaction::TransactionHash;
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

    fn known_hash(bytes: [u8; 32]) -> TransactionHash {
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

    fn expect_event_page(
        result: groundhog_indexer::EventPageResult,
    ) -> groundhog_indexer::EventPage {
        match result {
            groundhog_indexer::EventPageResult::IndexerQueryOk { page } => page,
            other => panic!("expected successful event page, got {other:?}"),
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
        let page = expect_event_page(
            client
                .get_events(&empty_event_query())
                .await
                .expect("bug: get_events should succeed"),
        );

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
        let page = expect_event_page(
            client
                .get_events(&event_query_with_cursor(CANNED_EVENT_CURSOR))
                .await
                .expect("bug: get_events page 2 should succeed"),
        );

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
        let page1 = expect_event_page(
            client
                .get_events(&empty_event_query())
                .await
                .expect("bug: page 1 should succeed"),
        );
        let page2 = expect_event_page(
            client
                .get_events(&event_query_with_cursor(page1.cursor.0.as_slice()))
                .await
                .expect("bug: page 2 should succeed"),
        );

        // Assert: all three events collected across two pages
        let total = page1.events.0.len() + page2.events.0.len();
        assert_eq!(total, 3, "multi-page walk should yield 3 events total");

        // Verify first page events reference hash A
        for event in &page1.events.0 {
            assert_eq!(
                *event.tx_hash, CANNED_TX_HASH_A,
                "page 1 events should reference tx hash A"
            );
        }
        // Verify second page event references hash B
        assert_eq!(
            *page2.events.0[0].tx_hash, CANNED_TX_HASH_B,
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
