//! Per-call public admission. Guards release capacity on completion or cancellation.
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::http::Extensions;
use jsonrpsee::{
    core::{
        middleware::{Batch, Notification, RpcServiceT},
        server::{ConnectionId, MethodResponse},
    },
    types::{ErrorObjectOwned, Request},
};
use thruput_json_rpc_server::RpcServerError;
use thruput_json_rpc_types::error::native::{ErrorData, codes};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

use crate::{
    config::ClientAdmissionConfig,
    metrics::{GatewayMetrics, admission::GaugeGuard},
    middleware::ClientKey,
};

const SHARDS: usize = 64;

fn shard(key: &impl Hash) -> usize {
    let mut hash = DefaultHasher::new();
    key.hash(&mut hash);
    usize::try_from(hash.finish() % SHARDS as u64).expect("bug: shard index fits usize")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // No user code runs while these locks are held. Recovering poison keeps
    // cancellation cleanup usable even if an unrelated task panics.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Bucket {
    // Integer token-nanoseconds avoid floating point rounding in retry delays.
    credit: u128,
    touched: Instant,
}

impl Bucket {
    fn new(burst: usize, now: Instant) -> Self {
        Self {
            credit: burst as u128 * 1_000_000_000,
            touched: now,
        }
    }

    fn take(
        &mut self,
        cost: usize,
        config: &ClientAdmissionConfig,
        now: Instant,
    ) -> Result<(), u64> {
        // Concurrent callers can sample the clock before acquiring a shard in
        // the opposite order. Never move a bucket's refill clock backwards.
        let now = now.max(self.touched);
        let rate = config.client_rate_per_sec.get() as u128;
        self.credit = (self.credit + now.duration_since(self.touched).as_nanos() * rate)
            .min(config.client_burst.get() as u128 * 1_000_000_000);
        self.touched = now;
        let cost = cost as u128 * 1_000_000_000;
        if self.credit >= cost {
            self.credit -= cost;
            Ok(())
        } else {
            let millis = (cost - self.credit).div_ceil(rate * 1_000_000);
            Err(u64::try_from(millis).unwrap_or(u64::MAX))
        }
    }
}

struct ClientShard {
    buckets: HashMap<ClientKey, Bucket>,
    last_sweep: Instant,
}

pub(crate) struct Admission {
    config: ClientAdmissionConfig,
    global: Arc<Semaphore>,
    connections: [Mutex<HashMap<ConnectionId, u32>>; SHARDS],
    clients: [Mutex<ClientShard>; SHARDS],
    tracked: AtomicUsize,
    overflow: Mutex<Bucket>,
    metrics: GatewayMetrics,
    simulation_batch_calls: usize,
}

impl Admission {
    #[cfg(test)]
    pub(crate) fn connection_entries(&self) -> usize {
        self.connections.iter().map(|shard| lock(shard).len()).sum()
    }

