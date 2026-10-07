//! Bounded fanout of durable records. This module has no upstream client.
use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::api::FirehoseApiServer;
pub use crate::types::{Cursor, Record};
use groundhog_indexer::EventIndexerPulledBlock;
use groundhog_json_rpc_types::hex::HexBytes;
use jsonrpsee::{
    core::server::{ConnectionId, SubscriptionMessage, SubscriptionSink},
    server::{
        PingConfig, ServerBuilder, ServerConfig, ServerHandle,
        middleware::rpc::{Batch, Notification, Request, RpcServiceBuilder, RpcServiceT},
    },
    types::ErrorObjectOwned,
};
use serde::Serialize;
use tokio::sync::Notify;

/// Bounds apply independently of the upstream protocol's much larger ceiling.
#[derive(Clone, Debug, clap::Args)]
#[group(id = "SubscriptionConfig")]
pub struct Config {
    /// WebSocket subscription listener.
    #[arg(long, default_value = "127.0.0.1:9551")]
    pub ws_listen: SocketAddr,
    /// Explicitly permit unauthenticated non-loopback listeners.
    #[arg(long)]
    pub allow_remote: bool,
    /// Maximum simultaneous subscribers across all connections.
    #[arg(long, default_value_t = 64)]
    pub max_subscribers: u32,
    /// Maximum subscriptions per connection (default: 1 with --allow-remote,
    /// otherwise --max-subscribers).
    #[arg(long)]
    pub max_subscriptions_per_connection: Option<u32>,
    /// Maximum queued records per live subscriber.
    #[arg(long, default_value_t = 8)]
    pub subscriber_queue: usize,
    /// Maximum raw bytes retained in each live queue.
    #[arg(long, default_value_t = 2 * 1024 * 1024)]
    pub subscriber_queue_bytes: usize,
    /// Maximum deliverable archive record; larger records terminate with a cursor.
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    pub max_record_bytes: u32,
    /// Live send/acknowledgement deadline before demotion.
    #[arg(long, default_value_t = 5000)]
    pub live_timeout_ms: u64,
    /// Archive send/acknowledgement deadline before disconnect.
    #[arg(long, default_value_t = 10000)]
    pub catchup_timeout_ms: u64,
    /// Disconnect if archive lag increases over this catch-up progress window.
    #[arg(long, default_value_t = 30000)]
    pub catchup_progress_ms: u64,
}

impl Config {
    /// Resolve the exposure-aware default, preserving explicit operator overrides.
    pub fn subscription_limit_per_connection(&self) -> u32 {
        self.max_subscriptions_per_connection
            .unwrap_or(if self.allow_remote {
                1
            } else {
                self.max_subscribers
            })
    }

    /// Reject invalid bounds and accidental public exposure before binding.
    pub fn validate(&self, metrics_address: SocketAddr) -> Result<(), crate::Error> {
        if self.max_subscribers == 0
            || self.subscription_limit_per_connection() == 0
            || self.subscriber_queue == 0
            || self.subscriber_queue_bytes == 0
            || self.max_record_bytes == 0
            || self.max_record_bytes > (u32::MAX - 4096) / 2
            || self.live_timeout_ms == 0
            || self.catchup_timeout_ms == 0
            || self.catchup_progress_ms == 0
        {
            return Err(crate::invalid(
                "subscription bounds must be positive and fit a JSON frame",
            ));
        }
        if !self.ws_listen.ip().is_loopback() || !metrics_address.ip().is_loopback() {
            if !self.allow_remote {
                return Err(crate::invalid(
                    "non-loopback listeners require --allow-remote",
                ));
            }
            tracing::warn!("UNAUTHENTICATED experimental firehose listening beyond loopback");
        }
        Ok(())
    }
}

fn record_from_bytes(block: &EventIndexerPulledBlock, bytes: Vec<u8>) -> Record {
    Record {
        height: crate::block_height(block),
        consensus_hash: HexBytes::from(block.block.identity.consensus_hash.as_ref().to_vec()),
        byte_length: bytes.len(),
        record: HexBytes::from(bytes),
        ingested_at_unix_micros: None,
    }
}

/// Lifetime totals, also reported for each active subscriber.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    /// Number of transitions out of live mode due to backpressure.
    pub demotions: u64,
    /// Blocks handed to the bounded WebSocket transport (not acknowledgements).
    pub blocks_sent: u64,
    /// Raw archive bytes handed to the transport.
    pub bytes_sent: u64,
    /// Wall time reading/sending/waiting for acknowledgement in catch-up mode.
    pub catchup_seconds: f64,
    /// Subscription terminations by bounded reason vocabulary.
    pub disconnects: BTreeMap<String, u64>,
}

