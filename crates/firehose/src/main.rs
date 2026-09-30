use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use axum::{Router, extract::State, http::StatusCode, routing::get};
use clap::{Parser, Subcommand};
use thruput_firehose::{
    Error,
    client::{self, Client, Outcome},
    metrics::Metrics,
    store::Store,
};
use thruput_indexer::EventIndexerPullResult;
use thruput_xdr::{PROTOCOL_VERSION, network::ProtocolCommit};
use tokio::sync::watch;

mod discovery;

#[derive(Debug, Parser)]
#[command(
    name = "hog-firehose-store",
    version,
    about = "Experimental committed-block archive writer"
)]
struct Cli {
    /// Emit machine-readable discovery and tail output.
    #[arg(long, global = true)]
    json: bool,
    /// Exact 20-byte hogp protocol token; defaults to the compiled bindings.
    #[arg(long, global = true)]
    protocol: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Identify `EventIndexer1` among explicit candidate addresses.
    #[command(
        after_help = "Exit codes: 0 = at least one match; 1 = no match, all probes answered; 2 = no match, at least one probe failed (inconclusive)."
    )]
    Discover {
        #[arg(required = true)]
        candidates: Vec<String>,
    },
    /// Verify the local archive and exit without connecting to a node.
    Verify {
        #[arg(long)]
        store: PathBuf,
    },
    /// Follow committed blocks into the archive.
    Run(Config),
    /// Follow and print one summary line per newly stored block.
    Tail(Config),
}

/// Per-block stdout reporting, selected by the subcommand and `--json`.
#[derive(Clone, Copy, Debug)]
enum Tail {
    Off,
    Text,
    Json,
}

#[derive(Debug, clap::Args)]
struct Config {
    /// Explicit `EventIndexer1` address; never inferred from other endpoints.
    #[arg(long)]
    event_endpoint: String,
    /// Directory holding the immutable archive.
    #[arg(long)]
    store: PathBuf,
    /// Health and Prometheus listener.
    #[arg(long, default_value = "127.0.0.1:9550")]
    listen: std::net::SocketAddr,
    /// Optional `SchedGateway` address for independent chain-tip lag sampling.
    #[arg(long)]
    tip_endpoint: Option<String>,
}

#[tokio::main]
async fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if matches!(cli.command, Command::Discover { .. }) {
        // Unknown listeners are expected here. Attribute failures to candidates,
        // including diagnostics from the transport's independently spawned tasks.
        filter = filter
            .add_directive("rpc_driver=off".parse()?)
            .add_directive("thruput_x23_transport=off".parse()?);
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    let protocol = match cli.protocol {
        Some(text) => ProtocolCommit::try_from(text.as_bytes())
            .map_err(|_| "protocol token must be exactly 20 bytes")?,
        None => PROTOCOL_VERSION,
    };
    match cli.command {
        Command::Discover { candidates } => {
            let report = discovery::Report::probe(candidates, protocol).await;
            report.print(cli.json)?;
            return Ok(report.exit_code().into());
        }
        Command::Verify { store } => {
            let start = Instant::now();
            let store = Store::open(&store)?;
            println!(
                "{}",
                serde_json::json!({"blocks": store.blocks, "height": store.height(), "bytes": store.bytes, "recoverySeconds": start.elapsed().as_secs_f64()})
            );
        }
        Command::Run(config) => run_store(config, protocol, Tail::Off).await?,
        Command::Tail(config) if cli.json => run_store(config, protocol, Tail::Json).await?,
        Command::Tail(config) => run_store(config, protocol, Tail::Text).await?,
    }
    Ok(std::process::ExitCode::SUCCESS)
}

async fn health(State(metrics): State<Metrics>) -> (StatusCode, &'static str) {
    if metrics.is_healthy() {
        (StatusCode::OK, "ok\n")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "ingest unavailable\n")
    }
}

