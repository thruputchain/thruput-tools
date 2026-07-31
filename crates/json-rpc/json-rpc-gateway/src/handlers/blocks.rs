//! `groundhog_chainTip`, `groundhog_getBlockByNumber`, and
//! `groundhog_pendingTransactionCount` handlers.

use groundhog_indexer::{BlockLookupOutcome, BlockLookupResult};
use groundhog_json_rpc_server::RpcRequestContext;
use groundhog_json_rpc_types::BlockHeight;
use groundhog_json_rpc_types::block::{BlockInfo, ChainTip};
use std::time::Instant;

use crate::config::HealthBackend;
use crate::context::GatewayContext;
use crate::error::{self, GatewayError};
use crate::metrics::BackendRequestOutcome;

use super::backend_deadline;

/// Current chain tip: height + block hash.
///
/// Routes through the per-gateway `TipCoalescer` so that concurrent
/// client requests share a single in-flight `schedcoord_get_tip` RPC.
/// See `crate::tip_coalescer` for the linearizable coalescing design.
#[tracing::instrument(skip_all)]
pub async fn chain_tip(
    ctx: &GatewayContext,
    context: RpcRequestContext,
) -> Result<ChainTip, GatewayError> {
    ctx.tip_coalescer.chain_tip(context).await
}

/// Block data at a given height.
#[tracing::instrument(skip_all, fields(height))]
pub async fn get_block_by_number(
    ctx: &GatewayContext,
    context: RpcRequestContext,
    height: BlockHeight,
) -> Result<Option<BlockInfo>, GatewayError> {
    let client = ctx
        .indexer
        .read()
        .await
        .clone()
        .ok_or_else(|| GatewayError::BackendUnavailable("indexer not connected".into()))?;

    let started_at = Instant::now();
    let deadline = backend_deadline(context, ctx.config.backend_request_timeout())?;
    let rpc_result =
        tokio::time::timeout_at(deadline.into(), client.get_block_by_height(&height)).await;
    ctx.metrics.observe_backend_request(
        HealthBackend::Indexer,
        "get_block_by_height",
        match &rpc_result {
            Ok(Ok(_)) => BackendRequestOutcome::Ok,
            Ok(Err(_)) => BackendRequestOutcome::Error,
            Err(_) => BackendRequestOutcome::Timeout,
        },
        started_at.elapsed(),
    );
    let result = rpc_result
        .map_err(|_| GatewayError::BackendTimeout)?
        .map_err(|error| GatewayError::BackendUnavailable(format!("indexer: {error}")))?;

    match result {
        BlockLookupResult::BlockFound { value } => BlockInfo::try_from(value)
            .map(Some)
            .map_err(|error| GatewayError::BackendUnavailable(format!("indexer block: {error}"))),
        BlockLookupResult::Default { outcome }
            if outcome == BlockLookupOutcome::BlockNotFound.0 =>
        {
            Ok(None)
        }
        BlockLookupResult::Default { outcome } => Err(GatewayError::BackendUnavailable(format!(
            "indexer returned unexpected block lookup outcome: {outcome}"
        ))),
        _ => Err(GatewayError::BackendUnavailable(
            "indexer returned unknown block lookup result variant".into(),
        )),
    }
}