#[derive(Debug)]
struct Subscriber {
    awaiting_origin: bool,
    connection: ConnectionId,
    next: i64,
    pending: Option<i64>,
    queue: VecDeque<Arc<Record>>,
    queued_bytes: usize,
    catchup_since: Option<Instant>,
    progress: Option<(Instant, i64)>,
    notify: Arc<Notify>,
    counters: Counters,
}

#[derive(Debug)]
struct State {
    tip: i64,
    next_id: u64,
    subscribers: BTreeMap<u64, Subscriber>,
    totals: Counters,
}

struct Selected {
    next: i64,
    catchup: bool,
    record: Option<Arc<Record>>,
}

/// Read-side state; the ingester only performs bounded, non-awaiting publication.
#[derive(Clone, Debug)]
pub struct Hub {
    directory: PathBuf,
    config: Config,
    state: Arc<Mutex<State>>,
    connections: Arc<AtomicUsize>,
}

fn error(message: &str) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32009, message, None::<()>)
}

/// Prometheus suffix and its `Counters` field, exposed both as process totals
/// and per active subscriber.
const COUNTER_SERIES: [(&str, &str); 4] = [
    ("demotions_total", "demotions"),
    ("blocks_sent_total", "blocksSent"),
    ("bytes_sent_total", "bytesSent"),
    ("catchup_seconds_total", "catchupSeconds"),
];

