use std::sync::Arc;

use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use thruput_indexer::{
    AddressTransactionQuery, BlockLookupOutcome, BlockLookupResult, BlockTransactionQuery,
    EventPageResult, EventQuery, IndexerBlockExplorer_IndexerBlockExplorer1,
    IndexerBlockExplorer_IndexerBlockExplorer1_Client,
    IndexerBlockExplorer_IndexerBlockExplorer1_Dispatcher, IndexerBlockExplorerIntroOutcome,
    TransactionPageResult, TransactionQuery, TransactionRefPageResult, TxInfoResult,
};
use thruput_json_rpc_types::ConsensusValue;
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

#[allow(refining_impl_trait)]
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
            6 => std::future::pending().await,
            _ => Err(ProgramError::msg("indexer read failed")),
        }
    }

    async fn get_transaction_info(&self, _: TransactionHash) -> Result<TxInfoResult, ProgramError> {
        unreachable!()
    }
    async fn get_events(&self, query: EventQuery) -> Result<EventPageResult, ProgramError> {
        use thruput_indexer::IndexerQueryOutcome as Outcome;
        if query.limit >= 10 {
            let page = history_page(query.limit);
            return Ok(EventPageResult::IndexerQueryOk {
                page: thruput_indexer::EventPage {
                    events: VecM(
                        page.refs
                            .0
                            .into_iter()
                            .map(|item| thruput_xdr::event::IdentifiedEvent {
                                tx_hash: item.tx_hash,
                                block_height: item.height,
                                sequence_number: 0,
                                emitted_event: thruput_xdr::event::TxEmittedEvent {
                                    contract_address: [9; 32].into(),
                                    num_topics: 0,
                                    serialized_event: BytesM::default(),
                                },
                            })
                            .collect(),
                    ),
                    cursor: page.cursor,
                    latest_block: page.latest_block,
                    oldest_block: page.oldest_block,
                },
            });
        }
        match query.limit {
            1 => Ok(EventPageResult::Default {
                outcome: Outcome::IndexerQueryInvalidCursor.0,
            }),
            2 => Ok(EventPageResult::Default {
                outcome: Outcome::IndexerQueryHistoryUnavailable.0,
            }),
            3 => std::future::pending().await,
            _ => Err(ProgramError::msg("history read failed")),
        }
    }
    async fn get_transactions(
        &self,
        _: TransactionQuery,
    ) -> Result<TransactionPageResult, ProgramError> {
        unreachable!()
    }
    async fn get_transactions_by_address(
        &self,
        query: AddressTransactionQuery,
    ) -> Result<thruput_indexer::AddressTransactionRefPageResult, ProgramError> {
        use thruput_indexer::IndexerQueryOutcome as Outcome;
        use thruput_indexer::{
            AddressTransactionRef, AddressTransactionRefPage, AddressTransactionRefPageResult,
        };
        if query.limit >= 10 {
            let page = history_page(query.limit);
            return Ok(AddressTransactionRefPageResult::IndexerQueryOk {
                page: AddressTransactionRefPage {
                    refs: VecM(
                        page.refs
                            .0
                            .into_iter()
                            .map(|item| AddressTransactionRef {
                                tx_hash: item.tx_hash,
                                height: item.height,
                                is_root_target: true,
                                was_invoked: true,
                            })
                            .collect(),
                    ),
                    cursor: page.cursor,
                    oldest_block: page.oldest_block,
                    latest_block: page.latest_block,
                },
            });
        }
        Ok(AddressTransactionRefPageResult::Default {
            outcome: if query.limit == 1 {
                Outcome::IndexerQueryInvalidCursor.0
            } else {
                Outcome::IndexerQueryHistoryUnavailable.0
            },
        })
    }
    async fn get_transactions_by_block(
        &self,
        query: BlockTransactionQuery,
    ) -> Result<TransactionRefPageResult, ProgramError> {
        use thruput_indexer::IndexerQueryOutcome as Outcome;
        if query.limit >= 10 {
            let mut page = history_page(query.limit);
            if query.limit == 11 {
                // Within indexed coverage, but not the requested block.
                page.refs.0[0].height = 2.into();
            }
            return Ok(TransactionRefPageResult::IndexerQueryOk { page });
        }
        Ok(TransactionRefPageResult::Default {
            outcome: match *query.height {
                7 => Outcome::IndexerQueryInvalidCursor.0,
                8 => Outcome::IndexerQueryHistoryUnavailable.0,
                height => unreachable!("no test pages block {height}"),
            },
        })
    }
}