    pub(crate) fn new(
        config: ClientAdmissionConfig,
        simulation_batch_calls: u32,
        metrics: GatewayMetrics,
    ) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            global: Arc::new(Semaphore::new(config.max_in_flight_calls.get())),
            connections: std::array::from_fn(|_| Mutex::new(HashMap::new())),
            clients: std::array::from_fn(|_| {
                Mutex::new(ClientShard {
                    buckets: HashMap::new(),
                    last_sweep: now,
                })
            }),
            tracked: AtomicUsize::new(0),
            overflow: Mutex::new(Bucket::new(config.client_burst.get(), Instant::now())),
            config,
            metrics,
            simulation_batch_calls: simulation_batch_calls as usize,
        })
    }

    fn charge(&self, key: Option<ClientKey>, cost: usize, now: Instant) -> Result<(), u64> {
        if let Some(key) = key {
            let mut clients = lock(&self.clients[shard(&key)]);
            if let Some(bucket) = clients.buckets.get_mut(&key) {
                return bucket.take(cost, &self.config, now);
            }
            // Rotating client identities must not trigger a full shard scan on
            // every insert. The clock is sampled by the caller before locking.
            if now.saturating_duration_since(clients.last_sweep) >= Duration::from_secs(1) {
                let idle = Duration::from_mins(1)
                    + Duration::from_nanos(
                        u64::try_from(
                            (self.config.client_burst.get() as u128 * 1_000_000_000)
                                .div_ceil(self.config.client_rate_per_sec.get() as u128),
                        )
                        .expect("bug: validated bucket duration fits u64 nanoseconds"),
                    );
                let before = clients.buckets.len();
                clients
                    .buckets
                    .retain(|_, bucket| now.saturating_duration_since(bucket.touched) <= idle);
                clients.last_sweep = now;
                let removed = before - clients.buckets.len();
                self.tracked.fetch_sub(removed, Ordering::Relaxed);
                self.metrics.admission().tracked_clients.sub(f64::from(
                    u32::try_from(removed).expect("bug: tracked client bound fits u32"),
                ));
            }
            // Reserve a slot atomically across shards before inserting.
            if self
                .tracked
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    (count < self.config.max_tracked_clients.get()).then_some(count + 1)
                })
                .is_ok()
            {
                self.metrics.admission().tracked_clients.inc();
                return clients
                    .buckets
                    .entry(key)
                    .or_insert_with(|| Bucket::new(self.config.client_burst.get(), now))
                    .take(cost, &self.config, now);
            }
        }
        lock(&self.overflow).take(cost, &self.config, now)
    }

    fn acquire(
        self: &Arc<Self>,
        method: &str,
        extensions: &Extensions,
    ) -> Result<CallGuard, Rejection> {
        let permit = self.global.clone().try_acquire_owned().map_err(|_| {
            self.metrics.admission().reject("global");
            Rejection::Overloaded
        })?;
        let mut guard = CallGuard {
            admission: self.clone(),
            connection: None,
            _permit: permit,
            _metric: GaugeGuard::new(&self.metrics.admission().in_flight),
        };
        if let Some(&connection) = extensions.get::<ConnectionId>() {
            let mut connections = lock(&self.connections[shard(&connection)]);
            let count = connections.entry(connection).or_default();
            if *count as usize >= self.config.max_in_flight_per_connection.get() {
                self.metrics.admission().reject("connection");
                return Err(Rejection::Overloaded);
            }
            *count += 1;
            guard.connection = Some(connection);
        }
        // A bucket never holds more than the burst, so a larger cost could never
        // be paid; charge the whole burst instead.
        let cost = explicit_cost(method, self.simulation_batch_calls)
            .unwrap_or(1)
            .min(self.config.client_burst.get());
        self.charge(extensions.get::<ClientKey>().copied(), cost, Instant::now())
            .map_err(|retry| {
                self.metrics.admission().reject("rate_limited");
                Rejection::RateLimited(retry)
            })?;
        Ok(guard)
    }
}

struct CallGuard {
    admission: Arc<Admission>,
    connection: Option<ConnectionId>,
    _permit: OwnedSemaphorePermit,
    _metric: GaugeGuard,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if let Some(connection) = self.connection {
            let mut connections = lock(&self.admission.connections[shard(&connection)]);
            if let Some(count) = connections.get_mut(&connection) {
                *count -= 1;
                if *count == 0 {
                    connections.remove(&connection);
                }
            }
        }
    }
}

enum Rejection {
    Overloaded,
    RateLimited(u64),
}

impl Rejection {
    fn error(self) -> ErrorObjectOwned {
        match self {
            Self::Overloaded => ErrorObjectOwned::owned(
                codes::OVERLOADED,
                RpcServerError::Overloaded("gateway admission".into()).to_string(),
                Some(ErrorData::Overloaded {
                    admission: None,
                    transaction_hash: None,
                    retry_after_ms: None,
                    error: None,
                }),
            ),
            Self::RateLimited(retry) => ErrorObjectOwned::owned(
                codes::RATE_LIMITED,
                "Rate limited",
                Some(ErrorData::RateLimited {
                    retry_after_ms: Some(retry),
                }),
            ),
        }
    }
}