impl Hub {
    /// Construct after archive recovery, before ingestion starts.
    pub fn new(directory: PathBuf, tip: i64, config: Config) -> Self {
        Self {
            directory,
            config,
            connections: Arc::new(AtomicUsize::new(0)),
            state: Arc::new(Mutex::new(State {
                tip,
                next_id: 1,
                subscribers: BTreeMap::new(),
                totals: Counters::default(),
            })),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("bug: fanout lock poisoned")
    }

    /// Publish only after successful durable append. No subscriber I/O occurs here.
    pub(crate) fn publish(&self, block: &EventIndexerPulledBlock, bytes: Vec<u8>) {
        let mut record = record_from_bytes(block, bytes);
        record.ingested_at_unix_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_micros()).ok());
        let record = Arc::new(record);
        let mut state = self.lock();
        state.tip = record.height;
        let mut demotions = 0;
        for sub in state.subscribers.values_mut() {
            if sub.awaiting_origin {
                sub.next = record.height;
                sub.awaiting_origin = false;
            }
            if record.height < sub.next {
                continue;
            }
            if sub.catchup_since.is_none() {
                if sub.queue.len() >= self.config.subscriber_queue
                    || record.byte_length
                        > self
                            .config
                            .subscriber_queue_bytes
                            .saturating_sub(sub.queued_bytes)
                {
                    demote(sub, record.height);
                    demotions += 1;
                } else {
                    sub.queued_bytes += record.byte_length;
                    sub.queue.push_back(record.clone());
                }
            }
            sub.notify.notify_waiters();
        }
        state.totals.demotions += demotions;
    }

    fn attach(
        &self,
        cursor: &Cursor,
        connection: ConnectionId,
    ) -> Result<(u64, Arc<Notify>), ErrorObjectOwned> {
        let mut state = self.lock();
        if state.subscribers.len() >= self.config.max_subscribers as usize {
            return Err(error("subscriber limit reached"));
        }
        let next = match cursor {
            Cursor::Tip => state.tip.saturating_add(1),
            Cursor::Height { height } if (0..i64::MAX).contains(height) => *height,
            Cursor::Height { .. } => return Err(error("invalid height")),
        };
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| error("subscriber IDs exhausted"))?;
        let notify = Arc::new(Notify::new());
        let catchup_since = (next <= state.tip).then(Instant::now);
        let progress = catchup_since.map(|start| (start, lag_blocks(state.tip, next)));
        let awaiting_origin = matches!(cursor, Cursor::Tip) && state.tip < 0;
        state.subscribers.insert(
            id,
            Subscriber {
                awaiting_origin,
                connection,
                next,
                pending: None,
                queue: VecDeque::new(),
                queued_bytes: 0,
                catchup_since,
                progress,
                notify: notify.clone(),
                counters: Counters::default(),
            },
        );
        Ok((id, notify))
    }

    fn ack(
        &self,
        id: u64,
        height: i64,
        connection: Option<&ConnectionId>,
    ) -> Result<(), ErrorObjectOwned> {
        let mut state = self.lock();
        let sub = state
            .subscribers
            .get_mut(&id)
            .ok_or_else(|| error("unknown subscriber"))?;
        if connection != Some(&sub.connection) || sub.pending != Some(height) {
            return Err(error(
                "acknowledgement must match this connection's outstanding height",
            ));
        }
        sub.pending = None;
        sub.next = height + 1;
        sub.notify.notify_waiters();
        Ok(())
    }

    /// Bounded active-subscriber snapshot plus process-lifetime aggregate totals.
    pub fn stats(&self) -> serde_json::Value {
        let state = self.lock();
        let mut totals = state.totals.clone();
        let subscribers: Vec<_> = state.subscribers.iter().map(|(id, sub)| {
            let mut counters = sub.counters.clone();
            if let Some(start) = sub.catchup_since {
                let elapsed = start.elapsed().as_secs_f64();
                counters.catchup_seconds += elapsed;
                totals.catchup_seconds += elapsed;
            }
            serde_json::json!({"id": id, "nextHeight": sub.next,
                "lagBlocks": lag_blocks(state.tip, sub.next),
                "mode": if sub.catchup_since.is_some() { "catchup" } else { "live" },
                "queuedBlocks": sub.queue.len(), "queuedBytes": sub.queued_bytes, "counters": counters})
        }).collect();
        serde_json::json!({"currentSubscribers": subscribers.len(), "tip": state.tip,
            "totals": totals, "subscribers": subscribers})
    }

    /// Prometheus exposition with active-only subscriber labels and retained totals.
    pub fn render_metrics(&self) -> String {
        use std::fmt::Write;
        let stats = self.stats();
        let mut output = format!("firehose_subscribers {}\n", stats["currentSubscribers"]);
        let _ = writeln!(output, "# TYPE firehose_connections gauge");
        let _ = writeln!(
            output,
            "firehose_connections {}",
            self.connections.load(Ordering::Relaxed)
        );
        let totals = &stats["totals"];
        for (name, field) in COUNTER_SERIES {
            let _ = writeln!(output, "firehose_subscription_{name} {}", totals[field]);
        }
        if let Some(reasons) = totals["disconnects"].as_object() {
            for (reason, count) in reasons {
                let _ = writeln!(
                    output,
                    "firehose_subscription_disconnects_total{{reason=\"{reason}\"}} {count}"
                );
            }
        }
        let mut lag = 0_i64;
        if let Some(subscribers) = stats["subscribers"].as_array() {
            for sub in subscribers {
                lag = lag.saturating_add(sub["lagBlocks"].as_i64().unwrap_or(0));
                let id = &sub["id"];
                let _ = writeln!(
                    output,
                    "firehose_subscriber_lag_blocks{{subscriber=\"{id}\"}} {}",
                    sub["lagBlocks"]
                );
                for (name, field) in COUNTER_SERIES {
                    let _ = writeln!(
                        output,
                        "firehose_subscriber_{name}{{subscriber=\"{id}\"}} {}",
                        sub["counters"][field]
                    );
                }
            }
        }
        let _ = writeln!(output, "firehose_subscription_lag_blocks {lag}");
        output
    }

    fn remove(&self, id: u64, reason: &str) -> i64 {
        let mut state = self.lock();
        if let Some(mut sub) = state.subscribers.remove(&id) {
            if let Some(start) = sub.catchup_since {
                let elapsed = start.elapsed().as_secs_f64();
                state.totals.catchup_seconds += elapsed;
                sub.counters.catchup_seconds += elapsed;
            }
            *sub.counters.disconnects.entry(reason.into()).or_default() += 1;
            *state.totals.disconnects.entry(reason.into()).or_default() += 1;
            tracing::info!(subscriber = id, reason, next_height = sub.next,
                counters = ?sub.counters, "subscription closed");
            sub.next
        } else {
            0
        }
    }

    /// Start the independent WebSocket server with bounded transport buffering.
    pub async fn serve(&self) -> Result<(SocketAddr, ServerHandle), Box<dyn std::error::Error>> {
        // A library caller may never have validated; the binary checks its own
        // metrics listener first, so only this listener is left to re-check.
        self.config.validate(self.config.ws_listen)?;
        let config = ServerConfig::builder()
            .ws_only()
            .enable_ws_ping(PingConfig::new())
            .max_connections(self.config.max_subscribers)
            .max_subscriptions_per_connection(self.config.subscription_limit_per_connection())
            .set_message_buffer_capacity(2)
            .max_request_body_size(4096)
            .max_response_body_size(self.config.max_record_bytes * 2 + 4096)
            .build();
        let connections = self.connections.clone();
        let middleware = RpcServiceBuilder::new().layer_fn(move |service| {
            connections.fetch_add(1, Ordering::Relaxed);
            ConnectionService {
                service,
                _guard: Arc::new(ConnectionGuard(connections.clone())),
            }
        });
        let server = ServerBuilder::default()
            .set_rpc_middleware(middleware)
            .set_config(config)
            .build(self.config.ws_listen)
            .await?;
        let address = server.local_addr()?;
        if self.config.allow_remote && self.config.max_subscriptions_per_connection.is_none() {
            tracing::info!(
                "--allow-remote defaults --max-subscriptions-per-connection to 1; set it explicitly to override"
            );
        }
        Ok((address, server.start(self.clone().into_rpc())))
    }

    fn select(&self, id: u64) -> Result<Option<Selected>, &'static str> {
        let mut state = self.lock();
        let tip = state.tip;
        let sub = state
            .subscribers
            .get_mut(&id)
            .expect("bug: active subscriber absent");
        if let Some((started, lag)) = sub.progress
            && started.elapsed() >= Duration::from_millis(self.config.catchup_progress_ms)
        {
            let current_lag = lag_blocks(tip, sub.next);
            if current_lag > lag {
                return Err("catchup_falling_behind");
            }
            sub.progress = Some((Instant::now(), current_lag));
        }
        if sub.next > tip {
            sub.progress = None;
            if let Some(start) = sub.catchup_since.take() {
                let elapsed = start.elapsed().as_secs_f64();
                sub.counters.catchup_seconds += elapsed;
                state.totals.catchup_seconds += elapsed;
            }
            Ok(None)
        } else {
            let record = sub.queue.pop_front();
            if let Some(record) = &record {
                sub.queued_bytes -= record.byte_length;
            }
            Ok(Some(Selected {
                next: sub.next,
                catchup: sub.catchup_since.is_some(),
                record,
            }))
        }
    }

    async fn deliver(&self, id: u64, notify: Arc<Notify>, sink: &SubscriptionSink) -> &'static str {
        loop {
            // Register before examining state; publication cannot fall between check and sleep.
            let changed = notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let selected = match self.select(id) {
                Ok(value) => value,
                Err(reason) => return reason,
            };
            let Some(Selected {
                next,
                catchup,
                record,
            }) = selected
            else {
                changed.await;
                continue;
            };
            let record = if catchup {
                match tokio::time::timeout(
                    Duration::from_millis(self.config.catchup_timeout_ms),
                    self.read(next),
                )
                .await
                {
                    Ok(Ok(record)) => record,
                    Ok(Err(reason)) => return reason,
                    Err(_) => return "archive_timeout",
                }
            } else if let Some(record) = record {
                record
            } else {
                changed.await;
                continue;
            };
            if record.height != next {
                return "archive_gap";
            }
            if record.height == i64::MAX {
                return "height_exhausted";
            }
            if record.byte_length > self.config.max_record_bytes as usize {
                return "record_too_large";
            }
            self.lock()
                .subscribers
                .get_mut(&id)
                .expect("bug: subscriber absent")
                .pending = Some(next);
            let Ok(message) = SubscriptionMessage::new(
                sink.method_name(),
                sink.subscription_id(),
                &crate::types::Notification::Block {
                    subscriber_id: id,
                    block: record.as_ref(),
                },
            ) else {
                return "encoding_error";
            };
            let send = self.send_ack(id, &notify, sink, message, record.byte_length);
            tokio::pin!(send);
            let timeout = if catchup {
                self.config.catchup_timeout_ms
            } else {
                self.config.live_timeout_ms
            };
            match tokio::time::timeout(Duration::from_millis(timeout), &mut send).await {
                Ok(true) => continue,
                Ok(false) => return "client_closed",
                Err(_) if catchup => return "catchup_timeout",
                Err(_) => {
                    let mut state = self.lock();
                    let tip = state.tip;
                    let sub = state
                        .subscribers
                        .get_mut(&id)
                        .expect("bug: subscriber absent");
                    if sub.catchup_since.is_none() {
                        demote(sub, tip);
                        state.totals.demotions += 1;
                    }
                }
            }
            // Preserve the in-flight send/ACK future: no duplicate send on demotion.
            match tokio::time::timeout(
                Duration::from_millis(self.config.catchup_timeout_ms),
                &mut send,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => return "client_closed",
                Err(_) => return "catchup_timeout",
            }
        }
    }

    async fn send_ack(
        &self,
        id: u64,
        notify: &Notify,
        sink: &SubscriptionSink,
        message: SubscriptionMessage,
        byte_length: usize,
    ) -> bool {
        if sink.send(message).await.is_err() {
            return false;
        }
        {
            let mut state = self.lock();
            let sub = state
                .subscribers
                .get_mut(&id)
                .expect("bug: subscriber absent");
            sub.counters.blocks_sent += 1;
            sub.counters.bytes_sent += byte_length as u64;
            state.totals.blocks_sent += 1;
            state.totals.bytes_sent += byte_length as u64;
        }
        loop {
            let changed = notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self
                .lock()
                .subscribers
                .get(&id)
                .is_none_or(|sub| sub.pending.is_none())
            {
                return true;
            }
            changed.await;
        }
    }

    async fn read(&self, height: i64) -> Result<Arc<Record>, &'static str> {
        use tokio::io::AsyncReadExt;
        let path = crate::store::record_path(&self.directory, height);
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|_| "archive_error")?;
        let mut bytes = Vec::new();
        file.take(u64::from(self.config.max_record_bytes) + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "archive_error")?;
        if bytes.len() > self.config.max_record_bytes as usize {
            return Err("record_too_large");
        }
        let block = xdrs::txs::decode(&bytes).map_err(|_| "archive_corrupt")?;
        crate::verify::verify(&block).map_err(|_| "archive_corrupt")?;
        Ok(Arc::new(record_from_bytes(&block, bytes)))
    }
}

