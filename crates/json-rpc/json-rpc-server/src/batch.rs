//! Ordered, bounded dispatch for JSON-RPC arrays.

use std::{collections::BTreeMap, num::NonZeroUsize, str::FromStr};

use futures_util::{StreamExt, stream::FuturesUnordered};
use jsonrpsee::{
    core::{
        middleware::{Batch, BatchEntry, Notification, RpcServiceT},
        server::{BatchResponseBuilder, MethodResponse},
    },
    types::{Id, Request, error::reject_too_big_batch_response},
};

/// Per-array limit on active and buffered entries (1..=1024, default 128).
/// Independent of backend submit batching and array-length limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "usize")]
pub struct RpcBatchConcurrency(NonZeroUsize);

impl RpcBatchConcurrency {
    /// Return the validated entry limit.
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for RpcBatchConcurrency {
    fn default() -> Self {
        Self(NonZeroUsize::new(128).expect("bug: default concurrency is positive"))
    }
}

impl TryFrom<usize> for RpcBatchConcurrency {
    type Error = InvalidRpcBatchConcurrency;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match NonZeroUsize::new(value) {
            Some(value) if value.get() <= 1024 => Ok(Self(value)),
            _ => Err(InvalidRpcBatchConcurrency),
        }
    }
}

impl FromStr for RpcBatchConcurrency {
    type Err = InvalidRpcBatchConcurrency;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<usize>()
            .map_err(|_| InvalidRpcBatchConcurrency)?
            .try_into()
    }
}

/// An RPC array concurrency setting outside the supported resource bound.
#[derive(Debug, thiserror::Error)]
#[error("RPC batch concurrency must be in 1..=1024")]
pub struct InvalidRpcBatchConcurrency;

/// Bounded concurrent dispatch for every nonempty JSON-RPC array.
///
/// Execution order is unspecified; responses and their extensions retain request order.
/// Notifications delegate to the inner notification service and produce no array entry.
/// Empty arrays and single calls retain the inner service's behavior.
/// Completed responses share the array's byte budget, even while awaiting earlier entries.
/// Overflow cancels outstanding dispatch without waiting for the head of the array.
/// Cancellation does not roll back side effects already accepted by a backend.
#[derive(Clone, Debug)]
pub struct ConcurrentBatchService<S> {
    inner: S,
    max_response_body_size: u32,
    concurrency: RpcBatchConcurrency,
}

impl<S> ConcurrentBatchService<S> {
    /// Wrap a service using the same response limit as its jsonrpsee `ServerConfig`.
    pub fn new(inner: S, max_response_body_size: u32, concurrency: RpcBatchConcurrency) -> Self {
        Self {
            inner,
            max_response_body_size,
            concurrency,
        }
    }
}

