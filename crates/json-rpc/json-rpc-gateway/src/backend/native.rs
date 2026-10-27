use super::{
    ContractAddress, GatewayBackend, RpcRequestContext, RpcServerError, async_trait, handlers,
};
use thruput_json_rpc_server::native::transaction::{NativeReadError, NativeTransactionReadPlane};
use thruput_json_rpc_server::{
    ViewBatchConfig,
    native::read::{NativeSimulationPlane, NativeStatePlane, StateKey},
};
use thruput_json_rpc_types::{requests as input, responses as out, transaction::TxHash};

#[async_trait]
impl NativeStatePlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_storage(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
        key: StateKey,
    ) -> Result<out::StorageResult, RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx
            .storage_batcher
            .native_storage(context, address, key)
            .await
    }
    #[tracing::instrument(skip_all)]
    async fn native_registered_auth(
        &self,
        context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<out::RegisteredAuthResult, RpcServerError> {
        let context = self.bounded_context(context);
        self.ctx
            .storage_batcher
            .native_registered_auth(context, address)
            .await
    }
}

#[async_trait]
impl NativeSimulationPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn simulate(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationRequest,
        gas_limit: u64,
    ) -> Result<out::SimulationResult, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::view::native_simulate(&self.ctx, context, request, gas_limit).await
    }
    #[tracing::instrument(skip_all)]
    async fn simulate_batch(
        &self,
        context: RpcRequestContext,
        request: input::SimulateInvocationsRequest,
        gas_limit: u64,
        limits: ViewBatchConfig,
    ) -> Result<out::SimulationResults, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::view::native_simulate_batch(&self.ctx, context, request, gas_limit, limits).await
    }
}

