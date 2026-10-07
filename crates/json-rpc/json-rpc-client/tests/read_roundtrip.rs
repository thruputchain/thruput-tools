//! Roundtrip tests for the read-path RPC methods: populated success
//! payloads served by a scripted backend must come back unchanged
//! through the generated HTTP client.

use std::sync::Arc;

use groundhog_json_rpc_api::GroundhogApiServer;
use groundhog_json_rpc_client::{GroundhogApiClient, HttpClient, HttpClientBuilder};
use groundhog_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, TipReading, ValidatedTransaction, ViewPlane,
};
use groundhog_json_rpc_types::{
    BlockHeight, ContractAddress, NetworkHash,
    address_auth::{AddressAuthDataInfo, AddressAuthDataResponse, AuthPublicKey, AuthSigner},
    block::{BlockInfo, ChainTip},
    events::{
        BlockAnchor, ContractAddressList, EmittedEvent, EventFilter, EventFilterList, EventPage,
        EventQuery, EventTopicFilter, IdentifiedEvent, IterationOrder, PaginationCursor, TopicList,
    },
    hex::{Hex0x, HexBytes, Topic},
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    state::{StorageItemInfo, StorageItemRead},
    transaction::{
        AddressTransactionQuery, TransactionRef, TransactionRefPage, TransactionStatus, TxHash,
    },
};
use jsonrpsee::server::{Server, ServerHandle};
use tokio::sync::Mutex;
use xdr_common::VecM;

const EVENT_TX_HASH: [u8; 32] = [0x21; 32];
const EVENT_CONTRACT: [u8; 32] = [0x42; 32];
const EVENT_TOPIC: [u8; 32] = [0x63; 32];
const EVENT_DATA: &[u8] = &[0xde, 0xad, 0xbe, 0xef];
const EVENT_CURSOR: &[u8] = &[0x01, 0x02, 0x03];
const TIP_HASH: [u8; 32] = [0xaa; 32];
/// The one parameter the read backend's tip config states.
const TIP_MAX_CONTRACT_SIZE_BYTES: u32 = 1_048_576;

type StorageRead = (ContractAddress, [u8; 32]);

fn test_network_info() -> NetworkInfo {
    NetworkInfo::new(NetworkHash::from(groundhog_xdr::types::SHA256Hash::from(
        [0x07; 32],
    )))
}

/// Read/chain backend returning fully-populated canned payloads and
/// recording the queries it was dispatched with.
#[derive(Clone, Default)]
struct PopulatedReadBackend {
    event_queries: Arc<Mutex<Vec<EventQuery>>>,
    address_queries: Arc<Mutex<Vec<AddressTransactionQuery>>>,
    storage_reads: Arc<Mutex<Vec<StorageRead>>>,
}

fn canned_event_page() -> EventPage {
    EventPage {
        events: vec![IdentifiedEvent {
            tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from(EVENT_TX_HASH))),
            block_height: BlockHeight::from(11),
            sequence_number: 3,
            event: EmittedEvent {
                contract_address: Hex0x(groundhog_xdr::types::ContractAddress::from(
                    EVENT_CONTRACT,
                )),
                topics: vec![Topic(EVENT_TOPIC)],
                data: HexBytes(EVENT_DATA.to_vec()),
            },
        }],
        cursor: PaginationCursor(EVENT_CURSOR.to_vec()),
        latest_block: BlockHeight::from(12),
        oldest_block: BlockHeight::from(2),
    }
}

fn canned_ref_page() -> TransactionRefPage {
    TransactionRefPage {
        refs: vec![
            TransactionRef {
                tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x51; 32]))),
                height: BlockHeight::from(5),
            },
            TransactionRef {
                tx_hash: TxHash(Hex0x(groundhog_xdr::types::SHA256Hash::from([0x52; 32]))),
                height: BlockHeight::from(6),
            },
        ],
        cursor: PaginationCursor(vec![0x0a, 0x0b]),
        latest_block: BlockHeight::from(9),
        oldest_block: BlockHeight::from(1),
    }
}