fn lag_blocks(tip: i64, next: i64) -> i64 {
    tip.saturating_sub(next).saturating_add(1).max(0)
}

fn demote(sub: &mut Subscriber, tip: i64) {
    sub.queue.clear();
    sub.queued_bytes = 0;
    sub.catchup_since = Some(Instant::now());
    sub.progress = Some((Instant::now(), lag_blocks(tip, sub.next)));
    sub.counters.demotions += 1;
}

#[jsonrpsee::core::async_trait]
impl FirehoseApiServer for Hub {
    fn statistics(&self) -> jsonrpsee::core::RpcResult<serde_json::Value> {
        Ok(self.stats())
    }

    fn acknowledge(
        &self,
        extensions: &jsonrpsee::Extensions,
        subscriber_id: u64,
        height: i64,
    ) -> jsonrpsee::core::RpcResult<()> {
        self.ack(subscriber_id, height, extensions.get::<ConnectionId>())
    }

    async fn follow_blocks(&self, pending: jsonrpsee::PendingSubscriptionSink, cursor: Cursor) {
        let (id, notify) = match self.attach(&cursor, pending.connection_id()) {
            Ok(value) => value,
            Err(err) => {
                pending.reject(err).await;
                return;
            }
        };
        let guard = Cleanup {
            hub: self.clone(),
            id,
        };
        if let Ok(sink) = pending.accept().await {
            let reason = tokio::select! {
                reason = self.deliver(id, notify, &sink) => reason,
                () = sink.closed() => "client_closed",
            };
            let next_height = self.remove(id, reason);
            let close = crate::types::Notification::<Record>::Closed {
                reason: reason.into(),
                next_height,
            };
            if let Ok(message) =
                SubscriptionMessage::new(sink.method_name(), sink.subscription_id(), &close)
            {
                let _ = sink.send_timeout(message, Duration::from_millis(250)).await;
            }
        }
        drop(guard);
    }
}

/// Deregisters on cancellation or panic. A close that ran to completion has
/// already removed the subscriber with its real reason, and `remove` tolerates
/// the absent entry.
struct Cleanup {
    hub: Hub,
    id: u64,
}

/// One guard per WebSocket session, shared by its in-flight RPC calls.
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct ConnectionService<S> {
    service: S,
    _guard: Arc<ConnectionGuard>,
}

impl<S: RpcServiceT> RpcServiceT for ConnectionService<S> {
    type MethodResponse = S::MethodResponse;
    type BatchResponse = S::BatchResponse;
    type NotificationResponse = S::NotificationResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        self.service.call(request)
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        self.service.batch(batch)
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.service.notification(notification)
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.hub.remove(self.id, "client_closed");
    }
}
