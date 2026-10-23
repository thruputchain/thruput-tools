//! Native request decoding through a minimal fixture module. Production
//! adapters are covered by `native_transport`; this does not qualify native
//! backend error mapping.
use std::sync::{Arc, Mutex};

use jsonrpsee::{
    RpcModule,
    core::{client::ClientT, traits::ToRpcParams},
    server::Server,
    types::Params,
};
use serde_json::{Value, json, value::RawValue};
use thruput_json_rpc_api::params::NamedParams;
use thruput_json_rpc_client::{HttpClient, HttpClientBuilder};
use thruput_json_rpc_server::params::{parse_empty, parse_named};
use thruput_json_rpc_types::{
    BlockHeight, NetworkHash,
    base58::Base58,
    block::BlockInfo,
    network_info::NetworkInfo,
    requests::{BlockQuery, EmptyRequest},
};
use thruput_open_rpc::{Project, ProjectInfo};
use thruput_open_rpc_macros::open_rpc;

// The documentation macro emits only metadata. Explicit adapters below own
// transport decoding; no jsonrpsee transport is generated from this declaration.
#[open_rpc]
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

fn register_native(backend: Backend, captured: Captured) -> RpcModule<Backend> {
    let mut module = RpcModule::new(backend);
    let network_capture = captured.clone();
    module
        .register_method("getNetworkInfo", move |params, _, _| {
            record(&network_capture, "getNetworkInfo", &params);
            parse_empty(&params)?;
            Ok::<_, jsonrpsee::types::ErrorObjectOwned>(network())
        })
        .unwrap();
    module
        .register_method("getBlockByHeight", move |params, backend, _| {
            record(&captured, "getBlockByHeight", &params);
            let request: BlockQuery = parse_named(&params)?;
            Ok::<_, jsonrpsee::types::ErrorObjectOwned>(backend.block_at(request.block_height))
        })
        .unwrap();
    module
}

#[derive(Clone, Default)]
struct Backend {
    heights: Arc<Mutex<Vec<i64>>>,
}

impl Backend {
    fn block_at(&self, height: BlockHeight) -> Option<BlockInfo> {
        self.heights.lock().unwrap().push(*height);
        (*height != 0).then(|| block(height))
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
async fn native_requests_roundtrip_through_named_params() {
    let backend = Backend::default();
    let captured = Captured::default();
    let module = register_native(backend.clone(), captured.clone());
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