fn canned_block_info() -> BlockInfo {
    BlockInfo {
        block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x71; 32])),
        protocol: groundhog_xdr::types::Protocol::from(4),
        network_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x72; 32])),
        config_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x79; 32])),
        proposer: Hex0x(groundhog_xdr::validator::ValidatorID::from([0x73; 32])),
        block_timepoint: groundhog_xdr::types::TimePoint::from(1_700_000_000),
        block_height: BlockHeight::from(33),
        txs_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x74; 32])),
        storage_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x75; 32])),
        events_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x76; 32])),
        prev_block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x77; 32])),
        proposal_txs_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from([0x78; 32])),
        upgrades: vec![HexBytes(vec![0x01]), HexBytes(vec![0x02, 0x03])],
    }
}

#[async_trait::async_trait]
impl SubmitPlane for PopulatedReadBackend {
    async fn submit_raw_transaction(
        &self,
        _context: RpcRequestContext,
        _tx: ValidatedTransaction,
    ) -> Result<(), RpcServerError> {
        Err(RpcServerError::NotImplemented("sendRawTransaction".into()))
    }
}

#[async_trait::async_trait]
impl ChainPlane for PopulatedReadBackend {
    async fn chain_tip(&self, _context: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        Ok(TipReading::new(
            ChainTip {
                height: BlockHeight::from(42),
                block_hash: Hex0x(groundhog_xdr::types::SHA256Hash::from(TIP_HASH)),
            },
            groundhog_xdr::network_config::NetworkConfig::_0 {
                v0: groundhog_xdr::network_config::NetworkConfigV0 {
                    params: VecM(vec![
                        groundhog_xdr::network_config::ConfigParam::kConfigParamMaxContractSizeBytes {
                            max_contract_size_bytes: TIP_MAX_CONTRACT_SIZE_BYTES,
                        },
                    ]),
                },
            },
        ))
    }