/// Number of transactions in the mempool.
///
/// **Deferred** — no `size()` RPC on the scheduler interface yet.
#[tracing::instrument(skip_all)]
pub async fn pending_transaction_count(_ctx: &GatewayContext) -> Result<u64, GatewayError> {
    Err(error::not_implemented("pendingTransactionCount"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use groundhog_indexer::{
        AddressTransactionQuery as WireAddressTransactionQuery,
        BlockTransactionQuery as WireBlockTransactionQuery, EventPage, EventQuery,
        IndexerBlockExplorer_IndexerBlockExplorer1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client,
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
        TransactionPage, TransactionQuery, TransactionRef as WireTransactionRef,
        TransactionRefPage as WireTransactionRefPage, TxInfoResult,
    };
    use groundhog_json_rpc_types::{
        block::consensus_value_hash,
        events::{IterationOrder, PaginationCursor},
        transaction::BlockTransactionQuery,
    };
    use groundhog_scheduler::{ConsensusValue, ProposalTxsHash};
    use groundhog_scp::network::ProtocolCommit;
    use groundhog_x23_transport::try_connect;
    use groundhog_xdr::{
        header::ProposalHeader,
        transaction::{SignedTransactionHash, SignedTransactionHash as TxHash},
        types::{
            ConsensusValueHash, EventsHash, ExtensionPoint, NetworkHash, Protocol, SHA256Hash,
            StorageShortHash, TimePoint, TxsHash,
        },
        validator::ValidatorID,
    };
    use rpc_driver::{ProgramError, RpcServer, ServiceMap};
    use tokio::{net::TcpListener, sync::RwLock};
    use xdr_common::{BytesM, VecM};

    use super::*;
    use crate::{
        config::GatewayConfig,
        context::{BackendClient, GatewayContext},
        handlers::transactions::get_transactions_by_block,
        health::GatewayHealth,
        metrics::GatewayMetrics,
    };

    enum FakeResponse {
        Block(Box<BlockLookupResult>),
        BlockRpcError,
        BlockSlow,
        Transactions,
        TransactionsRpcError,
        TransactionsSlow,
    }

    struct FakeIndexer {
        response: FakeResponse,
    }

    #[async_trait]
    impl IndexerBlockExplorer_IndexerBlockExplorer1 for FakeIndexer {
        async fn indexer_block_explorer_intro(
            &self,
            _arg0: ProtocolCommit,
        ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
            Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
        }

        async fn get_transaction_info(
            &self,
            _arg0: SignedTransactionHash,
        ) -> Result<TxInfoResult, ProgramError> {
            panic!("block handler tests should not query transaction info")
        }

        async fn get_block_by_height(
            &self,
            _height: BlockHeight,
        ) -> Result<BlockLookupResult, ProgramError> {
            match &self.response {
                FakeResponse::Block(result) => Ok((**result).clone()),
                FakeResponse::BlockRpcError => Err(ProgramError::msg("fake block lookup failure")),
                FakeResponse::BlockSlow => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    Ok(BlockLookupResult::Default {
                        outcome: BlockLookupOutcome::BlockNotFound.0,
                    })
                }
                _ => panic!("transaction handler test should not query block data"),
            }
        }

        async fn get_events(&self, _arg0: EventQuery) -> Result<EventPage, ProgramError> {
            panic!("block handler tests should not query events")
        }

        async fn get_transactions(
            &self,
            _arg0: TransactionQuery,
        ) -> Result<TransactionPage, ProgramError> {
            panic!("block handler tests should not query global transactions")
        }

        async fn get_transactions_by_address(
            &self,
            _arg0: WireAddressTransactionQuery,
        ) -> Result<WireTransactionRefPage, ProgramError> {
            panic!("block handler tests should not query transactions by address")
        }

        async fn get_transactions_by_block(
            &self,
            query: WireBlockTransactionQuery,
        ) -> Result<WireTransactionRefPage, ProgramError> {
            match self.response {
                FakeResponse::Transactions => {
                    let mut hashes = [[1; 32], [2; 32]];
                    if query.order
                        == groundhog_indexer::pagination::IterationOrder::IterationOrderDescending
                    {
                        hashes.reverse();
                    }
                    let refs = if query.cursor.0.is_empty() {
                        hashes
                            .into_iter()
                            .map(|hash| WireTransactionRef {
                                tx_hash: TxHash::new(hash),
                                height: query.height.clone(),
                            })
                            .collect()
                    } else {
                        vec![]
                    };
                    Ok(WireTransactionRefPage {
                        refs: VecM(refs),
                        cursor: if query.cursor.0.is_empty() {
                            BytesM(vec![9])
                        } else {
                            BytesM(vec![])
                        },
                        latest_block: BlockHeight::from(10),
                        oldest_block: BlockHeight::from(0),
                    })
                }
                FakeResponse::TransactionsRpcError => {
                    Err(ProgramError::msg("fake block transaction failure"))
                }
                FakeResponse::TransactionsSlow => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    Ok(WireTransactionRefPage {
                        refs: VecM(vec![]),
                        cursor: BytesM(vec![]),
                        latest_block: BlockHeight::from(10),
                        oldest_block: BlockHeight::from(0),
                    })
                }
                _ => panic!("block lookup test should not query transactions"),
            }
        }
    }

    async fn test_context(response: FakeResponse) -> GatewayContext {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test indexer should bind");
        let address = listener
            .local_addr()
            .expect("test indexer should have address");
        let mut services = ServiceMap::new();
        services.add_service(Box::new(
            IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(FakeIndexer {
                response,
            })),
        ));
        let _server = RpcServer::new(listener, services).start().await;
        let client = try_connect(
            "test indexer",
            &address.to_string(),
            1,
            IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
        )
        .await
        .expect("test indexer should connect")
        .expect("test indexer client should exist");

        GatewayContext::assemble(
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(Some(BackendClient::new(
                client.clone(),
                client,
            )))),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(GatewayConfig::default()),
            GatewayMetrics::new("test", None).expect("test metrics should register"),
        )
    }

    fn context_without_indexer() -> GatewayContext {
        GatewayContext::assemble(
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
            Arc::new(GatewayConfig::default()),
            GatewayMetrics::new("test", None).expect("test metrics should register"),
        )
    }

    async fn metrics_body(context: &GatewayContext) -> String {
        let health = GatewayHealth::new(&context.config, "test", None);
        context
            .metrics
            .gather(&health.snapshot().await)
            .expect("test metrics should encode")
    }

    fn consensus_value(height: i64) -> ConsensusValue {
        ConsensusValue {
            protocol: Protocol::new(1),
            ext: ExtensionPoint::_0,
            prev_hash: ConsensusValueHash::new(SHA256Hash::from([7; 32])),
            proposal_header: ProposalHeader {
                ext: ExtensionPoint::_0,
                network_hash: NetworkHash::new(SHA256Hash::from([1; 32])),
                proposer: ValidatorID::new([2; 32]),
                block_timepoint: TimePoint::new(123),
                block_height: BlockHeight::from(height),
                txs_hash: TxsHash::new(SHA256Hash::from([3; 32])),
                storage_hash: StorageShortHash::new(SHA256Hash::from([4; 32])),
                events_hash: EventsHash::new(SHA256Hash::from([5; 32])),
            },
            proposal_txs_hash: ProposalTxsHash::new(SHA256Hash::from([6; 32])),
            upgrades: VecM(vec![BytesM(vec![0xaa])]),
        }
    }

    #[tokio::test]
    async fn block_lookup_maps_found_and_not_found() {
        let value = consensus_value(7);
        let expected_hash = consensus_value_hash(&value).expect("test value should hash");
        let found = test_context(FakeResponse::Block(Box::new(
            BlockLookupResult::BlockFound { value },
        )))
        .await;

        let block =
            get_block_by_number(&found, RpcRequestContext::unbounded(), BlockHeight::from(7))
                .await
                .expect("found block should project")
                .expect("found block should be present");
        assert_eq!(block.block_height, BlockHeight::from(7));
        assert_eq!(block.block_hash, expected_hash);
        assert!(
            metrics_body(&found)
                .await
                .contains("backend=\"indexer\",operation=\"get_block_by_height\",outcome=\"ok\"")
        );

        let missing = test_context(FakeResponse::Block(Box::new(BlockLookupResult::Default {
            outcome: BlockLookupOutcome::BlockNotFound.0,
        })))
        .await;
        assert!(
            get_block_by_number(
                &missing,
                RpcRequestContext::unbounded(),
                BlockHeight::from(8),
            )
            .await
            .expect("ordinary miss should succeed")
            .is_none()
        );
    }

    #[tokio::test]
    async fn block_lookup_rejects_unknown_outcomes_and_rpc_failures() {
        let unknown = test_context(FakeResponse::Block(Box::new(BlockLookupResult::Default {
            outcome: 99,
        })))
        .await;
        assert!(matches!(
            get_block_by_number(
                &unknown,
                RpcRequestContext::unbounded(),
                BlockHeight::from(7),
            )
            .await,
            Err(GatewayError::BackendUnavailable(_))
        ));

        let absent = context_without_indexer();
        assert!(matches!(
            get_block_by_number(
                &absent,
                RpcRequestContext::unbounded(),
                BlockHeight::from(7),
            )
            .await,
            Err(GatewayError::BackendUnavailable(_))
        ));

        let failed = test_context(FakeResponse::BlockRpcError).await;
        assert!(matches!(
            get_block_by_number(
                &failed,
                RpcRequestContext::unbounded(),
                BlockHeight::from(7),
            )
            .await,
            Err(GatewayError::BackendUnavailable(_))
        ));
    }

    #[tokio::test]
    async fn transactions_by_block_preserves_order_cursors_and_empty_pages() {
        for order in [IterationOrder::Ascending, IterationOrder::Descending] {
            let context = test_context(FakeResponse::Transactions).await;
            let page = get_transactions_by_block(
                &context,
                RpcRequestContext::unbounded(),
                BlockTransactionQuery {
                    height: BlockHeight::from(7),
                    order,
                    cursor: PaginationCursor(vec![]),
                    limit: 2,
                },
            )
            .await
            .expect("block transaction page should succeed");
            let first = if order == IterationOrder::Ascending {
                1
            } else {
                2
            };
            assert_eq!(page.refs[0].tx_hash.0.0.into_inner(), [first; 32]);
            assert_eq!(page.cursor.0, vec![9]);
            assert_eq!(page.latest_block, BlockHeight::from(10));
            assert_eq!(page.oldest_block, BlockHeight::from(0));
            assert!(metrics_body(&context).await.contains(
                "backend=\"indexer\",operation=\"get_transactions_by_block\",outcome=\"ok\""
            ));

            let empty = get_transactions_by_block(
                &context,
                RpcRequestContext::unbounded(),
                BlockTransactionQuery {
                    height: BlockHeight::from(7),
                    order,
                    cursor: page.cursor,
                    limit: 2,
                },
            )
            .await
            .expect("exhausted page should succeed");
            assert!(empty.refs.is_empty());
            assert!(empty.cursor.0.is_empty());
        }
    }

    #[tokio::test]
    async fn transactions_by_block_does_not_convert_rpc_failure_to_empty_page() {
        let context = test_context(FakeResponse::TransactionsRpcError).await;
        let result = get_transactions_by_block(
            &context,
            RpcRequestContext::unbounded(),
            BlockTransactionQuery {
                height: BlockHeight::from(7),
                order: IterationOrder::Ascending,
                cursor: PaginationCursor(vec![]),
                limit: 2,
            },
        )
        .await;

        assert!(matches!(result, Err(GatewayError::BackendUnavailable(_))));
    }

    #[tokio::test]
    async fn block_handlers_enforce_deadlines_and_record_timeouts() {
        let block = test_context(FakeResponse::BlockSlow).await;
        let block_result = get_block_by_number(
            &block,
            RpcRequestContext::new(Some(std::time::Duration::from_millis(5))),
            BlockHeight::from(7),
        )
        .await;
        assert!(matches!(block_result, Err(GatewayError::BackendTimeout)));
        assert!(
            metrics_body(&block).await.contains(
                "backend=\"indexer\",operation=\"get_block_by_height\",outcome=\"timeout\""
            )
        );

        let transactions = test_context(FakeResponse::TransactionsSlow).await;
        let transactions_result = get_transactions_by_block(
            &transactions,
            RpcRequestContext::new(Some(std::time::Duration::from_millis(5))),
            BlockTransactionQuery {
                height: BlockHeight::from(7),
                order: IterationOrder::Ascending,
                cursor: PaginationCursor(vec![]),
                limit: 2,
            },
        )
        .await;
        assert!(matches!(
            transactions_result,
            Err(GatewayError::BackendTimeout)
        ));
        assert!(metrics_body(&transactions).await.contains(
            "backend=\"indexer\",operation=\"get_transactions_by_block\",outcome=\"timeout\""
        ));
    }
}