// Names come from native_methods!, which NativeModule uses for registration.
fn explicit_cost(method: &str, simulation_batch_calls: usize) -> Option<usize> {
    Some(match method {
        "getVersionInfo" | "getNetworkInfo" | "getServerLimits" => 0,
        "getChainTip"
        | "getChainConfig"
        | "getStorage"
        | "getBalance"
        | "getRegisteredAuth"
        | "getBlockByHeight"
        | "getTransactionReceipt"
        | "getTransactionStatus"
        | "dev_fundAccount"
        | "dev_getPendingTransactionCount" => 1,
        "sendTransaction" => 2,
        "getEvents" | "getTransactionsByRootAddress" | "getTransactionsByBlock" => 5,
        "simulateInvocation" => 10,
        // Conservative charge avoids decoding arbitrary simulation calldata here.
        "simulateInvocations" => simulation_batch_calls.saturating_mul(10),
        "faucet_requestAirdrop" => 20,
        _ => return None,
    })
}

#[derive(Clone)]
pub(crate) struct RpcAdmissionService<S> {
    inner: S,
    admission: Arc<Admission>,
}

impl<S> RpcAdmissionService<S> {
    pub(crate) fn new(inner: S, admission: Arc<Admission>) -> Self {
        Self { inner, admission }
    }
}