    async fn get_block_by_number(
        &self,
        _context: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        if *height == 33 {
            Ok(Some(canned_block_info()))
        } else {
            Ok(None)
        }
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
impl ReadPlane for PopulatedReadBackend {
    async fn transaction_receipt(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<Option<TransactionReceipt>, RpcServerError> {
        Ok(None)
    }

    async fn transaction_status(
        &self,
        _context: RpcRequestContext,
        _tx_hash: TxHash,
    ) -> Result<TransactionStatus, RpcServerError> {
        Ok(TransactionStatus::Unknown)
    }

    async fn contract_events(
        &self,
        _context: RpcRequestContext,
        query: EventQuery,
    ) -> Result<EventPage, RpcServerError> {
        self.event_queries.lock().await.push(query);
        Ok(canned_event_page())
    }

    async fn transactions_by_address(
        &self,
        _context: RpcRequestContext,
        query: AddressTransactionQuery,
    ) -> Result<TransactionRefPage, RpcServerError> {
        self.address_queries.lock().await.push(query);
        Ok(canned_ref_page())
    }

    async fn get_storage_at(
        &self,
        _context: RpcRequestContext,
        address: ContractAddress,
        key: [u8; 32],
    ) -> Result<StorageItemRead, RpcServerError> {
        self.storage_reads.lock().await.push((address, key));
        Ok(StorageItemRead {
            block_height: BlockHeight::from(21),
            item: Some(StorageItemInfo::Bytestring {
                bytes: HexBytes(vec![0x10, 0x20, 0x30]),
            }),
        })
    }

    async fn read_native_hog_balance(
        &self,
        _context: RpcRequestContext,
        _address: ContractAddress,
    ) -> Result<StorageItemRead, RpcServerError> {
        Ok(StorageItemRead {
            block_height: BlockHeight::from(22),
            item: Some(StorageItemInfo::NonnegativeInt {
                value: "123456789012345678901234567890".into(),
            }),
        })
    }

    async fn read_address_auth_data(
        &self,
        _context: RpcRequestContext,
        address: ContractAddress,
    ) -> Result<AddressAuthDataResponse, RpcServerError> {
        Ok(AddressAuthDataResponse {
            address,
            height: BlockHeight::from(23),
            auth_data: Some(canned_auth_data()),
        })
    }
}

fn canned_auth_data() -> AddressAuthDataInfo {
    AddressAuthDataInfo {
        signers: vec![
            AuthSigner {
                key_id: 0,
                public_key: AuthPublicKey::Ed25519 {
                    key: Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([0x31; 32])),
                },
                max_weight: 1,
            },
            AuthSigner {
                key_id: 4,
                public_key: AuthPublicKey::Ed25519 {
                    key: Hex0x(groundhog_xdr::types::Ed25519PublicKey::from([0x32; 32])),
                },
                max_weight: 2,
            },
        ],
        required_weight: 3,
    }
}

#[async_trait::async_trait]
impl ViewPlane for PopulatedReadBackend {}

async fn start_server(backend: PopulatedReadBackend) -> (HttpClient, ServerHandle) {
    let server = Server::builder()
        .build("127.0.0.1:0")
        .await
        .expect("bug: test server should bind");
    let addr = server
        .local_addr()
        .expect("bug: test server local address should be available");
    let rpc = GroundhogRpcServer::unified(
        backend,
        test_network_info(),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    );
    let handle = server.start(rpc.into_rpc());
    let client = HttpClientBuilder::default()
        .build(format!("http://{addr}"))
        .expect("bug: test client should build");
    (client, handle)
}

async fn stop(handle: ServerHandle) {
    handle.stop().expect("bug: test server should stop");
    handle.stopped().await;
}

fn populated_event_query() -> EventQuery {
    EventQuery {
        filters: EventFilterList(VecM(vec![EventFilter {
            contract_addresses: ContractAddressList(VecM(vec![Hex0x(
                groundhog_xdr::types::ContractAddress::from(EVENT_CONTRACT),
            )])),
            topics: EventTopicFilter {
                topic0: TopicList(VecM(vec![Topic(EVENT_TOPIC)])),
                topic1: TopicList(VecM(vec![])),
                topic2: TopicList(VecM(vec![])),
            },
        }])),
        order: IterationOrder::Descending,
        anchor: BlockAnchor::AtHeight {
            height: BlockHeight::from(10),
        },
        cursor: PaginationCursor(vec![0x0c]),
        limit: 5,
    }
}

#[tokio::test]
async fn get_events_roundtrips_a_populated_page() {
    let backend = PopulatedReadBackend::default();
    let queries = backend.event_queries.clone();
    let (client, handle) = start_server(backend).await;

    let page = client
        .get_events(populated_event_query())
        .await
        .expect("bug: getEvents should succeed");

    assert_eq!(*queries.lock().await, vec![populated_event_query()]);
    assert_eq!(page.events.len(), 1);
    let event = &page.events[0];
    assert_eq!(
        event.tx_hash.0.0,
        groundhog_xdr::types::SHA256Hash::from(EVENT_TX_HASH)
    );
    assert_eq!(*event.block_height, 11);
    assert_eq!(event.sequence_number, 3);
    assert_eq!(
        event.event.contract_address.0,
        groundhog_xdr::types::ContractAddress::from(EVENT_CONTRACT)
    );
    assert_eq!(event.event.topics, vec![Topic(EVENT_TOPIC)]);
    assert_eq!(event.event.data.0, EVENT_DATA);
    assert_eq!(page.cursor, PaginationCursor(EVENT_CURSOR.to_vec()));
    assert_eq!(*page.latest_block, 12);
    assert_eq!(*page.oldest_block, 2);

    stop(handle).await;
}

#[tokio::test]
async fn get_transactions_by_address_roundtrips_a_populated_page() {
    let backend = PopulatedReadBackend::default();
    let queries = backend.address_queries.clone();
    let (client, handle) = start_server(backend).await;
    let query = AddressTransactionQuery {
        address: Hex0x(groundhog_xdr::types::ContractAddress::from([0x31; 32])),
        order: IterationOrder::Ascending,
        anchor: BlockAnchor::AtBeginning,
        cursor: PaginationCursor(vec![]),
        limit: 2,
    };

    let page = client
        .get_transactions_by_address(query.clone())
        .await
        .expect("bug: getTransactionsByAddress should succeed");

    assert_eq!(*queries.lock().await, vec![query]);
    let expected = canned_ref_page();
    assert_eq!(page.refs.len(), expected.refs.len());
    for (actual, expected) in page.refs.iter().zip(&expected.refs) {
        assert_eq!(actual.tx_hash, expected.tx_hash);
        assert_eq!(*actual.height, *expected.height);
    }
    assert_eq!(page.cursor, expected.cursor);
    assert_eq!(*page.latest_block, 9);
    assert_eq!(*page.oldest_block, 1);

    stop(handle).await;
}

#[tokio::test]
async fn get_storage_at_roundtrips_a_bytestring_item() {
    let backend = PopulatedReadBackend::default();
    let reads = backend.storage_reads.clone();
    let (client, handle) = start_server(backend).await;
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([0x61; 32]));
    let key = [0x62u8; 32];

    let read = client
        .get_storage_at(address, HexBytes(key.to_vec()))
        .await
        .expect("bug: getStorageAt should succeed");

    assert_eq!(*reads.lock().await, vec![(address, key)]);
    assert_eq!(*read.block_height, 21);
    match read.item {
        Some(StorageItemInfo::Bytestring { bytes }) => {
            assert_eq!(bytes.0, vec![0x10, 0x20, 0x30]);
        }
        other => panic!("expected Bytestring item, got {other:?}"),
    }

    stop(handle).await;
}

#[tokio::test]
async fn get_balance_roundtrips_a_decimal_string_balance() {
    let (client, handle) = start_server(PopulatedReadBackend::default()).await;
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([0x64; 32]));

