use jsonrpsee::core::RpcResult;

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::IdentifiedEvent,
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};

use crate::{
    ChainPlane, ReadPlane, RpcServerConfig, RpcServerError, SubmitPlane,
    tx::validate_raw_transaction,
};

/// Generic implementation of the public Groundhog JSON-RPC API over
/// independently-typed submit, chain, and read backend planes.
#[derive(Debug, Clone)]
pub struct GroundhogRpcServer<S, C, R> {
    submit: S,
    chain: C,
    read: R,
    config: RpcServerConfig,
}

impl<S, C, R> GroundhogRpcServer<S, C, R> {
    /// Create a server wrapper around independent backend planes.
    pub fn new(submit: S, chain: C, read: R, config: RpcServerConfig) -> Self {
        Self {
            submit,
            chain,
            read,
            config,
        }
    }
}

impl<B> GroundhogRpcServer<B, B, B>
where
    B: Clone,
{
    /// Convenience constructor for backends that implement all three planes
    /// on a single type.
    pub fn unified(backend: B, config: RpcServerConfig) -> Self {
        Self::new(backend.clone(), backend.clone(), backend, config)
    }
}

#[async_trait::async_trait]
impl<S, C, R> GroundhogApiServer for GroundhogRpcServer<S, C, R>
where
    S: SubmitPlane,
    C: ChainPlane,
    R: ReadPlane,
{
    #[tracing::instrument(skip_all, fields(tx_hex_len = tx_hex.len()))]
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash> {
        let tx = validate_raw_transaction(&tx_hex)?;
        let tx_hash = tx.hash;
        self.submit.submit_raw_transaction(tx).await?;
        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        Ok(self.read.transaction_receipt(tx_hash).await?)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        Ok(self.read.transaction_status(tx_hash).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        Ok(self.chain.chain_tip().await?)
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        Ok(self.chain.get_block_by_number(height).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        Ok(self.chain.pending_transaction_count().await?)
    }

    #[tracing::instrument(skip_all, fields(?address, ?from_height, ?to_height))]
    async fn get_contract_events(
        &self,
        address: ContractAddress,
        from_height: BlockHeight,
        to_height: BlockHeight,
    ) -> RpcResult<Vec<IdentifiedEvent>> {
        validate_event_range(&from_height, &to_height, self.config.max_event_height_range)?;
        Ok(self
            .read
            .contract_events(address, from_height, to_height)
            .await?)
    }
}

fn validate_event_range(
    from_height: &BlockHeight,
    to_height: &BlockHeight,
    max_event_height_range: u64,
) -> Result<(), RpcServerError> {
    let from = **from_height;
    let to = **to_height;
    if from < 0 || to < 0 {
        return Err(RpcServerError::InvalidParam(
            "block height must be non-negative".into(),
        ));
    }
    if to < from {
        return Err(RpcServerError::InvalidParam(
            "to_height must be >= from_height".into(),
        ));
    }

    // Both `to` and `from` are non-negative i64 with `to >= from`,
    // so the subtraction is in `[0, i64::MAX]` and fits in `u64`.
    let span = (to - from).cast_unsigned();
    if span > max_event_height_range {
        return Err(RpcServerError::RangeTooLarge(format!(
            "height range {span} exceeds maximum {max_event_height_range}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Mutex;

    use super::*;
    use crate::ValidatedTransaction;
    use groundhog_rpc_api_types::hex::Hex0x;

    const SIGNED_TX_XDR: &str = "\
bc10f38522c4e14265a1483af9e5375f5dea0e2c6bf25727ee8b429bf3c1a9b7\
000000000000000000000000000f42400000000000000000000f424000000000000000010000000000000001\
000000040000000050ec72dbafd4898c5df0e15ea6ba3e46240945b7210fe2d63dde1e896f42107a000000\
00000000000000000000000001000000000000000000000000706f58acd087550f315030db4beeb1527e86\
f0e12d9781957cdcc522f33e5d719b93c200381cff5bfb26a1ee8ce7874ffd485322cd5a0806605d2ffa\
51c8cf0f";

    #[derive(Default)]
    struct MockBackend {
        submitted: Mutex<Vec<ValidatedTransaction>>,
        event_calls: Mutex<u32>,
    }

    #[derive(Clone)]
    struct SubmitOnly {
        submitted: Arc<Mutex<Vec<ValidatedTransaction>>>,
    }

    #[derive(Clone, Copy)]
    struct ChainOnly;

    #[derive(Clone)]
    struct ReadOnly {
        event_calls: Arc<Mutex<u32>>,
    }

    fn server(
        mock: Arc<MockBackend>,
    ) -> GroundhogRpcServer<Arc<MockBackend>, Arc<MockBackend>, Arc<MockBackend>> {
        GroundhogRpcServer::unified(
            mock,
            RpcServerConfig::default().with_max_event_height_range(10),
        )
    }

    #[async_trait::async_trait]
    impl SubmitPlane for Arc<MockBackend> {
        async fn submit_raw_transaction(
            &self,
            tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.submitted.lock().await.push(tx);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl SubmitPlane for SubmitOnly {
        async fn submit_raw_transaction(
            &self,
            tx: ValidatedTransaction,
        ) -> Result<(), RpcServerError> {
            self.submitted.lock().await.push(tx);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ChainPlane for Arc<MockBackend> {
        async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
            Err(RpcServerError::NotImplemented("chainTip".into()))
        }

        async fn get_block_by_number(
            &self,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Err(RpcServerError::NotImplemented("getBlockByNumber".into()))
        }

        async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
            Err(RpcServerError::NotImplemented(
                "pendingTransactionCount".into(),
            ))
        }
    }

    #[async_trait::async_trait]
    impl ChainPlane for ChainOnly {
        async fn chain_tip(&self) -> Result<ChainTip, RpcServerError> {
            Ok(ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0u8; 32])),
            })
        }

        async fn get_block_by_number(
            &self,
            _height: BlockHeight,
        ) -> Result<Option<BlockInfo>, RpcServerError> {
            Ok(None)
        }

        async fn pending_transaction_count(&self) -> Result<u64, RpcServerError> {
            Ok(7)
        }
    }

    #[async_trait::async_trait]
    impl ReadPlane for Arc<MockBackend> {
        async fn transaction_receipt(
            &self,
            _tx_hash: TxHash,
        ) -> Result<Option<TransactionReceipt>, RpcServerError> {
            Ok(None)
        }

        async fn transaction_status(
            &self,
            _tx_hash: TxHash,
        ) -> Result<TransactionStatus, RpcServerError> {
            Ok(TransactionStatus::Unknown)
        }

        async fn contract_events(
            &self,
            _address: ContractAddress,
            _from_height: BlockHeight,
            _to_height: BlockHeight,
        ) -> Result<Vec<IdentifiedEvent>, RpcServerError> {
            *self.event_calls.lock().await += 1;
            Ok(vec![])
        }
    }

    #[async_trait::async_trait]
    impl ReadPlane for ReadOnly {
        async fn transaction_receipt(
            &self,
            _tx_hash: TxHash,
        ) -> Result<Option<TransactionReceipt>, RpcServerError> {
            Ok(None)
        }

        async fn transaction_status(
            &self,
            _tx_hash: TxHash,
        ) -> Result<TransactionStatus, RpcServerError> {
            Ok(TransactionStatus::Unknown)
        }

        async fn contract_events(
            &self,
            _address: ContractAddress,
            _from_height: BlockHeight,
            _to_height: BlockHeight,
        ) -> Result<Vec<IdentifiedEvent>, RpcServerError> {
            *self.event_calls.lock().await += 1;
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn valid_submit_calls_backend_and_returns_hash() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let tx_hash = server
            .send_raw_transaction(format!("0x{SIGNED_TX_XDR}"))
            .await
            .expect("bug: valid signed tx should submit");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            "6e94695b6af0fbc13d96876f7fb8e940cd9db4d247e53614a45da252b3a7853a"
        );
        assert_eq!(mock.submitted.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn distinct_backend_plane_types_are_accepted() {
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let event_calls = Arc::new(Mutex::new(0));
        let server = GroundhogRpcServer::new(
            SubmitOnly {
                submitted: submitted.clone(),
            },
            ChainOnly,
            ReadOnly {
                event_calls: event_calls.clone(),
            },
            RpcServerConfig::default().with_max_event_height_range(10),
        );

        let tx_hash = server
            .send_raw_transaction(format!("0x{SIGNED_TX_XDR}"))
            .await
            .expect("bug: valid signed tx should submit through submit plane");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            "6e94695b6af0fbc13d96876f7fb8e940cd9db4d247e53614a45da252b3a7853a"
        );
        assert_eq!(submitted.lock().await.len(), 1);
        assert_eq!(
            server
                .pending_transaction_count()
                .await
                .expect("bug: chain plane should answer pending count"),
            7
        );

        server
            .get_contract_events(address(), BlockHeight::from(1), BlockHeight::from(2))
            .await
            .expect("bug: read plane should answer contract events");

        assert_eq!(*event_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn inverted_event_range_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let err = server
            .get_contract_events(address(), BlockHeight::from(2), BlockHeight::from(1))
            .await
            .expect_err("bug: inverted event range should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn excessive_event_range_is_rejected_before_backend_dispatch() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let err = server
            .get_contract_events(address(), BlockHeight::from(1), BlockHeight::from(12))
            .await
            .expect_err("bug: excessive event range should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::RANGE_TOO_LARGE
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn exact_max_event_range_passes_to_backend() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        // Span of 10 is the configured maximum and must be accepted.
        server
            .get_contract_events(address(), BlockHeight::from(1), BlockHeight::from(11))
            .await
            .expect("bug: span of exactly max should succeed");

        assert_eq!(*mock.event_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn zero_span_event_range_passes_to_backend() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        server
            .get_contract_events(address(), BlockHeight::from(5), BlockHeight::from(5))
            .await
            .expect("bug: zero-span range should succeed");

        assert_eq!(*mock.event_calls.lock().await, 1);
    }

    #[tokio::test]
    async fn negative_from_height_is_rejected_as_invalid_param() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let err = server
            .get_contract_events(address(), BlockHeight::from(-1), BlockHeight::from(5))
            .await
            .expect_err("bug: negative from_height should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn negative_to_height_is_rejected_as_invalid_param() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock.clone());

        let err = server
            .get_contract_events(address(), BlockHeight::from(0), BlockHeight::from(-1))
            .await
            .expect_err("bug: negative to_height should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*mock.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn backend_not_implemented_maps_to_public_code() {
        let mock = Arc::new(MockBackend::default());
        let server = server(mock);

        let err = server
            .pending_transaction_count()
            .await
            .expect_err("bug: mock pending count should be unimplemented");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::NOT_IMPLEMENTED
        );
    }

    fn address() -> ContractAddress {
        Hex0x(groundhog_xdr::types::ContractAddress::from([0u8; 32]))
    }
}
