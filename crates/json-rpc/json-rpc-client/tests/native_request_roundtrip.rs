//! Native names are registered only in this fixture. Production still serves
//! the complete current wire; this does not qualify native backend error mapping.
use std::sync::{Arc, Mutex};

use jsonrpsee::{
    RpcModule,
    core::{client::ClientT, traits::ToRpcParams},
    server::Server,
    types::Params,
};
use serde_json::{Value, json, value::RawValue};
use thruput_json_rpc_api::{GroundhogApiServer, params::NamedParams};
use thruput_json_rpc_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_server::{
    BuildInfo, ChainPlane, GroundhogRpcServer, ReadPlane, RpcRequestContext, RpcServerConfig,
    RpcServerError, SubmitPlane, TipReading, ValidatedTransaction, ViewPlane,
    params::{parse_empty, parse_named},
};
use thruput_json_rpc_types::{
    BlockHeight, NetworkHash,
    base58::Base58,
    block::BlockInfo,
    decimal::Decimal,
    events::{EventPage, EventQuery},
    network_info::NetworkInfo,
    receipt::TransactionReceipt,
    requests::{BlockQuery, EmptyRequest},
    transaction::{AddressTransactionQuery, TransactionRefPage, TransactionStatus, TxHash},
};
use thruput_open_rpc::{Project, ProjectInfo};
use thruput_open_rpc_macros::open_rpc;

// Object-mode documentation emits only metadata. Explicit adapters below own
// transport decoding; no jsonrpsee transport is generated from this declaration.
#[open_rpc(params = "object")]
trait NativeFixture {
    /// Network identity. Noarg also accepts omission, null and empty arrays.
    #[method(name = "getNetworkInfo")]
    async fn network(&self, request: EmptyRequest) -> jsonrpsee::core::RpcResult<NetworkInfo>;
    #[method(name = "getBlockByHeight")]
    async fn block(&self, request: BlockQuery) -> jsonrpsee::core::RpcResult<Option<BlockInfo>>;
}

#[path = "support/schema.rs"]
mod schema;

#[path = "native_request_roundtrip/contracts.rs"]
mod contracts;

fn spec() -> Value {
    let mut project = Project::new(ProjectInfo {
        title: "Native request fixture (not a serving API)".into(),
        version: "14.0.0".into(),
    });
    project.module(NativeFixtureOpenRpc::module_doc);
    project.to_json()
}

type Captured = Arc<Mutex<Vec<(String, Option<String>)>>>;
fn record(captured: &Captured, method: &str, params: &Params<'_>) {
    captured
        .lock()
        .unwrap()
        .push((method.into(), params.as_str().map(str::to_owned)));
}

fn register_native<T: GroundhogApiServer>(server: T, captured: Captured) -> RpcModule<T> {
    let mut module = RpcModule::new(server);
    let network_capture = captured.clone();
    module
        .register_async_method("getNetworkInfo", move |params, server, _| {
            record(&network_capture, "getNetworkInfo", &params);
            async move {
                parse_empty(&params)?;
                server.network_info().await
            }
        })
        .unwrap();
    module
        .register_async_method("getBlockByHeight", move |params, server, _| {
            record(&captured, "getBlockByHeight", &params);
            async move {
                let request: BlockQuery = parse_named(&params)?;
                server
                    .get_block_by_number(Decimal(request.block_height))
                    .await
            }
        })
        .unwrap();
    module
}

#[derive(Clone, Default)]
struct Backend {
    heights: Arc<Mutex<Vec<i64>>>,
}
struct Unused;