// All pages decode successfully. Their query-dependent defects must be counted
// as backend errors before the shared service can reject them a second time.
fn history_page(case: u32) -> thruput_indexer::TransactionRefPage {
    let hashes = match case {
        13 => (0..14).collect(), // More rows than the requested limit.
        14 => vec![3, 1, 2],     // Neither ascending nor descending.
        _ => vec![7],
    };
    thruput_indexer::TransactionRefPage {
        refs: VecM(
            hashes
                .into_iter()
                .map(|byte| thruput_indexer::TransactionRef {
                    tx_hash: SHA256Hash::from([byte; 32]),
                    height: if case == 11 { 5.into() } else { 1.into() },
                })
                .collect(),
        ),
        cursor: BytesM(if case == 12 { vec![1, 2, 3] } else { vec![] }),
        latest_block: 4.into(),
        oldest_block: 1.into(),
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
    let ctx = Arc::new(GatewayContext::assemble(
        PROTOCOL_VERSION,
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(Some(BackendClient::new(
            client.clone(),
            client,
        )))),
        Arc::new(RwLock::new(None)),
        Arc::new(RwLock::new(None)),
        Arc::new(GatewayConfig {
            request_timeout_ms: 1_000,
            ..GatewayConfig::default()
        }),
        GatewayMetrics::for_tests(),
    ));
    let health = crate::health::GatewayHealth::new(
        &GatewayConfig::default(),
        thruput_build_info::BuildVersion::new("test", None),
        None,
    );
    check_native_blocks(&ctx, &health).await;
    check_native_history(&ctx, &health).await;
    check_native_history_page_metrics(&ctx, &health).await;
    server.abort();
}

async fn check_native_blocks(ctx: &Arc<GatewayContext>, health: &crate::health::GatewayHealth) {
    use thruput_json_rpc_server::native::read::NativeBlockPlane;
    let context = || RpcRequestContext::new(Some(std::time::Duration::from_secs(5)));
    let backend = crate::backend::GatewayBackend::new(ctx.clone());
    assert!(
        backend
            .native_block(context(), 1.into())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        backend
            .native_block(context(), 3.into())
            .await
            .unwrap()
            .is_none()
    );
    for height in [2, 4] {
        assert!(matches!(
            backend.native_block(context(), height.into()).await,
            Err(RpcServerError::BackendDataInvalid { .. })
        ));
    }
    assert!(matches!(
        backend.native_block(context(), 5.into()).await,
        Err(RpcServerError::BackendUnavailable(_))
    ));
    let metrics = ctx.metrics.gather(&health.snapshot().await).unwrap();
    for (outcome, count) in [("ok", 2), ("error", 3)] {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"indexer\",operation=\"get_block_by_height\",outcome=\"{outcome}\"}} {count}\n"
        )), "{metrics}");
    }
    let deadline = RpcRequestContext::new(Some(std::time::Duration::from_millis(20)));
    assert!(matches!(
        backend.native_block(deadline, 6.into()).await,
        Err(RpcServerError::BackendTimeout)
    ));
}

