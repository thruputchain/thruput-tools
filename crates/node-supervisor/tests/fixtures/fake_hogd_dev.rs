use std::{
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
use xdr_common::VecM;

const MODE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_MODE";
/// Optional path the fake writes its PID to before printing readiness.
/// Tests use this to verify cleanup (poll `kill(pid, 0)` until ESRCH).
const PID_FILE_ENV: &str = "GROUNDHOG_FAKE_HOGD_DEV_PID_FILE";
static TERMINATED: AtomicBool = AtomicBool::new(false);

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
        _ => {}
    }
}

fn run_ready_then_exit() {
    write_pid_file_if_requested();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!("{}", readiness(scheduler.addr));
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
    println!("{}", readiness(scheduler.addr));
    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"ready"}}"#);

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    eprintln!(r#"{{"level":"info","target":"hogd_dev","message":"stopping"}}"#);
}

fn run_dead_scheduler() {
    install_sigterm_handler();
    write_pid_file_if_requested();
    println!("{}", readiness(dead_loopback_addr()));

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term() {
    ignore_sigterm();
    let scheduler = start_fake_scheduler(FakeSchedulerMode::Normal);
    println!("{}", readiness(scheduler.addr));
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
    println!("{}", readiness(scheduler.addr));

    while !TERMINATED.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_ignore_term_loop() {
    loop {
        thread::sleep(Duration::from_secs(60));
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

fn readiness(scheduler: SocketAddr) -> String {
    format!(
        r#"{{"status":"ready","network_id":"groundhog-local","endpoints":{{"scheduler":"{scheduler}"}}}}"#
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
