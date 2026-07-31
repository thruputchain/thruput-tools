use std::sync::Arc;

use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClientBuilder};
use groundhog_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, ValidatedTransaction, ViewPlane,
};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress,
    block::{BlockInfo, ChainTip},
    events::{EventPage, EventQuery, IterationOrder, PaginationCursor},
    hex::{Hex0x, HexBytes},
    receipt::TransactionReceipt,
    state::StorageItemRead,
    transaction::{
        AddressTransactionQuery, BlockTransactionQuery, TransactionRef, TransactionRefPage,
        TransactionStatus, TxHash,
    },
};
use jsonrpsee::{core::client::ClientT, rpc_params, server::Server};
use tokio::sync::Mutex;

#[derive(Clone)]
struct BlockBackend {
    block: Option<BlockInfo>,
    page: TransactionRefPage,
    block_queries: Arc<Mutex<Vec<BlockHeight>>>,
    transaction_queries: Arc<Mutex<Vec<BlockTransactionQuery>>>,
}

impl BlockBackend {
    fn new(block: Option<BlockInfo>, page: TransactionRefPage) -> Self {
        Self {
            block,
            page,
            block_queries: Arc::default(),
            transaction_queries: Arc::default(),
        }
    }
}

#[async_trait::async_trait]
impl SubmitPlane for BlockBackend {
    async fn submit_raw_transaction(
        &self,
        _context: RpcRequestContext,
        _tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        Err(RpcServerError::NotImplemented("sendRawTransaction".into()))
    }
}

#[async_trait::async_trait]
impl ChainPlane for BlockBackend {
    async fn chain_tip(&self, _context: RpcRequestContext) -> Result<ChainTip, RpcServerError> {
        Err(RpcServerError::NotImplemented("chainTip".into()))
    }

    async fn pending_transaction_count(
        &self,
        _context: RpcRequestContext,
    ) -> Result<u64, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "pendingTransactionCount".into(),
        ))
    }
}

#[async_trait::async_trait]
impl ReadPlane for BlockBackend {
    async fn get_block_by_number(
        &self,
        _context: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        self.block_queries.lock().await.push(height);
        Ok(self.block.clone())
    }

    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionReceipt".into(),
        ))
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionStatus".into(),
        ))
    }

    async fn contract_events(
        &self,
        _context: RpcRequestContext,
        _query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        Err(RpcServerError::NotImplemented("getEvents".into()))
    }

    async fn transactions_by_address(
        &self,
        _context: RpcRequestContext,
        _query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        Err(RpcServerError::NotImplemented(
            "getTransactionsByAddress".into(),
        ))
    }

    async fn transactions_by_block(
        &self,
        _context: RpcRequestContext,
        query: BlockTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        self.transaction_queries.lock().await.push(query);
        Ok(self.page.clone())
    }

    async fn get_storage_at(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
        _key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        Err(RpcServerError::NotImplemented("getStorageAt".into()))
    }

    async fn read_native_hog_balance(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        Err(RpcServerError::NotImplemented("getBalance".into()))
    }
}

#[async_trait::async_trait]
impl ViewPlane for BlockBackend {}

fn block_info(height: i64) -> BlockInfo {
    BlockInfo {
        block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x10; 32])),
        protocol: groundhog_xdr::types::Protocol::new(1),
        network_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x11; 32])),
        proposer: Hex0x(groundhog_xdr::validator::ValidatorID::new([0x12; 32])),
        block_timepoint: u64::MAX.to_string(),
        block_height: BlockHeight::from(height),
        txs_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x13; 32])),
        storage_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x14; 32])),
        events_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x15; 32])),
        prev_block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x16; 32])),
        proposal_txs_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x17; 32])),
        upgrades: vec![HexBytes(vec![0xaa])],
    }
}

fn transaction_page() -> TransactionRefPage {
    TransactionRefPage {
        refs: vec![TransactionRef {
            tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x21; 32]))),
            height: BlockHeight::from(7),
        }],
        cursor: PaginationCursor(b"next-block-page".to_vec()),
        latest_block: BlockHeight::from(10),
        oldest_block: BlockHeight::from(0),
    }
}

async fn start_server(backend: BlockBackend) -> (jsonrpsee::server::ServerHandle, String) {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        backend,
        RpcServerConfig::default(),
        BuildInfo::new("client-test", None),
    );
    (server.start(rpc.into_rpc()), format!("http://{addr}"))
}

#[tokio::test]
async fn block_found_roundtrips_with_decimal_string_timepoint() {
    let backend = BlockBackend::new(Some(block_info(7)), transaction_page());
    let queries = backend.block_queries.clone();
    let (handle, url) = start_server(backend).await;
    let client = HttpClientBuilder::default()
        .build(url)
        .expect("bug: test client should build");

    let block = client
        .get_block_by_number(BlockHeight::from(7))
        .await
        .expect("bug: block lookup should succeed")
        .expect("bug: scripted block should be present");
    let raw: serde_json::Value = client
        .request("groundhog_getBlockByNumber", rpc_params![7])
        .await
        .expect("bug: raw block lookup should succeed");

    assert_eq!(*block.block_height, 7);
    assert_eq!(block.block_timepoint, u64::MAX.to_string());
    assert_eq!(raw["blockTimepoint"], u64::MAX.to_string());
    assert!(raw["blockTimepoint"].is_string());
    assert_eq!(
        queries.lock().await.as_slice(),
        [BlockHeight::from(7), BlockHeight::from(7)]
    );

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}

#[tokio::test]
async fn block_not_found_roundtrips_as_none() {
    let backend = BlockBackend::new(None, transaction_page());
    let (handle, url) = start_server(backend).await;
    let client = HttpClientBuilder::default()
        .build(url)
        .expect("bug: test client should build");

    let block = client
        .get_block_by_number(BlockHeight::from(8))
        .await
        .expect("bug: block miss should succeed");

    assert!(block.is_none());

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}

#[tokio::test]
async fn block_transaction_page_and_query_cursor_roundtrip_unchanged() {
    let expected_page = transaction_page();
    let backend = BlockBackend::new(None, expected_page.clone());
    let queries = backend.transaction_queries.clone();
    let (handle, url) = start_server(backend).await;
    let client = HttpClientBuilder::default()
        .build(url)
        .expect("bug: test client should build");
    let query_cursor = PaginationCursor(b"opaque-request-cursor".to_vec());

    let page = client
        .get_transactions_by_block(BlockTransactionQuery {
            height: BlockHeight::from(7),
            order: IterationOrder::Descending,
            cursor: query_cursor.clone(),
            limit: 1,
        })
        .await
        .expect("bug: block transaction page should succeed");

    assert_eq!(page.refs, expected_page.refs);
    assert_eq!(page.cursor, expected_page.cursor);
    assert_eq!(*page.latest_block, 10);
    assert_eq!(*page.oldest_block, 0);
    let calls = queries.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].cursor, query_cursor);
    assert_eq!(calls[0].order, IterationOrder::Descending);

    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}