    let balance = client
        .get_balance(address)
        .await
        .expect("bug: getBalance should succeed");

    assert_eq!(balance.address, address);
    // i128-scale value: must survive as a decimal string, not a JSON number.
    assert_eq!(balance.raw_balance, "123456789012345678901234567890");
    assert_eq!(*balance.height, 22);

    stop(handle).await;
}

#[tokio::test]
async fn get_address_auth_data_roundtrips_signers_and_threshold() {
    let (client, handle) = start_server(PopulatedReadBackend::default()).await;
    let address: ContractAddress = Hex0x(groundhog_xdr::types::ContractAddress::from([0x65; 32]));

    let read = client
        .get_address_auth_data(address)
        .await
        .expect("bug: getAddressAuthData should succeed");

    assert_eq!(read.address, address);
    assert_eq!(*read.height, 23);
    assert_eq!(read.auth_data, Some(canned_auth_data()));

    stop(handle).await;
}

#[tokio::test]
async fn chain_tip_roundtrips_height_and_hash() {
    let (client, handle) = start_server(PopulatedReadBackend::default()).await;

    let tip = client
        .chain_tip()
        .await
        .expect("bug: chainTip should succeed");

    assert_eq!(*tip.height, 42);
    assert_eq!(
        tip.block_hash.0,
        groundhog_xdr::types::SHA256Hash::from(TIP_HASH)
    );

    stop(handle).await;
}

#[tokio::test]
async fn chain_config_roundtrips_stated_parameters_only() {
    let (client, handle) = start_server(PopulatedReadBackend::default()).await;

    let config = client
        .chain_config()
        .await
        .expect("bug: chainConfig should succeed");

    assert_eq!(
        config.max_contract_size_bytes,
        Some(TIP_MAX_CONTRACT_SIZE_BYTES)
    );
    assert_eq!(
        config.max_new_contracts_per_tx, None,
        "an unstated parameter stays absent across the wire"
    );

    stop(handle).await;
}

#[tokio::test]
async fn get_block_by_number_roundtrips_a_populated_block() {
    let (client, handle) = start_server(PopulatedReadBackend::default()).await;

    let block = client
        .get_block_by_number(BlockHeight::from(33).into())
        .await
        .expect("bug: getBlockByNumber should succeed")
        .expect("bug: scripted block should be present");

    let expected = canned_block_info();
    assert_eq!(block.block_hash, expected.block_hash);
    assert_eq!(*block.protocol, *expected.protocol);
    assert_eq!(block.network_hash, expected.network_hash);
    assert_eq!(block.proposer.0.as_ref(), expected.proposer.0.as_ref());
    assert_eq!(*block.block_timepoint, *expected.block_timepoint);
    assert_eq!(*block.block_height, 33);
    assert_eq!(block.txs_hash, expected.txs_hash);
    assert_eq!(block.storage_hash, expected.storage_hash);
    assert_eq!(block.events_hash, expected.events_hash);
    assert_eq!(block.prev_block_hash, expected.prev_block_hash);
    assert_eq!(block.proposal_txs_hash, expected.proposal_txs_hash);
    assert_eq!(block.upgrades.len(), 2);
    assert_eq!(block.upgrades[0].0, vec![0x01]);
    assert_eq!(block.upgrades[1].0, vec![0x02, 0x03]);

    let missing = client
        .get_block_by_number(BlockHeight::from(34).into())
        .await
        .expect("bug: getBlockByNumber should succeed");
    assert!(missing.is_none());

    stop(handle).await;
}