impl<S> RpcServiceT for RpcAdmissionService<S>
where
    S: RpcServiceT<
            MethodResponse = MethodResponse,
            BatchResponse = MethodResponse,
            NotificationResponse = MethodResponse,
        > + Clone
        + Send
        + Sync
        + 'static,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = MethodResponse;
    type NotificationResponse = MethodResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let this = self.clone();
        async move {
            let _guard = match this.admission.acquire(req.method_name(), req.extensions()) {
                Ok(guard) => guard,
                Err(rejection) => {
                    return MethodResponse::error(req.id().into_owned(), rejection.error());
                }
            };
            this.inner.call(req).await
        }
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        let this = self.clone();
        async move {
            let Ok(_guard) = this
                .admission
                .acquire(notification.method_name(), notification.extensions())
            else {
                return MethodResponse::notification();
            };
            this.inner.notification(notification).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpsee::{core::server::ResponsePayload, types::Id};
    use std::{future, net::IpAddr};

    fn admission(settings: &str) -> Arc<Admission> {
        Admission::new(
            toml::from_str(settings).unwrap(),
            16,
            GatewayMetrics::for_tests(),
        )
    }

    fn key(n: u8) -> ClientKey {
        IpAddr::from([192, 0, 2, n]).into()
    }

    #[test]
    fn all_registered_methods_have_explicit_costs() {
        // Use the exact macro expanded by NativeModule::new, not a copied list.
        macro_rules! check {
            ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
                $(assert!(explicit_cost($wire, 16).is_some(), "missing cost: {}", $wire);)*
            };
        }
        thruput_json_rpc_api::native_methods!(check);
        assert_eq!(explicit_cost("simulateInvocations", 16), Some(160));
        assert_eq!(explicit_cost("unknown", 16), None);
    }

    #[tokio::test(start_paused = true)]
    async fn token_budget_refill_retry_and_zero_cost() {
        let admission = admission("client_rate_per_sec = 2\nclient_burst = 4");
        let now = Instant::now();
        assert_eq!(admission.charge(Some(key(1)), 4, now), Ok(()));
        assert_eq!(admission.charge(Some(key(1)), 1, now), Err(500));
        assert_eq!(admission.charge(Some(key(1)), 0, now), Ok(()));
        assert_eq!(admission.charge(Some(key(2)), 4, now), Ok(()));
        tokio::time::advance(Duration::from_millis(125)).await;
        assert_eq!(admission.charge(Some(key(1)), 1, Instant::now()), Err(375));
        tokio::time::advance(Duration::from_millis(375)).await;
        assert_eq!(admission.charge(Some(key(1)), 1, Instant::now()), Ok(()));
        assert_eq!(admission.charge(Some(key(1)), 1, Instant::now()), Err(500));
        tokio::time::advance(Duration::from_secs(100)).await;
        assert_eq!(admission.charge(Some(key(1)), 4, Instant::now()), Ok(()));
        assert_eq!(admission.charge(Some(key(1)), 1, Instant::now()), Err(500));
    }

    #[tokio::test(start_paused = true)]
    async fn cost_above_burst_charges_the_burst_and_can_succeed() {
        // view_batch max_calls 32 makes simulateInvocations cost 320 > burst 200.
        let admission = Admission::new(
            toml::from_str("client_rate_per_sec = 50\nclient_burst = 200").unwrap(),
            32,
            GatewayMetrics::for_tests(),
        );
        let mut extensions = Extensions::new();
        extensions.insert(key(9));
        assert!(
            admission
                .acquire("simulateInvocations", &extensions)
                .is_ok()
        );
        let Err(Rejection::RateLimited(retry)) =
            admission.acquire("simulateInvocations", &extensions)
        else {
            panic!("second batch should be rate limited");
        };
        assert_eq!(retry, 4000);
        tokio::time::advance(Duration::from_millis(retry)).await;
        assert!(
            admission
                .acquire("simulateInvocations", &extensions)
                .is_ok()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn overflow_is_shared_and_idle_clients_are_evicted() {
        let admission =
            admission("client_rate_per_sec = 1\nclient_burst = 1\nmax_tracked_clients = 1");
        let now = Instant::now();
        assert_eq!(admission.charge(Some(key(1)), 1, now), Ok(()));
        assert_eq!(admission.charge(Some(key(2)), 1, now), Ok(()));
        assert_eq!(admission.charge(Some(key(3)), 1, now), Err(1000));
        assert_eq!(admission.charge(None, 1, now), Err(1000));
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 1);
        let colliding = (2..=255)
            .map(key)
            .find(|candidate| shard(candidate) == shard(&key(1)))
            .unwrap();
        tokio::time::advance(Duration::from_secs(62)).await;
        assert_eq!(admission.charge(Some(colliding), 1, Instant::now()), Ok(()));
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 1);
        assert!(
            !lock(&admission.clients[shard(&key(1))])
                .buckets
                .contains_key(&key(1))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_sweep_runs_at_most_once_per_second_per_shard() {
        let admission =
            admission("client_rate_per_sec = 1\nclient_burst = 1\nmax_tracked_clients = 3");
        let keys: Vec<_> = (0..10_000_u32)
            .map(|address| ClientKey::from(IpAddr::V4(address.into())))
            .filter(|candidate| shard(candidate) == shard(&key(1)))
            .take(4)
            .collect();
        assert_eq!(keys.len(), 4);
        let shard_index = shard(&keys[0]);
        assert_eq!(admission.charge(Some(keys[0]), 1, Instant::now()), Ok(()));

        // At exactly the idle threshold the first entry survives this sweep.
        tokio::time::advance(Duration::from_secs(61)).await;
        let swept_at = Instant::now();
        assert_eq!(admission.charge(Some(keys[1]), 1, swept_at), Ok(()));
        assert_eq!(lock(&admission.clients[shard_index]).last_sweep, swept_at);
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 2);
        assert_eq!(
            admission
                .metrics
                .admission()
                .tracked_clients
                .get()
                .to_bits(),
            2.0_f64.to_bits()
        );

        // This new key must not scan, even though the first entry is now idle.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(admission.charge(Some(keys[2]), 1, Instant::now()), Ok(()));
        {
            let shard = lock(&admission.clients[shard_index]);
            assert_eq!(shard.last_sweep, swept_at);
            assert!(shard.buckets.contains_key(&keys[0]));
            assert_eq!(shard.buckets.len(), 3);
        }
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 3);
        assert_eq!(
            admission
                .metrics
                .admission()
                .tracked_clients
                .get()
                .to_bits(),
            3.0_f64.to_bits()
        );

        // After one full second, eviction frees a slot instead of overflowing.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(admission.charge(Some(keys[3]), 1, Instant::now()), Ok(()));
        {
            let shard = lock(&admission.clients[shard_index]);
            assert_eq!(shard.last_sweep, Instant::now());
            assert!(!shard.buckets.contains_key(&keys[0]));
            assert!(shard.buckets.contains_key(&keys[3]));
            assert_eq!(shard.buckets.len(), 3);
        }
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 3);
        assert_eq!(
            admission
                .metrics
                .admission()
                .tracked_clients
                .get()
                .to_bits(),
            3.0_f64.to_bits()
        );
    }

    #[test]
    fn admission_rejections_match_native_wire_and_reader_contracts() {
        use thruput_json_rpc_types::error::native::DecodedError;
        let overloaded = serde_json::to_value(Rejection::Overloaded.error()).unwrap();
        assert_eq!(overloaded["code"], codes::OVERLOADED);
        assert_eq!(
            overloaded["message"],
            RpcServerError::Overloaded("gateway admission".into()).to_string()
        );
        assert_eq!(
            overloaded["data"],
            serde_json::json!({"kind": "overloaded"})
        );
        assert!(
            matches!(DecodedError::decode("getChainTip", overloaded), DecodedError::Known { data, .. } if matches!(*data, ErrorData::Overloaded { admission: None, transaction_hash: None, retry_after_ms: None, error: None }))
        );

        let limited = serde_json::to_value(Rejection::RateLimited(123).error()).unwrap();
        assert_eq!(limited["code"], codes::RATE_LIMITED);
        assert_eq!(
            limited["data"],
            serde_json::json!({"kind": "rateLimited", "retryAfterMs": "123"})
        );
        assert!(
            matches!(DecodedError::decode("getChainTip", limited), DecodedError::Known { data, .. } if matches!(*data, ErrorData::RateLimited { retry_after_ms: Some(123) }))
        );
    }

    #[derive(Clone)]
    struct HeldService(Arc<Semaphore>, Arc<AtomicUsize>);

    impl RpcServiceT for HeldService {
        type MethodResponse = MethodResponse;
        type BatchResponse = MethodResponse;
        type NotificationResponse = MethodResponse;

        fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            let gate = self.0.clone();
            let calls = self.1.clone();
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                let _gate = gate.acquire().await.unwrap();
                MethodResponse::response(
                    req.id().into_owned(),
                    ResponsePayload::success(true),
                    4096,
                )
            }
        }
        fn batch<'a>(&self, _: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            future::ready(MethodResponse::notification())
        }
        fn notification<'a>(
            &self,
            _: Notification<'a>,
        ) -> impl Future<Output = MethodResponse> + Send + 'a {
            let gate = self.0.clone();
            let calls = self.1.clone();
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                let _gate = gate.acquire().await.unwrap();
                MethodResponse::notification()
            }
        }
    }

    async fn poll_pending<F: Future>(future: std::pin::Pin<&mut F>) {
        let mut future = future;
        future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn permits_and_connection_counts_release_on_drop_and_completion() {
        let admission = admission("max_in_flight_calls = 1");
        let gate = Arc::new(Semaphore::new(0));
        let service =
            RpcAdmissionService::new(HeldService(gate.clone(), Arc::default()), admission.clone());
        let request = || {
            let mut req = Request::borrowed("getVersionInfo", None, Id::Number(1));
            req.extensions_mut().insert(ConnectionId(123));
            req
        };
        let mut held = Box::pin(service.call(request()));
        poll_pending(held.as_mut()).await;
        assert_eq!(
            service.call(request()).await.as_error_code(),
            Some(codes::OVERLOADED)
        );
        assert_eq!(admission.global.available_permits(), 0);
        drop(held);
        assert_eq!(admission.global.available_permits(), 1);
        assert!(lock(&admission.connections[shard(&ConnectionId(123))]).is_empty());
        gate.add_permits(1);
        assert!(service.call(request()).await.as_error_code().is_none());
        assert_eq!(admission.global.available_permits(), 1);
        assert!(lock(&admission.connections[shard(&ConnectionId(123))]).is_empty());
    }

    #[tokio::test]
    async fn connection_then_rate_checks_release_global_capacity() {
        let admission = admission(
            "max_in_flight_per_connection = 1\nclient_rate_per_sec = 1\nclient_burst = 1",
        );
        let mut extensions = Extensions::new();
        extensions.insert(ConnectionId(1));
        let guard = admission.acquire("getChainTip", &extensions).ok().unwrap();
        assert!(matches!(
            admission.acquire("getVersionInfo", &extensions),
            Err(Rejection::Overloaded)
        ));
        drop(guard);
        assert!(matches!(
            admission.acquire("getChainTip", &extensions),
            Err(Rejection::RateLimited(_))
        ));
        assert!(admission.acquire("getVersionInfo", &extensions).is_ok());
        assert_eq!(admission.global.available_permits(), 4096);
        assert!(lock(&admission.connections[shard(&ConnectionId(1))]).is_empty());
    }

    #[tokio::test]
    async fn rejected_notifications_are_silent_and_invoke_no_handler() {
        let admission = admission(
            "client_rate_per_sec = 1\nclient_burst = 1\nmax_in_flight_calls = 1\nmax_in_flight_per_connection = 1",
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let service = RpcAdmissionService::new(
            HeldService(Arc::new(Semaphore::new(1)), calls.clone()),
            admission.clone(),
        );
        let notification = || {
            serde_json::from_str::<Notification<'_>>(r#"{"jsonrpc":"2.0","method":"getChainTip"}"#)
                .unwrap()
        };
        service.notification(notification()).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let response = service.notification(notification()).await;
        assert!(response.is_notification());
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // A cost-zero method still checks the global and connection caps.
        let guard = admission
            .acquire("getVersionInfo", &Extensions::new())
            .ok()
            .unwrap();
        let response = service.notification(notification()).await;
        assert!(response.is_notification());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        drop(guard);

        let snapshot = crate::health::GatewayHealthSnapshot::for_test(
            crate::health::HealthStatus::Starting,
            None,
            &[],
        );
        let metrics = admission.metrics.gather(&snapshot).unwrap();
        assert!(
            metrics.contains("groundhog_gateway_admission_rejected_total{reason=\"global\"} 1")
        );
        assert!(
            metrics
                .contains("groundhog_gateway_admission_rejected_total{reason=\"rate_limited\"} 1")
        );
        assert!(metrics.contains("groundhog_gateway_admission_in_flight 0"));
    }

    #[test]
    fn concurrent_inserts_respect_the_total_tracking_cap() {
        let admission = admission("max_tracked_clients = 8");
        let barrier = std::sync::Barrier::new(SHARDS);
        std::thread::scope(|scope| {
            for client in 0..SHARDS {
                let admission = &admission;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    assert_eq!(
                        admission.charge(
                            Some(key(u8::try_from(client).unwrap())),
                            0,
                            Instant::now()
                        ),
                        Ok(())
                    );
                });
            }
        });
        assert_eq!(admission.tracked.load(Ordering::Relaxed), 8);
        assert_eq!(
            admission
                .clients
                .iter()
                .map(|shard| lock(shard).buckets.len())
                .sum::<usize>(),
            8
        );
        assert_eq!(
            admission
                .metrics
                .admission()
                .tracked_clients
                .get()
                .to_bits(),
            8.0_f64.to_bits()
        );
    }

    #[tokio::test]
    async fn panicking_call_releases_guards() {
        let admission = admission("max_in_flight_calls = 1");
        let state = admission.clone();
        let task = tokio::spawn(async move {
            let mut extensions = Extensions::new();
            extensions.insert(ConnectionId(9));
            let _guard = state.acquire("getVersionInfo", &extensions).ok().unwrap();
            panic!("handler panic while admitted");
        });
        assert!(task.await.unwrap_err().is_panic());
        assert_eq!(admission.global.available_permits(), 1);
        assert_eq!(admission.connection_entries(), 0);
        assert_eq!(
            admission.metrics.admission().in_flight.get().to_bits(),
            0.0_f64.to_bits()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_lock_acquisition_never_moves_refill_clock_backwards() {
        let admission = admission("client_rate_per_sec = 2\nclient_burst = 2");
        let first = Instant::now();
        assert_eq!(admission.charge(Some(key(1)), 2, first), Ok(()));
        tokio::time::advance(Duration::from_millis(500)).await;
        let later = Instant::now();
        assert_eq!(admission.charge(Some(key(1)), 1, later), Ok(()));
        assert_eq!(admission.charge(Some(key(1)), 1, first), Err(500));
        assert_eq!(admission.charge(Some(key(1)), 1, later), Err(500));
    }
}