async fn exposition(
    State(metrics): State<Metrics>,
) -> (StatusCode, [(&'static str, &'static str); 1], Vec<u8>) {
    match metrics.render() {
        Ok(bytes) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4")],
            bytes,
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "text/plain")],
            error.to_string().into_bytes(),
        ),
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            }
            Err(error) => {
                tracing::error!(%error, "cannot install SIGTERM handler");
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_store(
    config: Config,
    protocol: ProtocolCommit,
    tail: Tail,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let store = Store::open(&config.store)?;
    tracing::info!(
        blocks = store.blocks,
        height = store.height(),
        recovery_seconds = started.elapsed().as_secs_f64(),
        "archive recovered and verified"
    );
    let metrics = Metrics::new()?;
    update_store_metrics(&metrics, &store);
    let router = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(exposition))
        .with_state(metrics.clone());
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    let (stop, mut stopped) = watch::channel(false);
    let http = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = stopped.changed().await;
    });
    let tip_task = config
        .tip_endpoint
        .as_ref()
        .map(|endpoint| tokio::spawn(sample_tip(endpoint.clone(), protocol, metrics.clone())));
    let http = std::future::IntoFuture::into_future(http);
    tokio::pin!(http);
    let (result, http_done) = tokio::select! {
        result = ingest(&config, protocol, store, &metrics, tail) => (result.map_err(Into::into), false),
        result = &mut http => (result.map_err(Into::into), true),
        () = shutdown() => (Ok(()), false),
    };
    metrics.set_healthy(false);
    let _ = stop.send(true);
    if let Some(task) = tip_task {
        task.abort();
        let _ = task.await;
    }
    if !http_done {
        http.await?;
    }
    result
}

fn update_store_metrics(metrics: &Metrics, store: &Store) {
    metrics.height.set(store.height());
    metrics
        .store
        .set(i64::try_from(store.bytes).unwrap_or(i64::MAX));
    metrics
        .allocated
        .set(i64::try_from(store.allocated_bytes).unwrap_or(i64::MAX));
}

async fn sample_tip(endpoint: String, protocol: ProtocolCommit, metrics: Metrics) {
    sample_tip_with(protocol, metrics, |deadline| {
        client::connect_endpoint(&endpoint, deadline, |addr| async move {
            thruput_x23_transport::connect(
                "scheduler_tip",
                &addr.to_string(),
                1,
                thruput_scheduler::sched_gateway::SchedGateway_SchedGateway1_Client::new,
            )
            .await
        })
    })
    .await;
}