async fn check_native_history(ctx: &Arc<GatewayContext>, health: &crate::health::GatewayHealth) {
    use thruput_json_rpc_server::native::read::NativeHistoryPlane;
    use thruput_json_rpc_types::{hex::HexBytes, requests as input};
    let backend = crate::backend::GatewayBackend::new(ctx.clone());
    let event = |limit| input::EventQuery {
        filters: vec![],
        start: None,
        cursor: Some(HexBytes(vec![1, 2, 3])),
        limit: Some(limit),
    };
    let root = |limit| input::AddressHistoryQuery {
        address: thruput_xdr::types::ContractAddress::from([9; 32]).into(),
        start: None,
        cursor: Some(HexBytes(vec![1, 2, 3])),
        limit: Some(limit),
    };
    let block = |height: i64| input::BlockPageRequest {
        block_height: height.into(),
        start: None,
        cursor: Some(HexBytes(vec![1, 2, 3])),
        limit: Some(1),
    };
    let context = || RpcRequestContext::new(Some(std::time::Duration::from_secs(5)));
    for error in [
        backend
            .native_events(context(), event(1))
            .await
            .unwrap_err(),
        backend
            .native_address_transactions(context(), root(1))
            .await
            .unwrap_err(),
        backend
            .native_block_transactions(context(), block(7))
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, RpcServerError::InvalidCursor(_)),
            "{error:?}"
        );
    }
    for error in [
        backend
            .native_events(context(), event(2))
            .await
            .unwrap_err(),
        backend
            .native_address_transactions(context(), root(2))
            .await
            .unwrap_err(),
        backend
            .native_block_transactions(context(), block(8))
            .await
            .unwrap_err(),
    ] {
        assert!(
            matches!(error, RpcServerError::HistoryUnavailable(_)),
            "{error:?}"
        );
    }
    assert!(matches!(
        backend.native_events(context(), event(4)).await,
        Err(RpcServerError::BackendUnavailable(_))
    ));
    // Let the configured backend deadline expire before the outer request
    // deadline so this verifies both the public error and timeout metric.
    assert!(matches!(
        backend.native_events(context(), event(3)).await,
        Err(RpcServerError::BackendTimeout)
    ));
    let metrics = ctx.metrics.gather(&health.snapshot().await).unwrap();
    for (operation, outcome, count) in [
        ("get_events", "error", 3),
        ("get_events", "timeout", 1),
        ("get_transactions_by_address", "error", 2),
        ("get_transactions_by_block", "error", 2),
    ] {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"indexer\",operation=\"{operation}\",outcome=\"{outcome}\"}} {count}\n"
        )), "{metrics}");
    }
}

async fn check_native_history_page_metrics(
    ctx: &Arc<GatewayContext>,
    health: &crate::health::GatewayHealth,
) {
    use thruput_json_rpc_server::native::read::NativeHistoryPlane;
    use thruput_json_rpc_types::{hex::HexBytes, requests as input};
    let backend = crate::backend::GatewayBackend::new(ctx.clone());
    let context = || RpcRequestContext::new(Some(std::time::Duration::from_secs(5)));
    for case in 10..=14 {
        let cursor = Some(HexBytes(vec![1, 2, 3]));
        let limit = Some(case);
        let address = thruput_xdr::types::ContractAddress::from([9; 32]).into();
        let event = input::EventQuery {
            filters: vec![input::EventFilter {
                addresses: vec![address],
                topics: None,
            }],
            start: None,
            cursor: cursor.clone(),
            limit,
        };
        let root = input::AddressHistoryQuery {
            address,
            start: None,
            cursor: cursor.clone(),
            limit,
        };
        let block = input::BlockPageRequest {
            block_height: 1.into(),
            start: None,
            cursor,
            limit,
        };
        for (method, result) in [
            (
                "events",
                backend.native_events(context(), event).await.map(|_| ()),
            ),
            (
                "root",
                backend
                    .native_address_transactions(context(), root)
                    .await
                    .map(|_| ()),
            ),
            (
                "block",
                backend
                    .native_block_transactions(context(), block)
                    .await
                    .map(|_| ()),
            ),
        ] {
            if case == 10 {
                assert!(result.is_ok(), "{method} valid page: {result:?}");
            } else {
                assert!(
                    matches!(result, Err(RpcServerError::BackendDataInvalid { .. })),
                    "{method} invalid page case {case}: {result:?}"
                );
            }
        }
    }
    let metrics = ctx.metrics.gather(&health.snapshot().await).unwrap();
    // Include the typed-outcome calls made earlier using this context.
    for (operation, outcome, count) in [
        ("get_events", "ok", 1),
        ("get_events", "error", 7),
        ("get_transactions_by_address", "ok", 1),
        ("get_transactions_by_address", "error", 6),
        ("get_transactions_by_block", "ok", 1),
        ("get_transactions_by_block", "error", 6),
    ] {
        assert!(metrics.contains(&format!(
            "groundhog_gateway_backend_requests_total{{backend=\"indexer\",operation=\"{operation}\",outcome=\"{outcome}\"}} {count}\n"
        )), "{metrics}");
    }
}
