//! JSON-RPC endpoint that answers from per-method handlers and records every
//! request it receives, so a test can assert what the CLI sent once it exits.
//!
//! The server runs on its own thread and runtime, so synchronous and async
//! tests use it alike. Requests for methods without a handler are recorded
//! too (and answered with "method not found"), and so is any HTTP request that
//! never reaches RPC dispatch (a stray `GET`, malformed JSON, the wrong content
//! type), as a synthetic `HTTP <METHOD> <path>` entry carrying the response
//! status. An exact comparison of [`RecordingServer::stop`]'s result therefore
//! rules out any unexpected request.

use jsonrpsee::{
    Extensions, RpcModule,
    server::{
        HttpRequest, HttpResponse, ServerBuilder,
        middleware::{
            http::ProxyGetRequestLayer,
            rpc::{Batch, Notification, Request, RpcServiceBuilder, RpcServiceT},
        },
    },
    types::ErrorObjectOwned,
};
use serde_json::{Value, value::RawValue};
use std::{
    borrow::Cow,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    thread,
};

/// One received request: its method name and its params (`null` when absent).
pub type RecordedCall = (String, Value);

type Calls = Arc<Mutex<Vec<RecordedCall>>>;
type Handler = Arc<dyn Fn(&Value) -> Result<Value, ErrorObjectOwned> + Send + Sync>;

/// Methods and plain `GET` routes a [`RecordingServer`] answers.
#[derive(Default)]
#[must_use]
pub struct RecordingServerBuilder {
    methods: Vec<(&'static str, Handler)>,
    get_routes: Vec<(&'static str, &'static str)>,
}

impl RecordingServerBuilder {
    /// Answer `name` with `handler`, which receives the request params.
    pub fn method(
        mut self,
        name: &'static str,
        handler: impl Fn(&Value) -> Result<Value, ErrorObjectOwned> + Send + Sync + 'static,
    ) -> Self {
        self.methods.push((name, Arc::new(handler)));
        self
    }

    /// Answer `GET path` with the bare result of `method`, recorded under that
    /// method name with `null` params.
    #[allow(dead_code)]
    pub fn get_route(mut self, path: &'static str, method: &'static str) -> Self {
        self.get_routes.push((path, method));
        self
    }

    /// Bind a loopback port and start serving.
    pub fn start(self) -> RecordingServer {
        let calls = Calls::default();
        let mut module = RpcModule::new(());
        for (name, handler) in self.methods {
            module
                .register_method(name, move |params, (), _| {
                    let params: Value = params.parse()?;
                    handler(&params)
                })
                .unwrap();
        }
        let proxy = ProxyGetRequestLayer::new(self.get_routes).unwrap();
        let rpc_recorded = calls.clone();
        let http_recorded = calls.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let server = ServerBuilder::default()
                    .set_http_middleware(
                        tower::ServiceBuilder::new()
                            .layer_fn(move |service| HttpRecorder {
                                service,
                                calls: http_recorded.clone(),
                            })
                            .layer(proxy),
                    )
                    .set_rpc_middleware(RpcServiceBuilder::new().layer_fn(move |service| {
                        Recorder {
                            service,
                            calls: rpc_recorded.clone(),
                        }
                    }))
                    .build("127.0.0.1:0")
                    .await
                    .unwrap();
                let address = server.local_addr().unwrap();
                let handle = server.start(module);
                ready_tx.send(address).unwrap();
                let _ = stop_rx.await;
                let _ = handle.stop();
                handle.stopped().await;
            });
        });
        let address = ready_rx.recv().unwrap();
        RecordingServer {
            url: format!("http://{address}"),
            calls,
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }
}

/// A running endpoint. Dropping it stops the server.
pub struct RecordingServer {
    url: String,
    calls: Calls,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl RecordingServer {
    pub fn builder() -> RecordingServerBuilder {
        RecordingServerBuilder::default()
    }

    /// `http://` URL of the endpoint.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stop the server and return every request it received, in arrival order.
    pub fn stop(mut self) -> Vec<RecordedCall> {
        self.shut_down();
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn shut_down(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Drop for RecordingServer {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// Set in an HTTP request's extensions once one of its calls is recorded, so
/// the HTTP layer knows whether the request reached RPC dispatch.
#[derive(Clone)]
struct Dispatched(Arc<AtomicBool>);

/// Records HTTP requests that never become an RPC call.
#[derive(Clone)]
struct HttpRecorder<S> {
    service: S,
    calls: Calls,
}

impl<S, B, ResponseBody> tower::Service<HttpRequest<B>> for HttpRecorder<S>
where
    S: tower::Service<HttpRequest<B>, Response = HttpResponse<ResponseBody>>,
    S::Future: Send + 'static,
    S::Error: 'static,
    ResponseBody: 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.service.poll_ready(context)
    }

    fn call(&mut self, mut request: HttpRequest<B>) -> Self::Future {
        let dispatched = Arc::new(AtomicBool::new(false));
        request
            .extensions_mut()
            .insert(Dispatched(Arc::clone(&dispatched)));
        let label = format!("HTTP {} {}", request.method(), request.uri().path());
        let calls = Arc::clone(&self.calls);
        let response = self.service.call(request);
        Box::pin(async move {
            let response = response.await;
            if !dispatched.load(Ordering::SeqCst) {
                let status = response
                    .as_ref()
                    .map_or(Value::Null, |response| response.status().as_u16().into());
                calls
                    .lock()
                    .unwrap()
                    .push((label, serde_json::json!({ "status": status })));
            }
            response
        })
    }
}

#[derive(Clone)]
struct Recorder<S> {
    service: S,
    calls: Calls,
}

impl<S> Recorder<S> {
    fn record(&self, extensions: &Extensions, method: &str, params: Option<&Cow<'_, RawValue>>) {
        if let Some(Dispatched(dispatched)) = extensions.get::<Dispatched>() {
            dispatched.store(true, Ordering::SeqCst);
        }
        let params = params.map_or(Value::Null, |raw| serde_json::from_str(raw.get()).unwrap());
        self.calls.lock().unwrap().push((method.to_owned(), params));
    }
}

impl<S> RpcServiceT for Recorder<S>
where
    S: RpcServiceT + Send + Sync + Clone + 'static,
{
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(
        &self,
        request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        self.record(
            request.extensions(),
            request.method_name(),
            request.params.as_ref(),
        );
        self.service.call(request)
    }

    fn batch<'a>(
        &self,
        requests: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        for entry in requests.iter().flatten() {
            self.record(entry.extensions(), entry.method_name(), entry.params());
        }
        self.service.batch(requests)
    }

    fn notification<'a>(
        &self,
        notification: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.record(
            notification.extensions(),
            notification.method_name(),
            notification.params.as_ref(),
        );
        self.service.notification(notification)
    }
}