#[async_trait]
impl NativeTransactionReadPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_receipt(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<Option<out::TransactionReceipt>, NativeReadError> {
        let context = self.bounded_context(context);
        handlers::receipt::native_transaction_receipt(&self.ctx, context, tx_hash).await
    }

    #[tracing::instrument(skip_all)]
    async fn native_status(
        &self,
        context: RpcRequestContext,
        tx_hash: TxHash,
    ) -> Result<out::TransactionStatus, NativeReadError> {
        let context = self.bounded_context(context);
        handlers::receipt::native_transaction_status(&self.ctx, context, tx_hash).await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::transaction::NativeSubmitPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_submit(
        &self,
        context: RpcRequestContext,
        tx: thruput_json_rpc_server::ValidatedTransaction,
        attempt: thruput_json_rpc_server::native::transaction::SubmissionAttempt,
    ) -> Result<(), RpcServerError> {
        let context = self.bounded_context(context);
        let _guard = attempt.guard();
        self.ctx
            .batch_submitter
            .submit_native(context, tx.bytes, tx.hash, attempt)
            .await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::read::NativeBlockPlane for GatewayBackend {
    #[tracing::instrument(skip_all)]
    async fn native_block(
        &self,
        context: RpcRequestContext,
        height: thruput_json_rpc_types::BlockHeight,
    ) -> Result<out::BlockResult, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::blocks::native_block_by_height(&self.ctx, context, height).await
    }
}

#[async_trait]
impl thruput_json_rpc_server::native::read::NativeHistoryPlane for GatewayBackend {
    #[tracing::instrument(skip_all, fields(filters_len = request.filters.len(), limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_events(
        &self,
        context: RpcRequestContext,
        request: input::EventQuery,
    ) -> Result<out::EventPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::native_history::events(&self.ctx, context, request).await
    }
    #[tracing::instrument(skip_all, fields(address = ?request.address, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_root_transactions(
        &self,
        context: RpcRequestContext,
        request: input::RootAddressQuery,
    ) -> Result<out::TransactionRefPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::native_history::root_transactions(&self.ctx, context, request).await
    }
    #[tracing::instrument(skip_all, fields(block_height = ?request.block_height, limit = ?request.limit, has_cursor = request.cursor.is_some()))]
    async fn native_block_transactions(
        &self,
        context: RpcRequestContext,
        request: input::BlockPageRequest,
    ) -> Result<out::BlockTransactionPage, RpcServerError> {
        let context = self.bounded_context(context);
        handlers::native_history::block_transactions(&self.ctx, context, request).await
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use thruput_hogd_backend::submit::SubmitBatch;
    use thruput_json_rpc_server::{
        BuildVersion, RpcServerConfig,
        native::service::{NativeCapabilities, NativeService},
    };
    use thruput_json_rpc_types::{NetworkHash, error::codes};
    use thruput_node_rpc::SchedGatewaySubmitOutcomeBatch;
    use thruput_xdr::types::SHA256Hash;
    use tokio::sync::RwLock;

    use super::{GatewayBackend, RpcServerError, async_trait};
    use crate::{
        config::GatewayConfig,
        context::GatewayContext,
        health::{GatewayHealthSnapshot, HealthStatus},
        metrics::GatewayMetrics,
        submitter::{BatchSubmitter, SubmitSink},
    };

    /// Structurally valid signed transaction (the shared server fixture).
    const SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
13df30e7ceeb7c4c78ba6287e9d8830cabf5a744f3d6be3139f2428d7bb4f9e7000000000000000000000001\
000000000000000041d0144af2776cae9804ed899018a6bc17a5705d74c84e02556b9695031c9d3fe2554e5c\
e62ceadc95ba43c4aa0884cd2f9ebea4e3845ef877761a6222e91f0d000000010000000050ec72dbafd4898c\
5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a";

    /// A scheduler that accepts the batch and never answers.
    struct SilentSink;

    #[async_trait]
    impl SubmitSink for SilentSink {
        async fn submit(
            &self,
            _batch: SubmitBatch<Vec<u8>>,
            _deadline: Instant,
        ) -> Result<SchedGatewaySubmitOutcomeBatch, RpcServerError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn send_transaction_at_request_deadline_counts_backend_timeout() {
        let config = Arc::new(GatewayConfig {
            request_timeout_ms: 50,
            ..GatewayConfig::default()
        });
        let metrics = GatewayMetrics::for_tests();
        let mut ctx = GatewayContext::assemble(
            thruput_xdr::PROTOCOL_VERSION,
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            config.clone(),
            metrics.clone(),
        );
        ctx.batch_submitter.abort();
        ctx.batch_submitter = BatchSubmitter::spawn_with_deadlines(
            &config.batch,
            &(Arc::new(SilentSink) as Arc<dyn SubmitSink>),
            metrics.clone(),
            config.request_timeout(),
            config.backend_request_timeout(),
            config.request_timeout_margin(),
        );
        let backend = GatewayBackend::new(Arc::new(ctx));
        let module = NativeService::new(
            backend.clone(),
            backend.clone(),
            backend.clone(),
            backend,
            NetworkHash::from(SHA256Hash::from([0; 32])),
            RpcServerConfig::default().with_request_timeout_ms(config.request_timeout_ms),
            BuildVersion::new("test", None),
            NativeCapabilities::new(true, false, false, false),
        )
        .into_module()
        .unwrap()
        .into_rpc();

        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "sendTransaction",
            "params": {"signedTransaction": format!("0x{SIGNED_TX_XDR}")},
        });
        let (response, _) = module
            .raw_json_request(&request.to_string(), 1)
            .await
            .unwrap();
        let response: serde_json::Value = serde_json::from_str(response.get()).unwrap();
        assert_eq!(
            response["error"]["code"],
            codes::BACKEND_TIMEOUT,
            "{response}"
        );

        let snapshot = GatewayHealthSnapshot::for_test(HealthStatus::Starting, None, &[]);
        let body = metrics.gather(&snapshot).unwrap();
        assert!(
            body.contains(
                "groundhog_gateway_submitted_transactions_total{outcome=\"backend_timeout\"} 1"
            ),
            "{body}"
        );
    }
}
