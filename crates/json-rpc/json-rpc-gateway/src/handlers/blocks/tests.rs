use std::sync::Arc;

use async_trait::async_trait;
use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use thruput_indexer::{
    AddressTransactionQuery, BlockLookupOutcome, BlockLookupResult, BlockTransactionQuery,
    EventPageResult, EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
    IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
    TransactionPageResult, TransactionQuery, TransactionRefPageResult, TxInfoResult,
};
use thruput_json_rpc_types::{ConsensusValue, events::PaginationCursor};
use thruput_x23_transport::try_connect;
use thruput_xdr::{
    PROTOCOL_VERSION,
    header::ProposalHeader,
    network::ProtocolCommit,
    transaction::TransactionHash,
    types::{ExtensionPoint, SHA256Hash},
};
use tokio::{net::TcpListener, sync::RwLock};
use xdr_common::{BytesM, VecM};

use super::*;
use crate::{config::GatewayConfig, context::BackendClient, metrics::GatewayMetrics};

struct Indexer;

#[async_trait]
impl IndexerBlockExplorer_IndexerBlockExplorer1 for Indexer {
    async fn indexer_block_explorer_intro(
        &self,
        _: ProtocolCommit,
    ) -> Result<IndexerBlockExplorerIntroOutcome, ProgramError> {
        Ok(IndexerBlockExplorerIntroOutcome::IntroSuccess)
    }

    async fn get_block_by_height(
        &self,
        height: BlockHeight,
    ) -> Result<BlockLookupResult, ProgramError> {
        match *height {
            1 | 2 => {
                let hash = SHA256Hash::from([7; 32]);
                Ok(BlockLookupResult::BlockFound {
                    value: ConsensusValue {
                        protocol: 1.into(),
                        ext: ExtensionPoint::_0,
                        prev_hash: hash.into(),
                        proposal_txs_hash: hash.into(),
                        upgrades: VecM(vec![]),
                        proposal_header: ProposalHeader {
                            ext: ExtensionPoint::_0,
                            network_hash: hash.into(),
                            config_hash: hash.into(),
                            proposer: [2; 32].into(),
                            block_timepoint: 123.into(),
                            block_height: 1.into(),
                            txs_hash: hash.into(),
                            storage_hash: hash.into(),
                            events_hash: hash.into(),
                        },
                    },
                })
            }
            3 => Ok(BlockLookupResult::Default {
                outcome: BlockLookupOutcome::BlockNotFound.0,
            }),
            4 => Ok(BlockLookupResult::Default { outcome: 999 }),
            _ => Err(ProgramError::msg("indexer read failed")),
        }
    }

    async fn get_transaction_info(&self, _: TransactionHash) -> Result<TxInfoResult, ProgramError> {
        unreachable!()
    }
    async fn get_events(&self, _: EventQuery) -> Result<EventPageResult, ProgramError> {
        unreachable!()
    }
    async fn get_transactions(
        &self,
        _: TransactionQuery,
    ) -> Result<TransactionPageResult, ProgramError> {
        unreachable!()
    }
    async fn get_transactions_by_address(
        &self,
        _: AddressTransactionQuery,
    ) -> Result<TransactionRefPageResult, ProgramError> {
        unreachable!()
    }
    async fn get_transactions_by_block(
        &self,
        query: BlockTransactionQuery,
    ) -> Result<TransactionRefPageResult, ProgramError> {
        let height = *query.height;
        if height == 5 {
            return Ok(TransactionRefPageResult::Default {
                outcome: thruput_indexer::IndexerQueryOutcome::IndexerQueryNotReady.0,
            });
        }
        if height == 6 {
            return Err(ProgramError::msg("indexer read failed"));
        }
        Ok(TransactionRefPageResult::IndexerQueryOk {
            page: thruput_indexer::TransactionRefPage {
                refs: VecM(if height == 3 {
                    vec![]
                } else {
                    vec![thruput_indexer::TransactionRef {
                        tx_hash: SHA256Hash::from([7; 32]),
                        height: 1.into(),
                    }]
                }),
                cursor: BytesM::default(),
                latest_block: 4.into(),
                oldest_block: if height == 4 { 0.into() } else { 1.into() },
            },
        })
    }
}

#[tokio::test]
async fn block_metrics_count_rejected_values_and_pages_as_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut services = ServiceMap::new();
    services.add_service(Box::new(
        IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher::new(Box::new(Indexer)),
    ));
    let server = RpcServer::new(listener, services).start().await;
    let client = try_connect(
        "indexer",
        &address.to_string(),
        1,
        IndexerBlockExplorer_IndexerBlockExplorer1_Client::new,
    )
    .await
    .unwrap()
    .unwrap();
    let ctx = GatewayContext::assemble(
        PROTOCOL_VERSION,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(Some(BackendClient::new(
            client.clone(),
            client,
        )))),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
        Arc::new(GatewayConfig::default()),
        GatewayMetrics::for_tests(),
    );
    let context = || RpcRequestContext::new(Some(std::time::Duration::from_secs(5)));
    assert!(
        get_block_by_number(&ctx, context(), 1.into())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        get_block_by_number(&ctx, context(), 3.into())
            .await
            .unwrap()
            .is_none()
    );
    for height in [2, 4, 5] {
        assert!(matches!(
            get_block_by_number(&ctx, context(), height.into()).await,
            Err(RpcServerError::BackendUnavailable(_))
        ));
    }
    for height in 1..=6 {
        let result = crate::handlers::transactions::get_transactions_by_block(
            &ctx,
            context(),
            thruput_json_rpc_types::transaction::BlockTransactionQuery {
                height: height.into(),
                order: thruput_json_rpc_types::events::IterationOrder::Ascending,
                cursor: PaginationCursor::default(),
                limit: 1,
            },
        )
        .await;
        if height == 1 || height == 3 {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(matches!(result, Err(RpcServerError::BackendUnavailable(_))));
        }
    }
    let health = crate::health::GatewayHealth::new(
        &GatewayConfig::default(),
        thruput_build_info::BuildVersion::new("test", None),
        None,
    );
    let metrics = ctx.metrics.gather(&health.snapshot().await).unwrap();
    for (outcome, count) in [("ok", 2), ("error", 3)] {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"indexer\",operation=\"get_block_by_height\",outcome=\"{outcome}\"}} {count}\n"
        )), "{metrics}");
    }
    for (outcome, count) in [("ok", 2), ("error", 4)] {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"indexer\",operation=\"get_transactions_by_block\",outcome=\"{outcome}\"}} {count}\n"
        )), "{metrics}");
    }
    server.abort();
}