async fn sample_tip_with<F, Fut>(protocol: ProtocolCommit, metrics: Metrics, mut connect: F)
where
    F: FnMut(tokio::time::Instant) -> Fut,
    Fut: std::future::Future<
            Output = Result<
                thruput_scheduler::sched_gateway::SchedGateway_SchedGateway1_Client,
                Error,
            >,
        >,
{
    use thruput_scheduler::{rpc_types::ExpectedDBTip, sched_gateway::SchedGatewayIntroOutcome};
    loop {
        let result: Result<(), Error> = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            let client = tokio::time::timeout_at(deadline, async {
                let client = connect(deadline).await?;
                if client.schedgateway_intro(&protocol).await?
                    != SchedGatewayIntroOutcome::SchedGatewayIntroSuccess
                {
                    return Err(Error::Invalid("scheduler intro rejected".into()));
                }
                Ok::<_, Error>(client)
            })
            .await
            .map_err(|_| Error::Invalid("tip connection timed out".into()))??;
            loop {
                let tip =
                    tokio::time::timeout(Duration::from_secs(2), client.schedgateway_get_tip())
                        .await
                        .map_err(|_| Error::Invalid("tip sample timed out".into()))??;
                if let ExpectedDBTip::kRPCNone { db_tip } = tip {
                    let lag =
                        (*db_tip.value.proposal_header.block_height - metrics.height.get()).max(0);
                    #[allow(clippy::cast_precision_loss)]
                    metrics.lag.set(lag as f64);
                } else {
                    return Err(Error::Invalid("tip unavailable".into()));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        .await;
        metrics.lag.set(f64::NAN);
        tracing::debug!(?result, "tip sampler reconnecting");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// XDR size of one component of a stored block, for the `tail` size breakdown.
fn encoded_len<T: xdrs::WriteXDR>(value: &T) -> Result<usize, Error> {
    Ok(xdrs::txs::encode(value)
        .map_err(|e| Error::Xdr(e.to_string()))?
        .len())
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
async fn ingest(
    config: &Config,
    protocol: ProtocolCommit,
    mut store: Store,
    metrics: &Metrics,
    tail: Tail,
) -> Result<(), Error> {
    let mut previous = Instant::now();
    let mut parked_total = 0.;
    loop {
        let connected = tokio::time::timeout(
            Duration::from_secs(5),
            Client::connect(&config.event_endpoint),
        )
        .await;
        let client = match connected {
            Ok(Ok(client)) => client,
            other => {
                metrics.set_healthy(false);
                tracing::warn!(?other, "event connection failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut parked = false;
        loop {
            let start = Instant::now();
            let pull = client.pull(protocol, store.cursor(parked));
            tokio::pin!(pull);
            let mut ticker = tokio::time::interval(Duration::from_millis(100));
            let result = loop {
                tokio::select! {
                    result = &mut pull => break result,
                    _ = ticker.tick(), if parked => { metrics.parked.set(parked_total + start.elapsed().as_secs_f64()); }
                }
            };
            metrics.pull.observe(start.elapsed().as_secs_f64());
            if parked {
                parked_total += start.elapsed().as_secs_f64();
                metrics.parked.set(parked_total);
            }
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    metrics
                        .outcomes
                        .with_label_values(&["transport_error"])
                        .inc();
                    metrics.set_healthy(false);
                    tracing::warn!(%error, "pull transport failed; reconnecting from durable cursor");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    break;
                }
            };
            let outcome = client::outcome(&result);
            if !matches!(outcome, Outcome::Block) {
                metrics.outcomes.with_label_values(&[outcome.label()]).inc();
            }
            match outcome {
                Outcome::Block => {
                    let EventIndexerPullResult::EventIndexerPullBlock { pulled_block } = result
                    else {
                        return Err(Error::Invalid("invalid block union".into()));
                    };
                    if pulled_block.origin.protocol != protocol {
                        return Err(Error::Invalid("block origin protocol mismatch".into()));
                    }
                    let before = store.bytes;
                    let (updated, pulled_block, appended) =
                        tokio::task::spawn_blocking(move || {
                            let appended = store.append(&pulled_block);
                            (store, pulled_block, appended)
                        })
                        .await?;
                    store = updated;
                    if appended? {
                        metrics.outcomes.with_label_values(&["block"]).inc();
                        let payload = &pulled_block.block.payload;
                        let events: usize =
                            payload.event_logs.iter().map(|log| log.events.len()).sum();
                        metrics.blocks.inc();
                        metrics.bytes.inc_by(store.bytes - before);
                        metrics.events.observe(events as f64);
                        let elapsed = previous.elapsed().as_secs_f64();
                        previous = Instant::now();
                        tracing::debug!(height = store.height(), "pull block durably stored");
                        match tail {
                            Tail::Off => {}
                            Tail::Text => println!(
                                "height={} events={events} txs={} payload_bytes={} since_previous={elapsed:.6}s",
                                store.height(),
                                payload.tx_bodies.len(),
                                encoded_len(payload)?
                            ),
                            Tail::Json => println!(
                                "{}",
                                serde_json::json!({"height": store.height(), "events": events, "transactions": payload.tx_bodies.len(), "payloadBytes": encoded_len(payload)?, "consensusBytes": encoded_len(&payload.consensus_value)?, "eventsBytes": encoded_len(&payload.event_logs)?, "transactionBytes": encoded_len(&payload.tx_bodies)?, "archiveBytes": store.bytes - before, "sincePreviousSeconds": elapsed})
                            ),
                        }
                    } else {
                        metrics.outcomes.with_label_values(&["replay"]).inc();
                        tracing::warn!(
                            height = store.height(),
                            "identical archive replay; backing off"
                        );
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    update_store_metrics(metrics, &store);
                    metrics.set_healthy(true);
                    parked = false;
                }
                Outcome::CaughtUp => {
                    let EventIndexerPullResult::EventIndexerPullCaughtUp { origin } = result else {
                        return Err(Error::Invalid("invalid caught-up union".into()));
                    };
                    if origin.protocol != protocol {
                        return Err(Error::Invalid("caught-up origin protocol mismatch".into()));
                    }
                    store = tokio::task::spawn_blocking(move || {
                        store.adopt(&origin)?;
                        Ok::<_, Error>(store)
                    })
                    .await??;
                    update_store_metrics(metrics, &store);
                    metrics.set_healthy(true);
                    tracing::info!(
                        height = store.height(),
                        "pull caught up; parking until publication"
                    );
                    parked = true;
                }
                Outcome::NotReady => {
                    metrics.set_healthy(false);
                    parked = false;
                    tracing::info!(?result, "pull not ready; waiting for outbox initialization");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Outcome::InternalFailure => {
                    metrics.set_healthy(false);
                    parked = false;
                    tracing::warn!(?result, "pull internal failure; retrying durable cursor");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Outcome::MismatchedProtocol => {
                    tracing::error!(?result, "pull protocol mismatch");
                    return Err(Error::Invalid(format!("protocol mismatch: {result:?}")));
                }
                Outcome::ProgressConflict => {
                    tracing::error!(?result, "pull progress conflict");
                    return Err(Error::Invalid(format!("progress conflict: {result:?}")));
                }
                Outcome::Unknown => {
                    tracing::error!(?result, "unknown pull outcome");
                    return Err(Error::Invalid(format!("unknown outcome: {result:?}")));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpc_driver::{ProgVers, RpcError, RpcServer, ServiceMap, handler::RpcHandler};
    use std::{
        collections::VecDeque,
        path::Path,
        sync::{Arc, Mutex},
    };
    use thruput_indexer::{EventIndexerPullRequest, EventIndexerPulledBlock};

    struct TipServer {
        sampled: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl RpcHandler for TipServer {
        fn prog_vers(&self) -> ProgVers {
            ProgVers::new(0x7ee7_aaaa, 1)
        }

        async fn dispatch(&self, procedure: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
            match procedure {
                0 => {
                    let protocol: ProtocolCommit = xdrs::txs::decode(data).unwrap();
                    assert_eq!(protocol, PROTOCOL_VERSION);
                    Ok(xdrs::txs::encode(
                        &thruput_scheduler::SchedGatewayIntroOutcome::SchedGatewayIntroSuccess,
                    )
                    .unwrap())
                }
                2 => {
                    self.sampled.notify_one();
                    Ok(
                        xdrs::txs::encode(&thruput_scheduler::ExpectedDBTip::Default { error: 1 })
                            .unwrap(),
                    )
                }
                _ => panic!("unexpected scheduler procedure {procedure}"),
            }
        }
    }

    #[tokio::test]
    async fn tip_sampler_reaches_later_address_when_first_connection_hangs() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        let hung = "127.0.0.1:1".parse().unwrap();
        let sampled = Arc::new(tokio::sync::Notify::new());
        let mut services = ServiceMap::new();
        services.add_service(Box::new(TipServer {
            sampled: sampled.clone(),
        }));
        let server = RpcServer::new(listener, services).start().await;
        let attempted = Arc::new(Mutex::new(Vec::new()));
        let start = tokio::time::Instant::now();
        let run = sample_tip_with(PROTOCOL_VERSION, Metrics::new().unwrap(), |deadline| {
            let attempted = attempted.clone();
            client::connect_addresses("test.invalid:1234", [hung, live], deadline, move |addr| {
                attempted.lock().unwrap().push(addr);
                async move {
                    if addr == hung {
                        // Deterministically model a SYN that never connects or refuses.
                        std::future::pending().await
                    } else {
                        thruput_x23_transport::connect(
                            "scheduler_tip",
                            &addr.to_string(),
                            1,
                            thruput_scheduler::SchedGateway_SchedGateway1_Client::new,
                        )
                        .await
                    }
                }
            })
        });
        tokio::pin!(run);
        tokio::select! {
            () = &mut run => panic!("tip sampler unexpectedly stopped"),
            result = tokio::time::timeout(Duration::from_secs(3), sampled.notified()) => {
                result.unwrap();
            }
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(*attempted.lock().unwrap(), [hung, live]);
        server.abort();
    }

    /// Every bound comes from the real CLI defaults, as a deployment would get them.
    fn config(event_endpoint: &str, store: &Path) -> Config {
        let cli = Cli::parse_from([
            "hog-firehose-store",
            "run",
            "--event-endpoint",
            event_endpoint,
            "--store",
            store.to_str().unwrap(),
            "--listen",
            "127.0.0.1:0",
        ]);
        let Command::Run(config) = cli.command else {
            unreachable!()
        };
        config
    }

    struct Script {
        replies: Mutex<VecDeque<EventIndexerPullResult>>,
        states: Arc<Mutex<Vec<i32>>>,
    }

    #[async_trait::async_trait]
    impl RpcHandler for Script {
        fn prog_vers(&self) -> ProgVers {
            ProgVers::new(0x4321_aaac, 1)
        }
        async fn dispatch(&self, procedure: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
            assert_eq!(procedure, 10, "procedure 11 must never reach the wire");
            let request: EventIndexerPullRequest = xdrs::txs::decode(data).unwrap();
            self.states.lock().unwrap().push(request.progress.state().0);
            let reply = self.replies.lock().unwrap().pop_front();
            match reply {
                Some(reply) => Ok(xdrs::txs::encode(&reply).unwrap()),
                None => std::future::pending().await,
            }
        }
    }

    fn diagnostic(outcome: i32) -> EventIndexerPullResult {
        EventIndexerPullResult::Default {
            outcome,
            diagnostic: "scripted outcome".try_into().unwrap(),
        }
    }

    async fn server(
        replies: Vec<EventIndexerPullResult>,
    ) -> (
        String,
        Arc<Mutex<Vec<i32>>>,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let states = Arc::new(Mutex::new(Vec::new()));
        let mut services = ServiceMap::new();
        services.add_service(Box::new(Script {
            replies: Mutex::new(replies.into()),
            states: states.clone(),
        }));
        let task = RpcServer::new(listener, services).start().await;
        (endpoint, states, task)
    }

    #[tokio::test]
    async fn retries_transient_outcomes_stores_block_and_parks_after_caught_up() {
        let hex: String = include_str!("../tests/fixtures/bootstrap.hex")
            .split_whitespace()
            .collect();
        let block: EventIndexerPulledBlock = xdrs::txs::decode(&hex::decode(hex).unwrap()).unwrap();
        // Replay the recorded block with its original protocol identity.
        let protocol = block.origin.protocol;
        let replies = vec![
            diagnostic(2),
            diagnostic(5),
            EventIndexerPullResult::EventIndexerPullBlock {
                pulled_block: block.clone(),
            },
            EventIndexerPullResult::EventIndexerPullCaughtUp {
                origin: block.origin,
            },
        ];
        let (endpoint, states, task) = server(replies).await;
        let dir = tempfile::tempdir().unwrap();
        let config = config(&endpoint, dir.path());
        let metrics = Metrics::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let expected = [0, 0, 0, 1, 2];
        // Two transient outcomes sleep a second each, so a fixed budget leaves
        // almost no headroom and fails on a loaded runner. Wait for the state
        // the assertions describe, with a ceiling that only trips on a hang.
        let settled = async {
            loop {
                let pulls = states.lock().unwrap().len();
                if pulls >= expected.len() && metrics.parked.get() > 0. {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let result = tokio::select! {
            outcome = ingest(&config, protocol, store, &metrics, Tail::Off) => Some(outcome),
            () = settled => None,
            () = tokio::time::sleep(Duration::from_mins(1)) => {
                panic!("bug: ingest never reached the parked final pull")
            }
        };
        assert!(result.is_none(), "the final pull must remain parked");
        assert_eq!(*states.lock().unwrap(), expected);
        for label in ["not_ready", "internal_failure", "block", "caught_up"] {
            assert_eq!(metrics.outcomes.with_label_values(&[label]).get(), 1);
        }
        assert!(metrics.is_healthy());
        assert!(metrics.parked.get() > 0.);
        assert_eq!(Store::open(dir.path()).unwrap().height(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn protocol_and_progress_conflicts_stop_without_advancing_cursor() {
        for (code, label) in [
            (3, "mismatched_protocol"),
            (4, "progress_conflict"),
            (99, "unknown"),
        ] {
            let (endpoint, states, task) = server(vec![diagnostic(code)]).await;
            let dir = tempfile::tempdir().unwrap();
            let config = config(&endpoint, dir.path());
            let metrics = Metrics::new().unwrap();
            let store = Store::open(dir.path()).unwrap();
            assert!(
                ingest(&config, PROTOCOL_VERSION, store, &metrics, Tail::Off)
                    .await
                    .is_err()
            );
            assert_eq!(*states.lock().unwrap(), [0]);
            assert_eq!(metrics.outcomes.with_label_values(&[label]).get(), 1);
            assert_eq!(Store::open(dir.path()).unwrap().blocks, 0);
            task.abort();
        }
    }

    #[tokio::test]
    async fn identical_replays_are_counted_and_backed_off() {
        let hex: String = include_str!("../tests/fixtures/bootstrap.hex")
            .split_whitespace()
            .collect();
        let block: EventIndexerPulledBlock = xdrs::txs::decode(&hex::decode(hex).unwrap()).unwrap();
        // Replay the recorded block with its original protocol identity.
        let protocol = block.origin.protocol;
        let replies = vec![
            EventIndexerPullResult::EventIndexerPullBlock {
                pulled_block: block
            };
            4
        ];
        let (endpoint, states, task) = server(replies).await;
        let dir = tempfile::tempdir().unwrap();
        let config = config(&endpoint, dir.path());
        let metrics = Metrics::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let run = ingest(&config, protocol, store, &metrics, Tail::Off);
        tokio::pin!(run);
        // Start timing at the first replay so filesystem speed cannot affect the bound.
        tokio::select! {
            result = &mut run => panic!("unexpected exit: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), async {
                while metrics.outcomes.with_label_values(&["replay"]).get() == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }) => result.unwrap(),
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut run)
                .await
                .is_err()
        );
        assert_eq!(*states.lock().unwrap(), [0, 1]);
        assert_eq!(metrics.outcomes.with_label_values(&["replay"]).get(), 1);
        assert_eq!(metrics.outcomes.with_label_values(&["block"]).get(), 1);
        assert_eq!(metrics.blocks.get(), 1);
        tokio::select! {
            result = &mut run => panic!("unexpected exit: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(2), async {
                while metrics.outcomes.with_label_values(&["replay"]).get() < 2 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }) => result.unwrap(),
        }
        assert_eq!(*states.lock().unwrap(), [0, 1, 1]);
        assert_eq!(metrics.blocks.get(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn discovery_accepts_event_service_and_rejects_program_or_protocol_mismatch() {
        let (endpoint, states, task) = server(vec![diagnostic(2), diagnostic(3)]).await;
        assert!(client::discover(&endpoint, PROTOCOL_VERSION).await.unwrap());
        assert!(!client::discover(&endpoint, PROTOCOL_VERSION).await.unwrap());
        assert_eq!(*states.lock().unwrap(), [0, 0]);
        task.abort();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let task = RpcServer::new(listener, ServiceMap::new()).start().await;
        assert!(!client::discover(&endpoint, PROTOCOL_VERSION).await.unwrap());
        task.abort();
    }
}