impl<S> RpcServiceT for ConcurrentBatchService<S>
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
        self.inner.call(req)
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = MethodResponse> + Send + 'a {
        self.inner.notification(notification)
    }

    fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        let inner = self.inner.clone();
        let max_response_body_size = self.max_response_body_size as usize;
        let concurrency = self.concurrency.get();
        async move {
            if batch.is_empty() {
                return inner.batch(batch).await;
            }

            // Keep futures lazy and bounded, including completed responses blocked by
            // an earlier entry. No detached work survives dropping this batch future.
            let mut entries = batch.into_iter().enumerate();
            let mut responses = FuturesUnordered::new();
            let mut completed = BTreeMap::new();
            let mut next_output = 0;
            // Match BatchResponseBuilder: '[' plus each JSON response and ',' (or final ']').
            // Never release bytes on ordered emission: the builder still retains them.
            let mut response_bytes = 1usize;
            let mut builder = BatchResponseBuilder::new_with_limit(max_response_body_size);
            loop {
                for (index, entry) in entries
                    .by_ref()
                    .take(concurrency.saturating_sub(responses.len() + completed.len()))
                {
                    let inner = &inner;
                    responses.push(async move {
                        let response = match entry {
                            Ok(BatchEntry::Call(req)) => Some(inner.call(req).await),
                            Ok(BatchEntry::Notification(notification)) => {
                                inner.notification(notification).await;
                                None
                            }
                            Err(err) => {
                                let (err, id) = err.into_parts();
                                Some(MethodResponse::error(id, err))
                            }
                        };
                        (index, response)
                    });
                }
                let Some((index, response)) = responses.next().await else {
                    break;
                };
                if let Some(response) = &response {
                    response_bytes = response_bytes
                        .saturating_add(response.as_json().get().len())
                        .saturating_add(1);
                    if response_bytes > max_response_body_size {
                        return MethodResponse::error(
                            Id::Null,
                            reject_too_big_batch_response(max_response_body_size),
                        );
                    }
                }
                completed.insert(index, response);
                while let Some(response) = completed.remove(&next_output) {
                    next_output += 1;
                    if let Some(response) = response {
                        // Append in request order to preserve JSON and extension precedence.
                        if let Err(err) = builder.append(response) {
                            return err;
                        }
                    }
                }
            }
            if builder.is_empty() {
                MethodResponse::notification()
            } else {
                MethodResponse::from_batch(builder.finish())
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use jsonrpsee::{
        core::{middleware::BatchEntryErr, server::ResponsePayload},
        types::{ErrorObject, Id, error::ErrorCode},
    };
    use serde_json::{Value, json};
    use tokio::sync::Semaphore;

    use super::*;

    const SEND: &str = "groundhog_sendRawTransaction";
    const RECEIPT: &str = "groundhog_getTransactionReceipt";
    const LIMIT: u32 = 4096;
    const BATCH_CONCURRENCY: usize = 4;

    #[derive(Default)]
    struct State {
        started: AtomicUsize,
        active: AtomicUsize,
        peak: AtomicUsize,
        completed: Mutex<Vec<u64>>,
        batches: AtomicUsize,
        notifications: AtomicUsize,
    }

    #[derive(Clone)]
    struct Probe {
        state: Arc<State>,
        gates: Arc<Vec<Semaphore>>,
        limit: u32,
        payload_bytes: usize,
    }

    impl Probe {
        fn new(count: usize, limit: u32) -> Self {
            Self {
                state: Arc::default(),
                gates: Arc::new((0..count).map(|_| Semaphore::new(0)).collect()),
                limit,
                payload_bytes: 0,
            }
        }
    }

    struct Active(Arc<State>);

    impl Drop for Active {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl RpcServiceT for Probe {
        type MethodResponse = MethodResponse;
        type BatchResponse = MethodResponse;
        type NotificationResponse = MethodResponse;

        fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            let this = self.clone();
            async move {
                let index = *req.id().as_number().unwrap();
                this.state.started.fetch_add(1, Ordering::SeqCst);
                let active = this.state.active.fetch_add(1, Ordering::SeqCst) + 1;
                this.state.peak.fetch_max(active, Ordering::SeqCst);
                let _active = Active(this.state.clone());
                this.gates[usize::try_from(index).unwrap()]
                    .acquire()
                    .await
                    .unwrap()
                    .forget();
                this.state.completed.lock().unwrap().push(index);
                let mut response = if this.payload_bytes == 0 {
                    MethodResponse::response(
                        req.id(),
                        ResponsePayload::success(index),
                        this.limit as usize,
                    )
                } else {
                    MethodResponse::response(
                        req.id(),
                        ResponsePayload::success("x".repeat(this.payload_bytes)),
                        this.limit as usize,
                    )
                };
                response.extensions_mut().insert(index);
                response
            }
        }

        fn notification<'a>(
            &self,
            _: Notification<'a>,
        ) -> impl Future<Output = MethodResponse> + Send + 'a {
            self.state.notifications.fetch_add(1, Ordering::SeqCst);
            std::future::ready(MethodResponse::notification())
        }

        fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
            let this = self.clone();
            this.state.batches.fetch_add(1, Ordering::SeqCst);
            async move {
                let mut builder = BatchResponseBuilder::new_with_limit(this.limit as usize);
                let mut notification = false;
                for entry in batch {
                    let response = match entry {
                        Ok(BatchEntry::Call(req)) => this.call(req).await,
                        Ok(BatchEntry::Notification(n)) => {
                            notification = true;
                            this.notification(n).await;
                            continue;
                        }
                        Err(err) => {
                            let (err, id) = err.into_parts();
                            MethodResponse::error(id, err)
                        }
                    };
                    if let Err(err) = builder.append(response) {
                        return err;
                    }
                }
                if notification && builder.is_empty() {
                    MethodResponse::notification()
                } else {
                    MethodResponse::from_batch(builder.finish())
                }
            }
        }
    }

    fn calls(method: &'static str, count: usize) -> Batch<'static> {
        Batch::from(
            (0..count)
                .map(|id| {
                    Ok(BatchEntry::Call(Request::borrowed(
                        method,
                        None,
                        Id::Number(u64::try_from(id).unwrap()),
                    )))
                })
                .collect(),
        )
    }

    fn value(response: &MethodResponse) -> Value {
        serde_json::from_str(response.as_json().get()).unwrap()
    }

    #[test]
    fn concurrency_defaults_to_128_and_validates_all_construction_paths() {
        assert_eq!(RpcBatchConcurrency::default().get(), 128);
        for value in [0, 1025, usize::MAX] {
            assert!(RpcBatchConcurrency::try_from(value).is_err());
            assert!(value.to_string().parse::<RpcBatchConcurrency>().is_err());
            assert!(serde_json::from_value::<RpcBatchConcurrency>(json!(value)).is_err());
        }
        for value in [1, 128, 1024] {
            assert_eq!(RpcBatchConcurrency::try_from(value).unwrap().get(), value);
            assert_eq!(
                value
                    .to_string()
                    .parse::<RpcBatchConcurrency>()
                    .unwrap()
                    .get(),
                value
            );
            assert_eq!(
                serde_json::from_value::<RpcBatchConcurrency>(json!(value))
                    .unwrap()
                    .get(),
                value
            );
        }
    }

    #[tokio::test]
    async fn homogeneous_calls_bound_active_and_buffered_work_and_preserve_order() {
        for (method, concurrency) in [SEND, RECEIPT, "groundhog_chainTip", "unknown"]
            .into_iter()
            .flat_map(|method| [1, 4, 128, 1024].map(|window| (method, window)))
        {
            let count = concurrency * 2 + 3;
            let limit = u32::try_from(count * 64).unwrap();
            let probe = Probe::new(count, limit);
            let service =
                ConcurrentBatchService::new(probe.clone(), limit, concurrency.try_into().unwrap());
            let mut response = Box::pin(service.batch(calls(method, count)));
            // FuturesUnordered may yield before polling the full window.
            std::future::poll_fn(|cx| {
                assert!(response.as_mut().poll(cx).is_pending());
                if probe.state.active.load(Ordering::SeqCst) == concurrency {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;

            // Complete the rest of the window in reverse order, leaving its head blocked.
            for index in (1..concurrency).rev() {
                probe.gates[index].add_permits(1);
                std::future::poll_fn(|cx| {
                    assert!(response.as_mut().poll(cx).is_pending());
                    if probe.state.active.load(Ordering::SeqCst) == index {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                })
                .await;
            }
            assert_eq!(probe.state.started.load(Ordering::SeqCst), concurrency);
            assert_eq!(probe.state.active.load(Ordering::SeqCst), 1);
            assert_eq!(
                *probe.state.completed.lock().unwrap(),
                (1..u64::try_from(concurrency).unwrap())
                    .rev()
                    .collect::<Vec<_>>()
            );

            for gate in probe.gates.iter() {
                gate.add_permits(1);
            }
            let response = response.await;
            let expected: Vec<_> = (0..count)
                .map(|id| json!({"jsonrpc":"2.0", "id":id, "result":id}))
                .collect();
            assert_eq!(value(&response), json!(expected));
            assert_eq!(probe.state.started.load(Ordering::SeqCst), count);
            assert_eq!(probe.state.peak.load(Ordering::SeqCst), concurrency);
            assert_eq!(probe.state.batches.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn dropping_batch_cancels_inflight_calls_without_starting_queued_calls() {
        let probe = Probe::new(BATCH_CONCURRENCY + 1, LIMIT);
        let service = ConcurrentBatchService::new(
            probe.clone(),
            LIMIT,
            BATCH_CONCURRENCY.try_into().unwrap(),
        );
        let mut response = Box::pin(service.batch(calls(SEND, BATCH_CONCURRENCY + 1)));
        assert!(futures_util::poll!(&mut response).is_pending());
        drop(response);
        assert_eq!(probe.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(
            probe.state.started.load(Ordering::SeqCst),
            BATCH_CONCURRENCY
        );
        assert!(probe.state.completed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_entries_preserve_error_ids_and_positions() {
        let probe = Probe::new(2, LIMIT);
        for gate in probe.gates.iter() {
            gate.add_permits(1);
        }
        let mut batch = calls(RECEIPT, 2).into_iter().collect::<Vec<_>>();
        batch.insert(
            0,
            Err(BatchEntryErr::new(
                Id::Null,
                ErrorCode::InvalidRequest.into(),
            )),
        );
        batch.insert(
            2,
            Err(BatchEntryErr::new(
                Id::Str("bad".into()),
                ErrorCode::InvalidParams.into(),
            )),
        );
        let response =
            ConcurrentBatchService::new(probe.clone(), LIMIT, RpcBatchConcurrency::default())
                .batch(Batch::from(batch))
                .await;
        assert_eq!(
            value(&response),
            json!([
                {"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid request"}},
                {"jsonrpc":"2.0","id":0,"result":0},
                {"jsonrpc":"2.0","id":"bad","error":{"code":-32602,"message":"Invalid params"}},
                {"jsonrpc":"2.0","id":1,"result":1}
            ])
        );
        assert_eq!(probe.state.batches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn overflow_matches_sequential_error_and_drops_remaining_window() {
        let probe = Probe::new(BATCH_CONCURRENCY * 2, 60);
        let service =
            ConcurrentBatchService::new(probe.clone(), 60, BATCH_CONCURRENCY.try_into().unwrap());
        let mut response = Box::pin(service.batch(calls(SEND, BATCH_CONCURRENCY * 2)));
        assert!(futures_util::poll!(&mut response).is_pending());
        probe.gates[0].add_permits(1);
        probe.gates[1].add_permits(1);
        let response = response.await;
        let baseline = Probe::new(2, 60);
        for gate in baseline.gates.iter() {
            gate.add_permits(1);
        }
        let expected = baseline.batch(calls(SEND, 2)).await;
        assert_eq!(response.as_json().get(), expected.as_json().get());
        assert_eq!(response.as_error_code(), Some(-32011));
        assert_eq!(probe.state.active.load(Ordering::SeqCst), 0);
        assert!(probe.state.started.load(Ordering::SeqCst) <= BATCH_CONCURRENCY + 1);
        assert_eq!(*probe.state.completed.lock().unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn large_completed_receipts_overflow_before_head_finishes_and_cancel_outstanding_calls() {
        let limit = jsonrpsee::core::TEN_MB_SIZE_BYTES;
        let mut probe = Probe::new(BATCH_CONCURRENCY + 1, limit);
        probe.payload_bytes = 9 * 1024 * 1024;
        let service = ConcurrentBatchService::new(
            probe.clone(),
            limit,
            BATCH_CONCURRENCY.try_into().unwrap(),
        );
        let mut response = Box::pin(service.batch(calls(RECEIPT, BATCH_CONCURRENCY + 1)));
        assert!(futures_util::poll!(&mut response).is_pending());
        assert_eq!(probe.state.active.load(Ordering::SeqCst), BATCH_CONCURRENCY);

        probe.gates[1].add_permits(1);
        assert!(futures_util::poll!(&mut response).is_pending());
        probe.gates[2].add_permits(1);
        let std::task::Poll::Ready(response) = futures_util::poll!(&mut response) else {
            panic!("overflow must not wait for the gated first receipt");
        };
        let expected =
            MethodResponse::error(Id::Null, reject_too_big_batch_response(limit as usize));
        assert_eq!(response.as_json().get(), expected.as_json().get());
        assert_eq!(probe.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(
            probe.state.started.load(Ordering::SeqCst),
            BATCH_CONCURRENCY
        );
        assert_eq!(*probe.state.completed.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn completed_byte_budget_includes_already_emitted_prefix() {
        let probe = Probe::new(4, 90);
        let service =
            ConcurrentBatchService::new(probe.clone(), 90, BATCH_CONCURRENCY.try_into().unwrap());
        let mut response = Box::pin(service.batch(calls(RECEIPT, 4)));
        assert!(futures_util::poll!(&mut response).is_pending());
        for index in [0, 2] {
            probe.gates[index].add_permits(1);
            assert!(futures_util::poll!(&mut response).is_pending());
        }
        probe.gates[3].add_permits(1);
        let std::task::Poll::Ready(response) = futures_util::poll!(&mut response) else {
            panic!("ordered emission must not release the prefix's byte budget");
        };
        assert_eq!(response.as_error_code(), Some(-32011));
        assert_eq!(probe.state.active.load(Ordering::SeqCst), 0);
        assert_eq!(*probe.state.completed.lock().unwrap(), vec![0, 2, 3]);
    }

    #[tokio::test]
    async fn byte_budget_matches_builder_boundary_and_preserves_ordered_extensions() {
        let baseline = Probe::new(3, LIMIT);
        for gate in baseline.gates.iter() {
            gate.add_permits(3);
        }
        let expected = baseline.batch(calls(RECEIPT, 3)).await;
        let exact_limit = u32::try_from(expected.as_json().get().len()).unwrap();
        for limit in [exact_limit, exact_limit - 1] {
            let probe = Probe::new(3, limit);
            let service = ConcurrentBatchService::new(
                probe.clone(),
                limit,
                BATCH_CONCURRENCY.try_into().unwrap(),
            );
            let mut response = Box::pin(service.batch(calls(RECEIPT, 3)));
            assert!(futures_util::poll!(&mut response).is_pending());
            for index in [2, 1] {
                probe.gates[index].add_permits(1);
                assert!(futures_util::poll!(&mut response).is_pending());
            }
            probe.gates[0].add_permits(1);
            let response = response.await;
            let baseline = Probe {
                limit,
                ..baseline.clone()
            };
            let expected = baseline.batch(calls(RECEIPT, 3)).await;
            assert_eq!(response.as_json().get(), expected.as_json().get());
            assert_eq!(
                response.extensions().get::<u64>(),
                expected.extensions().get::<u64>()
            );
            if limit == exact_limit {
                assert_eq!(response.extensions().get::<u64>(), Some(&2));
            } else {
                assert_eq!(response.as_error_code(), Some(-32011));
            }
        }
    }

    #[tokio::test]
    async fn mixed_methods_and_notifications_use_concurrent_dispatch() {
        for other in [RECEIPT, "groundhog_chainTip", "unknown", SEND] {
            let probe = Probe::new(2, LIMIT);
            let service =
                ConcurrentBatchService::new(probe.clone(), LIMIT, RpcBatchConcurrency::default());
            let mut batch = calls(SEND, 1);
            if other == SEND {
                let mut entries = batch.into_iter().collect::<Vec<_>>();
                entries.push(Ok(BatchEntry::Notification(
                    serde_json::from_str(
                        r#"{"jsonrpc":"2.0","method":"groundhog_sendRawTransaction"}"#,
                    )
                    .unwrap(),
                )));
                batch = Batch::from(entries);
            } else {
                batch.push(Request::borrowed(other, None, Id::Number(1)));
            }
            let mut response = Box::pin(service.batch(batch));
            assert!(futures_util::poll!(&mut response).is_pending());
            assert_eq!(probe.state.batches.load(Ordering::SeqCst), 0);
            let calls = if other == SEND { 1 } else { 2 };
            assert_eq!(probe.state.started.load(Ordering::SeqCst), calls);
            assert_eq!(
                probe.state.notifications.load(Ordering::SeqCst),
                usize::from(other == SEND)
            );
            for gate in probe.gates.iter() {
                gate.add_permits(1);
            }
            response.await;
            assert_eq!(probe.state.peak.load(Ordering::SeqCst), calls);
            assert_eq!(
                probe.state.notifications.load(Ordering::SeqCst),
                usize::from(other == SEND)
            );
        }
    }

    #[tokio::test]
    async fn only_empty_arrays_delegate_and_preserve_invalid_request() {
        let probe = Probe::new(0, LIMIT);
        let service =
            ConcurrentBatchService::new(probe.clone(), LIMIT, RpcBatchConcurrency::default());
        let empty = service.batch(Batch::new()).await;
        let baseline = Probe::new(0, LIMIT).batch(Batch::new()).await;
        assert_eq!(empty.as_json().get(), baseline.as_json().get());
        assert_eq!(value(&empty)["error"]["code"], -32600);
        let invalid = service
            .batch(Batch::from(vec![Err(BatchEntryErr::new(
                Id::Null,
                ErrorObject::from(ErrorCode::InvalidRequest),
            ))]))
            .await;
        assert_eq!(value(&invalid)[0]["error"]["code"], -32600);
        let notification = service
            .batch(Batch::from(vec![Ok(BatchEntry::Notification(
                serde_json::from_str(
                    r#"{"jsonrpc":"2.0","method":"groundhog_sendRawTransaction"}"#,
                )
                .unwrap(),
            ))]))
            .await;
        assert!(notification.is_notification());
        assert_eq!(probe.state.batches.load(Ordering::SeqCst), 1);
        assert_eq!(probe.state.started.load(Ordering::SeqCst), 0);
        assert_eq!(probe.state.notifications.load(Ordering::SeqCst), 1);
    }

    pub(crate) async fn http_request(address: std::net::SocketAddr, body: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!(
                "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).await.unwrap();
            response.split_once("\r\n\r\n").unwrap().1.to_owned()
        }).await.unwrap()
    }

    async fn start_server(
        concurrent: bool,
        limit: u32,
    ) -> (
        std::net::SocketAddr,
        jsonrpsee::server::ServerHandle,
        Arc<AtomicUsize>,
    ) {
        let invoked = Arc::new(AtomicUsize::new(0));
        let mut module = jsonrpsee::RpcModule::new(invoked.clone());
        for method in [SEND, RECEIPT, "groundhog_chainTip"] {
            module
                .register_async_method(method, |params, invoked, _| async move {
                    invoked.fetch_add(1, Ordering::SeqCst);
                    let (result,): (Value,) = params.parse()?;
                    if result == "fail" {
                        Err(ErrorObject::owned(-32000, "backend failure", None::<()>))
                    } else {
                        Ok(result)
                    }
                })
                .unwrap();
        }
        let config = jsonrpsee::server::ServerConfig::builder()
            .max_response_body_size(limit)
            .set_batch_request_config(jsonrpsee::server::BatchRequestConfig::Limit(40))
            .build();
        if concurrent {
            let middleware =
                jsonrpsee::core::middleware::RpcServiceBuilder::new().layer_fn(move |inner| {
                    ConcurrentBatchService::new(inner, limit, RpcBatchConcurrency::default())
                });
            let server = jsonrpsee::server::Server::builder()
                .set_config(config)
                .set_rpc_middleware(middleware)
                .build("127.0.0.1:0")
                .await
                .unwrap();
            (server.local_addr().unwrap(), server.start(module), invoked)
        } else {
            let server = jsonrpsee::server::Server::builder()
                .set_config(config)
                .build("127.0.0.1:0")
                .await
                .unwrap();
            (server.local_addr().unwrap(), server.start(module), invoked)
        }
    }

    #[tokio::test]
    async fn http_responses_match_jsonrpsee_for_mixed_invalid_and_overflow_inputs() {
        let call = |method, id, params| json!({"jsonrpc":"2.0", "method":method, "id":id, "params":params});
        let send = call(SEND, json!("same"), json!([7]));
        let receipt = call(RECEIPT, Value::Null, json!([null]));
        let notification = json!({"jsonrpc":"2.0", "method":SEND, "params":[7]});
        let tip = call("groundhog_chainTip", json!(2), json!([true]));
        let unknown = call("unknown", json!(3), json!([]));
        let cases = vec![
            "[".to_owned(),
            json!([]).to_string(),
            json!([null, 7, {}, []]).to_string(),
            send.to_string(),
            notification.to_string(),
            json!([send]).to_string(),
            json!([send, send]).to_string(),
            json!([receipt, receipt]).to_string(),
            json!([null, send, 7, {}, send, []]).to_string(),
            json!([send, call(SEND, json!(2), json!("bad params")), send]).to_string(),
            json!([send, call(SEND, json!(2), json!([])), send]).to_string(),
            json!([send, call(SEND, json!(2), json!(["fail"])), send]).to_string(),
            json!([send, call(SEND, json!({}), json!([7])), send]).to_string(),
            json!([call(SEND, json!(1), json!(["x".repeat(5000)])), send]).to_string(),
            json!([notification, notification]).to_string(),
            json!([send, notification, send]).to_string(),
            json!([send, receipt]).to_string(),
            json!([send, tip, send]).to_string(),
            json!([tip, tip]).to_string(),
            json!([send, notification, null, unknown, receipt, tip]).to_string(),
            json!([unknown, unknown]).to_string(),
            json!([send, unknown]).to_string(),
            json!([notification, null]).to_string(),
            json!(vec![send; 41]).to_string(),
        ];
        for limit in [60, LIMIT] {
            let (address, handle, invoked) = start_server(true, limit).await;
            let (baseline, baseline_handle, baseline_invoked) = start_server(false, limit).await;
            for body in &cases {
                let actual = http_request(address, body).await;
                let expected = http_request(baseline, body).await;
                assert_eq!(actual, expected, "limit={limit}, request={body}");
            }
            // A notification-only request must not invoke registered handlers.
            let before = invoked.load(Ordering::SeqCst);
            let notifications = json!([notification]).to_string();
            assert_eq!(
                http_request(address, &notifications).await,
                http_request(baseline, &notifications).await
            );
            assert_eq!(invoked.load(Ordering::SeqCst), before);
            assert!(baseline_invoked.load(Ordering::SeqCst) > 0);
            handle.stop().unwrap();
            baseline_handle.stop().unwrap();
            handle.stopped().await;
            baseline_handle.stopped().await;
        }
    }
}