// Any call to an unrelated plane is a fixture failure, not a canned success.
macro_rules! unused_planes {
    ($ty:ty) => {
        #[async_trait::async_trait]
        impl SubmitPlane for $ty {
            async fn submit_raw_transaction(
                &self,
                _: RpcRequestContext,
                _: ValidatedTransaction,
            ) -> Result<(), RpcServerError> {
                panic!("unexpected submit")
            }
        }
        #[async_trait::async_trait]
        impl ReadPlane for $ty {
            async fn get_storage_at(
                &self,
                _: RpcRequestContext,
                _: thruput_json_rpc_types::ContractAddress,
                _: [u8; 32],
            ) -> Result<thruput_json_rpc_types::state::StorageItemRead, RpcServerError> {
                panic!("unexpected storage")
            }
            async fn read_native_hog_balance(
                &self,
                _: RpcRequestContext,
                _: thruput_json_rpc_types::ContractAddress,
            ) -> Result<thruput_json_rpc_types::state::StorageItemRead, RpcServerError> {
                panic!("unexpected balance")
            }
            async fn read_address_auth_data(
                &self,
                _: RpcRequestContext,
                _: thruput_json_rpc_types::ContractAddress,
            ) -> Result<thruput_json_rpc_types::address_auth::AddressAuthDataResponse, RpcServerError> {
                panic!("unexpected auth")
            }
            async fn transaction_receipt(
                &self,
                _: RpcRequestContext,
                _: TxHash,
            ) -> Result<Option<TransactionReceipt>, RpcServerError> {
                panic!("unexpected receipt")
            }
            async fn transaction_status(
                &self,
                _: RpcRequestContext,
                _: TxHash,
            ) -> Result<TransactionStatus, RpcServerError> {
                panic!("unexpected status")
            }
            async fn contract_events(
                &self,
                _: RpcRequestContext,
                _: EventQuery,
            ) -> Result<EventPage, RpcServerError> {
                panic!("unexpected events")
            }
            async fn transactions_by_address(
                &self,
                _: RpcRequestContext,
                _: AddressTransactionQuery,
            ) -> Result<TransactionRefPage, RpcServerError> {
                panic!("unexpected history")
            }
        }
        #[async_trait::async_trait]
        impl ViewPlane for $ty {}
    };
}
unused_planes!(Backend);
unused_planes!(Unused);

#[async_trait::async_trait]
impl ChainPlane for Backend {
    async fn chain_tip(&self, _: RpcRequestContext) -> Result<TipReading, RpcServerError> {
        panic!("unexpected tip/cache access")
    }
    async fn pending_transaction_count(&self, _: RpcRequestContext) -> Result<u64, RpcServerError> {
        panic!("unexpected pending count")
    }
    async fn get_block_by_number(
        &self,
        _: RpcRequestContext,
        height: BlockHeight,
    ) -> Result<Option<BlockInfo>, RpcServerError> {
        self.heights.lock().unwrap().push(*height);
        if *height == 0 {
            return Ok(None);
        }
        Ok(Some(block(height)))
    }
}

fn network() -> NetworkInfo {
    NetworkInfo::new(NetworkHash::from(thruput_xdr::types::SHA256Hash::from(
        [7; 32],
    )))
}
fn block(height: BlockHeight) -> BlockInfo {
    let hash = || Base58(thruput_xdr::types::SHA256Hash::from([0x71; 32]));
    BlockInfo {
        block_hash: hash(),
        protocol: 4.into(),
        network_hash: hash(),
        config_hash: hash(),
        proposer: Base58(thruput_xdr::validator::ValidatorID::from([0x73; 32])),
        block_timepoint: 1_700_000_000_000_000_000u64.into(),
        block_height: height,
        txs_hash: hash(),
        storage_hash: hash(),
        events_hash: hash(),
        prev_block_hash: hash(),
        proposal_txs_hash: hash(),
        upgrades: vec![thruput_json_rpc_types::hex::HexBytes(vec![1, 2])],
    }
}

// Intentionally bypass the normal serializer to test malformed client requests.
struct RawParams<'a>(Option<&'a str>);
impl ToRpcParams for RawParams<'_> {
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
        self.0.map(|s| RawValue::from_string(s.into())).transpose()
    }
}

async fn get_network(client: &HttpClient) -> Result<NetworkInfo, jsonrpsee::core::client::Error> {
    client
        .request("getNetworkInfo", NamedParams(EmptyRequest {}))
        .await
}
async fn get_block(
    client: &HttpClient,
    height: i64,
) -> Result<Option<BlockInfo>, jsonrpsee::core::client::Error> {
    client
        .request(
            "getBlockByHeight",
            NamedParams(BlockQuery {
                block_height: height.into(),
            }),
        )
        .await
}

