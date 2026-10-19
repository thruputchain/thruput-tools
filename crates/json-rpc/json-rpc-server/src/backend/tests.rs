use std::{
    future::pending,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use super::{RpcRequestContext, RpcServerError};

struct Dropped(Arc<AtomicBool>);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[tokio::test(start_paused = true)]
async fn deadline_cancels_nested_work_and_reuses_the_outer_timeout() {
    let context = RpcRequestContext::new(Some(Duration::from_secs(1)));
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = Dropped(dropped.clone());
    let nested_returned = Arc::new(AtomicBool::new(false));
    let returned = nested_returned.clone();
    let result = context
        .within_deadline(async move {
            let _guard = guard;
            context
                .within_deadline(pending::<()>())
                .await
                .expect("outer timer owns expiry");
            returned.store(true, Ordering::Relaxed);
        })
        .await;
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
    assert!(dropped.load(Ordering::Relaxed));
    assert!(!nested_returned.load(Ordering::Relaxed));
}

#[tokio::test(start_paused = true)]
async fn shorter_deadline_expires_without_cancelling_the_outer_operation() {
    let outer = RpcRequestContext::new(Some(Duration::from_secs(10)));
    let shorter = outer.capped_from_start(Duration::from_secs(1));
    let result = outer
        .within_deadline(async {
            assert!(matches!(
                shorter.within_deadline(pending::<()>()).await,
                Err(RpcServerError::BackendTimeout)
            ));
            tokio::time::sleep(Duration::from_secs(2)).await;
            7
        })
        .await;
    assert!(matches!(result, Ok(7)));
}

#[tokio::test(start_paused = true)]
async fn spawned_work_keeps_its_deadline_after_the_parent_scope_finishes() {
    let outer = RpcRequestContext::new(Some(Duration::from_secs(1)));
    let child = RpcRequestContext::new(Some(Duration::from_secs(2)));
    let mut task = None;
    outer
        .within_deadline(async {
            task = Some(tokio::spawn(child.within_deadline(pending::<()>())));
        })
        .await
        .unwrap();
    let task = task.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
}

#[tokio::test(start_paused = true)]
async fn dropping_a_pending_scope_restores_independent_deadline_enforcement() {
    let context = RpcRequestContext::new(Some(Duration::from_secs(1)));
    let mut scoped = Box::pin(context.within_deadline(pending::<()>()));
    std::future::poll_fn(|cx| {
        assert!(scoped.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(scoped);
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        context.within_deadline(pending::<()>()),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
}

#[tokio::test(start_paused = true)]
async fn an_unbounded_context_leaves_the_future_result_unchanged() {
    assert_eq!(
        RpcRequestContext::unbounded()
            .within_deadline(async { 9 })
            .await
            .unwrap(),
        9
    );
}

#[tokio::test(start_paused = true)]
async fn partially_polled_nested_work_enforces_its_deadline_outside_the_parent_scope() {
    let outer = RpcRequestContext::new(Some(Duration::from_secs(1)));
    let child = RpcRequestContext::new(Some(Duration::from_secs(2)));
    let mut nested = Box::pin(child.within_deadline(pending::<()>()));
    outer
        .within_deadline(std::future::poll_fn(|cx| {
            assert!(nested.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        }))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), nested)
        .await
        .unwrap();
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
}

#[tokio::test(start_paused = true)]
async fn exhausting_the_cooperative_budget_cannot_starve_deadline_enforcement() {
    let context = RpcRequestContext::new(Some(Duration::from_secs(1)));
    let task = tokio::spawn(context.within_deadline(async {
        loop {
            tokio::task::consume_budget().await;
        }
    }));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        task.await.unwrap(),
        Err(RpcServerError::BackendTimeout)
    ));
}
