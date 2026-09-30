use std::future::Future;

use groundhog_json_rpc_server::{RpcRequestContext, RpcServerError};

pub(crate) async fn within_request_deadline<T>(
    context: RpcRequestContext,
    future: impl Future<Output = Result<T, RpcServerError>>,
) -> Result<T, RpcServerError> {
    let Some(deadline) = context.deadline() else {
        return future.await;
    };
    tokio::time::timeout_at(deadline.into(), future)
        .await
        .map_err(|_| RpcServerError::BackendTimeout)?
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn bounded_context_times_out_backend_work() {
        let result = within_request_deadline(
            RpcRequestContext::new(Some(Duration::from_millis(5))),
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            },
        )
        .await;

        assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
    }

    #[tokio::test]
    async fn unbounded_context_preserves_embedding_policy() {
        let result = within_request_deadline(RpcRequestContext::unbounded(), async { Ok(7) })
            .await
            .expect("unbounded work should complete");

        assert_eq!(result, 7);
    }
}