const BAD_BLOCK_PARAMS: &[Option<&str>] = &[
    None,
    Some("null"),
    Some("[]"),
    Some("[\"812\"]"),
    Some("[{\"blockHeight\":\"812\"}]"),
    Some("{}"),
    Some("{\"request\":{\"blockHeight\":\"812\"}}"),
    Some("{\"blockHeight\":null}"),
    Some("{\"blockHeight\":812}"),
    Some("{\"blockHeight\":\"-1\"}"),
    Some("{\"blockHeight\":\"01\"}"),
    Some("{\"blockHeight\":\"+1\"}"),
    Some("{\"blockHeight\":\"1.0\"}"),
    Some("{\"blockHeight\":\"9223372036854775808\"}"),
    Some("{\"blockHeight\":\"1\",\"extra\":true}"),
    Some("{\"block_height\":\"1\"}"),
];

#[tokio::test]
async fn native_requests_roundtrip_for_unified_and_split_backends() {
    for split in [false, true] {
        let backend = Backend::default();
        let captured = Captured::default();
        let module = if split {
            register_native(
                GroundhogRpcServer::new(
                    Unused,
                    backend.clone(),
                    Unused,
                    Unused,
                    network(),
                    RpcServerConfig::default().with_tip_refresh_interval_ms(0),
                    BuildInfo::for_tests(),
                ),
                captured.clone(),
            )
            .remove_context()
        } else {
            register_native(
                GroundhogRpcServer::unified(
                    backend.clone(),
                    network(),
                    RpcServerConfig::default().with_tip_refresh_interval_ms(0),
                    BuildInfo::for_tests(),
                ),
                captured.clone(),
            )
            .remove_context()
        };
        let mut names = module.method_names().collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, ["getBlockByHeight", "getNetworkInfo"]);
        let server = Server::builder().build("127.0.0.1:0").await.unwrap();
        let client = HttpClientBuilder::default()
            .build(format!("http://{}", server.local_addr().unwrap()))
            .unwrap();
        let handle = server.start(module);
        assert_eq!(get_network(&client).await.unwrap(), network());
        let value = get_block(&client, 812).await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::to_value(block(812.into())).unwrap()
        );
        assert!(get_block(&client, 0).await.unwrap().is_none());
        for height in [9_007_199_254_740_993, i64::MAX] {
            assert_eq!(
                *get_block(&client, height)
                    .await
                    .unwrap()
                    .unwrap()
                    .block_height,
                height
            );
        }
        assert_eq!(
            captured.lock().unwrap()[0],
            ("getNetworkInfo".into(), Some("{}".into()))
        );
        assert_eq!(
            captured.lock().unwrap()[1],
            (
                "getBlockByHeight".into(),
                Some(r#"{"blockHeight":"812"}"#.into())
            )
        );
        let calls = backend.heights.lock().unwrap().clone();
        for &params in BAD_BLOCK_PARAMS {
            let error = client
                .request::<Value, _>("getBlockByHeight", RawParams(params))
                .await
                .unwrap_err();
            let jsonrpsee::core::client::Error::Call(error) = error else {
                panic!("expected RPC error: {error}")
            };
            assert_eq!(error.code(), -32602, "{params:?}");
        }
        assert_eq!(
            *backend.heights.lock().unwrap(),
            calls,
            "invalid requests dispatched"
        );
        check_noarg_normalization(&client).await;
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

async fn check_noarg_normalization(client: &HttpClient) {
    for params in [None, Some("null"), Some("[]"), Some("[ ]"), Some("{}")] {
        assert_eq!(
            client
                .request::<NetworkInfo, _>("getNetworkInfo", RawParams(params))
                .await
                .unwrap(),
            network()
        );
    }
    for params in ["{\"extra\":true}", "[1]", "[{}]"] {
        let error = client
            .request::<Value, _>("getNetworkInfo", RawParams(Some(params)))
            .await
            .unwrap_err();
        let jsonrpsee::core::client::Error::Call(error) = error else {
            panic!("{error}")
        };
        assert_eq!(error.code(), -32602);
    }
}

#[test]
fn named_client_adapter_rejects_non_objects() {
    for value in [json!(null), json!([]), json!("812"), json!(812)] {
        assert!(NamedParams(value).to_rpc_params().is_err());
    }
}

struct OfflineMeta;
impl jsonschema::Retrieve for OfflineMeta {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        if uri.as_str().trim_end_matches('/') == "https://meta.json-schema.tools" {
            Ok(serde_json::from_str(include_str!(
                "fixtures/json-schema-tools-meta.json"
            ))?)
        } else {
            Err(format!("unregistered offline schema: {uri}").into())
        }
    }
}

