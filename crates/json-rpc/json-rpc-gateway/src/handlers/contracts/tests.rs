use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use rpc_driver::{CallBuf, ProgVers, RpcError, RpcResponse, RpcSender};
use thruput_db_gateway::DbGateway_DbGateway1_Client;
use tokio::sync::{RwLock, Semaphore};

use super::*;
use crate::{config::GatewayConfig, context::BackendClient, metrics::GatewayMetrics};

struct GatedSender {
    calls: AtomicUsize,
    release: Semaphore,
}

#[async_trait::async_trait]
impl RpcSender for GatedSender {
    fn call_buf(&self) -> CallBuf {
        CallBuf::new()
    }

    async fn send_call(&self, _: ProgVers, _: u32, _: CallBuf) -> Result<RpcResponse, RpcError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.release.acquire().await.unwrap().forget();
        Err(RpcError::SystemError)
    }
}

fn context() -> (GatewayContext, Arc<GatedSender>) {
    let sender = Arc::new(GatedSender {
        calls: AtomicUsize::new(0),
        release: Semaphore::new(0),
    });
    let client = DbGateway_DbGateway1_Client::new(sender.clone());
    let ctx = GatewayContext::assemble(
        thruput_xdr::PROTOCOL_VERSION,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(Some(BackendClient::new(
            client.clone(),
            client,
        )))),
        Arc::new(GatewayConfig::default()),
        GatewayMetrics::for_tests(),
    );
    (ctx, sender)
}

fn hash() -> SHA256Hash {
    thruput_xdr::types::SHA256Hash::from([7; 32]).into()
}

fn request() -> RpcRequestContext {
    RpcRequestContext::new(Some(Duration::from_secs(5)))
}

fn poll_pending<F: Future>(future: std::pin::Pin<&mut F>) {
    assert!(
        future
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending()
    );
}

#[tokio::test]
async fn burst_bounds_pending_backend_calls_and_queue_across_requests() {
    let (ctx, sender) = context();
    let mut reads = (0..64)
        .map(|_| Box::pin(artifact(&ctx, request(), hash())))
        .collect::<Vec<_>>();
    for read in &mut reads {
        poll_pending(read.as_mut());
    }
    assert_eq!(sender.calls.load(Ordering::SeqCst), 4);
    assert!(matches!(
        artifact(&ctx, request(), hash()).await,
        Err(RpcServerError::Overloaded(_))
    ));
    // Dropping queued requests releases admission without dispatching work.
    reads.truncate(4);
    assert_eq!(ctx.artifact_read_admission.available_permits(), 60);
    assert_eq!(sender.calls.load(Ordering::SeqCst), 4);
    sender.release.add_permits(4);
    for read in reads {
        assert!(matches!(
            read.await,
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }
    assert_eq!(ctx.artifact_read_admission.available_permits(), 64);
    assert_eq!(ctx.artifact_read_execution.available_permits(), 4);
}

#[tokio::test(start_paused = true)]
async fn queued_deadline_never_dispatches_and_returns_admission() {
    let (ctx, sender) = context();
    let _busy = ctx.artifact_read_execution.acquire_many(4).await.unwrap();
    assert!(matches!(
        artifact(&ctx, request(), hash()).await,
        Err(RpcServerError::BackendTimeout)
    ));
    assert_eq!(sender.calls.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.artifact_read_admission.available_permits(), 64);
}

#[tokio::test(start_paused = true)]
async fn connection_lock_wait_obeys_deadline_and_releases_permits() {
    let (ctx, sender) = context();
    let _lock = ctx.db_gateway.write().await;
    assert!(matches!(
        artifact(&ctx, request(), hash()).await,
        Err(RpcServerError::BackendTimeout)
    ));
    assert_eq!(sender.calls.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.artifact_read_admission.available_permits(), 64);
    assert_eq!(ctx.artifact_read_execution.available_permits(), 4);
}

#[tokio::test]
async fn canceling_active_read_releases_both_permits() {
    let (ctx, sender) = context();
    let mut read = Box::pin(artifact(&ctx, request(), hash()));
    poll_pending(read.as_mut());
    assert_eq!(sender.calls.load(Ordering::SeqCst), 1);
    drop(read);
    assert_eq!(ctx.artifact_read_admission.available_permits(), 64);
    assert_eq!(ctx.artifact_read_execution.available_permits(), 4);
}
