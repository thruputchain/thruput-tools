use jsonrpsee::core::RpcResult;

use groundhog_rpc_api::GroundhogApiServer;
use groundhog_rpc_api_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::IdentifiedEvent,
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};

use crate::{GroundhogBackend, RpcServerConfig, RpcServerError, tx::validate_raw_transaction};

/// Generic implementation of the public Groundhog JSON-RPC API.
#[derive(Debug, Clone)]
pub struct GroundhogRpcServer<B> {
    backend: B,
    config: RpcServerConfig,
}

impl<B> GroundhogRpcServer<B> {
    /// Create a server wrapper around a concrete backend adapter.
    pub fn new(backend: B, config: RpcServerConfig) -> Self {
        Self { backend, config }
    }
}

#[async_trait::async_trait]
impl<B> GroundhogApiServer for GroundhogRpcServer<B>
where
    B: GroundhogBackend,
{
    #[tracing::instrument(skip_all, fields(tx_hex_len = tx_hex.len()))]
    async fn send_raw_transaction(&self, tx_hex: String) -> RpcResult<TxHash> {
        let tx = validate_raw_transaction(&tx_hex)?;
        let tx_hash = tx.hash;
        self.backend.submit_raw_transaction(tx).await?;
        Ok(tx_hash)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_receipt(
        &self,
        tx_hash: TxHash,
    ) -> RpcResult<Option<TransactionReceipt>> {
        Ok(self.backend.transaction_receipt(tx_hash).await?)
    }

    #[tracing::instrument(skip_all, fields(?tx_hash))]
    async fn get_transaction_status(&self, tx_hash: TxHash) -> RpcResult<TransactionStatus> {
        Ok(self.backend.transaction_status(tx_hash).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn chain_tip(&self) -> RpcResult<ChainTip> {
        Ok(self.backend.chain_tip().await?)
    }

    #[tracing::instrument(skip_all, fields(?height))]
    async fn get_block_by_number(&self, height: BlockHeight) -> RpcResult<Option<BlockInfo>> {
        Ok(self.backend.get_block_by_number(height).await?)
    }

    #[tracing::instrument(skip_all)]
    async fn pending_transaction_count(&self) -> RpcResult<u64> {
        Ok(self.backend.pending_transaction_count().await?)
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
            .backend
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
    if to < from {
        return Err(RpcServerError::InvalidParam(
            "to_height must be >= from_height".into(),
        ));
    }

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
    use crate::{ChainPlane, ReadPlane, SubmitPlane, ValidatedTransaction};
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

    impl MockBackend {
        fn server(self) -> GroundhogRpcServer<Arc<Self>> {
            GroundhogRpcServer::new(
                Arc::new(self),
                RpcServerConfig {
                    max_event_height_range: 10,
                },
            )
        }
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

    #[tokio::test]
    async fn valid_submit_calls_backend_and_returns_hash() {
        let server = MockBackend::default().server();

        let tx_hash = server
            .send_raw_transaction(format!("0x{SIGNED_TX_XDR}"))
            .await
            .expect("bug: valid signed tx should submit");

        assert_eq!(
            hex::encode(tx_hash.0.0.as_ref()),
            "6e94695b6af0fbc13d96876f7fb8e940cd9db4d247e53614a45da252b3a7853a"
        );
        assert_eq!(server.backend.submitted.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn inverted_event_range_is_rejected_before_backend_dispatch() {
        let server = MockBackend::default().server();

        let err = server
            .get_contract_events(address(), BlockHeight::from(2), BlockHeight::from(1))
            .await
            .expect_err("bug: inverted event range should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::INVALID_PARAMS
        );
        assert_eq!(*server.backend.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn excessive_event_range_is_rejected_before_backend_dispatch() {
        let server = MockBackend::default().server();

        let err = server
            .get_contract_events(address(), BlockHeight::from(1), BlockHeight::from(12))
            .await
            .expect_err("bug: excessive event range should fail");

        assert_eq!(
            err.code(),
            groundhog_rpc_api_types::error::codes::RANGE_TOO_LARGE
        );
        assert_eq!(*server.backend.event_calls.lock().await, 0);
    }

    #[tokio::test]
    async fn backend_not_implemented_maps_to_public_code() {
        let server = MockBackend::default().server();

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