#[test]
fn generated_named_contract_passes_openrpc_meta_schema() {
    let meta: Value =
        serde_json::from_str(include_str!("fixtures/openrpc-meta-1.14.9.json")).unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft7)
        .with_retriever(OfflineMeta)
        .build(&meta)
        .unwrap();
    let generated = spec();
    let errors: Vec<_> = validator
        .iter_errors(&generated)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    // Prove this is validating method structure, not accepting any JSON.
    let mut broken = generated.clone();
    broken["methods"][0]["paramStructure"] = json!("positional");
    assert!(!validator.is_valid(&broken));
    for method in generated["methods"].as_array().unwrap() {
        assert_eq!(method["paramStructure"], "by-name");
        assert_eq!(method["x-params-schema"]["additionalProperties"], false);
    }
    let method = &generated["methods"][1];
    assert_eq!(method["name"], "getBlockByHeight");
    assert_eq!(method["params"][0]["name"], "blockHeight");
    assert_eq!(method["params"][0]["required"], true);
    assert_eq!(generated["methods"][0]["params"], json!([]));
}

#[test]
fn schema_and_runtime_agree_on_block_request_corpus() {
    let generated = spec();
    let schema = &generated["methods"][1]["x-params-schema"];
    let validator = jsonschema::validator_for(schema).unwrap();
    // The shared decimal schema declares exact string bounds as extensions;
    // ordinary JSON Schema numeric minimum/maximum do not apply to strings.
    let height_schema = &schema["properties"]["blockHeight"];
    let min = height_schema["x-integer-minimum"]
        .as_str()
        .unwrap()
        .parse::<i128>()
        .unwrap();
    let max = height_schema["x-integer-maximum"]
        .as_str()
        .unwrap()
        .parse::<i128>()
        .unwrap();
    assert_eq!((min, max), (0, i128::from(i64::MAX)));
    let valid = |value: &Value| {
        validator.is_valid(value)
            && value["blockHeight"]
                .as_str()
                .and_then(|s| s.parse::<i128>().ok())
                .is_some_and(|n| (min..=max).contains(&n))
    };
    for raw in BAD_BLOCK_PARAMS.iter().copied().chain([
        Some("{\"blockHeight\":\"0\"}"),
        Some("{\"blockHeight\":\"812\"}"),
        Some("{\"blockHeight\":\"9007199254740993\"}"),
        Some("{\"blockHeight\":\"9223372036854775807\"}"),
    ]) {
        let value: Value = serde_json::from_str(raw.unwrap_or("null")).unwrap();
        assert_eq!(
            valid(&value),
            parse_named::<BlockQuery>(&Params::new(raw)).is_ok(),
            "{raw:?}"
        );
    }
}

#[test]
fn production_registration_has_no_native_fixture_names() {
    let module = GroundhogRpcServer::unified(
        Backend::default(),
        network(),
        RpcServerConfig::default(),
        BuildInfo::for_tests(),
    )
    .into_rpc();
    assert!(
        module
            .method_names()
            .all(|name| name != "getNetworkInfo" && name != "getBlockByHeight")
    );
    assert_eq!(thruput_json_rpc_api::WIRE_FORMAT_VERSION, "14.1.0");
}
