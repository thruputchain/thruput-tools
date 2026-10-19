//! The reusable native adapters over real HTTP, with typed fake handlers. This
//! qualifies transport contracts, not backend semantics or serving activation.
use std::sync::{Arc, Mutex};

use jsonrpsee::{
    RpcModule,
    core::{client::ClientT, traits::ToRpcParams},
    server::{Server, ServerHandle},
    types::ErrorObjectOwned,
};
use serde_json::{Value, json, value::RawValue};
use thruput_json_rpc_api::native::{METHOD_NAMES, NativeApi};
use thruput_json_rpc_client::{
    DecodedError, DecodedResponse, HttpClient, HttpClientBuilder,
    native::{NativeClient, NativeClientError},
};
use thruput_json_rpc_server::native::NativeModule;
use thruput_json_rpc_types::requests::{AddressQuery, EmptyRequest, TransactionQuery};

#[path = "native_transport/fixtures.rs"]
mod fixtures;

#[derive(Clone, Default)]
struct Service {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    error: Option<ErrorObjectOwned>,
}

macro_rules! implement_service {
    ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
        impl NativeApi for Service {
            $(async fn $name(&self, request: $request) -> jsonrpsee::core::RpcResult<$response> {
                self.calls.lock().unwrap().push(($wire.into(), serde_json::to_value(request).unwrap()));
                if let Some(error) = &self.error { return Err(error.clone()); }
                Ok(serde_json::from_value(fixtures::response($wire)).unwrap())
            })*
        }
    }
}
thruput_json_rpc_api::native_methods!(implement_service);

async fn start<T: Send + Sync + 'static>(module: RpcModule<T>) -> (HttpClient, ServerHandle) {
    let server = Server::builder().build("127.0.0.1:0").await.unwrap();
    let client = HttpClientBuilder::default()
        .build(format!("http://{}", server.local_addr().unwrap()))
        .unwrap();
    (client, server.start(module))
}

fn sorted_methods() -> Vec<String> {
    let mut names: Vec<_> = METHOD_NAMES.iter().map(|name| (*name).to_owned()).collect();
    names.sort();
    names
}

struct RawParams(Option<String>);
impl ToRpcParams for RawParams {
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
        self.0.map(RawValue::from_string).transpose()
    }
}

fn assert_code(error: jsonrpsee::core::client::Error, expected: i32) {
    let jsonrpsee::core::client::Error::Call(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.code(), expected);
}

macro_rules! exercise_clients {
    ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
        async fn exercise_clients(client: &NativeClient<HttpClient>, service: &Service) {
            $(
                let request = fixtures::request($wire);
                let response = client.$name(serde_json::from_value::<$request>(request.clone()).unwrap()).await.unwrap();
                let DecodedResponse::Known(response) = response else { panic!("unexpected unknown result: {}", $wire); };
                let mut expected = fixtures::response($wire);
                if $wire == "getVersionInfo" { expected["supportedMethods"] = json!(sorted_methods()); }
                assert_eq!(serde_json::to_value(response).unwrap(), expected, "{} result", $wire);
                assert_eq!(service.calls.lock().unwrap().last().unwrap(), &($wire.into(), request));
            )*
        }
    }
}
thruput_json_rpc_api::native_methods!(exercise_clients);

#[tokio::test]
async fn all_twenty_typed_methods_roundtrip_through_actual_adapters() {
    assert_eq!(METHOD_NAMES.len(), 20);
    let service = Service::default();
    let module = NativeModule::new(service.clone()).unwrap().finalize();
    assert_eq!(module.supported_methods(), sorted_methods());
    let (http, handle) = start(module.into_rpc()).await;
    let client = NativeClient::new(http.clone());
    exercise_clients(&client, &service).await;
    assert_eq!(service.calls.lock().unwrap().len(), 20);
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test]
async fn strict_params_fail_before_any_handler_and_noarg_forms_normalize() {
    let service = Service::default();
    let (http, handle) = start(
        NativeModule::new(service.clone())
            .unwrap()
            .finalize()
            .into_rpc(),
    )
    .await;
    for method in METHOD_NAMES {
        let valid = fixtures::request(method);
        let mut extra = valid.clone();
        extra["extra"] = json!(true);
        let mut invalid = vec![extra, json!({"request": valid}), json!([valid])];
        if valid != json!({}) {
            let before = service.calls.lock().unwrap().len();
            assert_code(
                http.request::<Value, _>(method, RawParams(None))
                    .await
                    .unwrap_err(),
                -32602,
            );
            assert_eq!(service.calls.lock().unwrap().len(), before);
            invalid.extend([json!({}), json!([]), Value::Null]);
        }
        for invalid in invalid {
            let before = service.calls.lock().unwrap().len();
            assert_code(
                http.request::<Value, _>(method, RawParams(Some(invalid.to_string())))
                    .await
                    .unwrap_err(),
                -32602,
            );
            assert_eq!(
                service.calls.lock().unwrap().len(),
                before,
                "{method}: {invalid}"
            );
        }
        if valid == json!({}) {
            for params in [None, Some("{}"), Some("[]"), Some("null")] {
                let _: Value = http
                    .request(method, RawParams(params.map(str::to_owned)))
                    .await
                    .unwrap();
                assert_eq!(
                    service.calls.lock().unwrap().last().unwrap(),
                    &(method.to_string(), json!({}))
                );
            }
        }
    }
    // Nested object strictness and canonical decimal syntax also cross HTTP.
    let mut nested = fixtures::request("getEvents");
    nested["start"]["anchor"]["extra"] = json!(true);
    for (method, params) in [
        ("getEvents", nested),
        ("getBlockByHeight", json!({"blockHeight": 812})),
        ("dev_fundAccount", {
            let mut p = fixtures::request("dev_fundAccount");
            p["rawAmount"] = json!("01");
            p
        }),
    ] {
        assert_code(
            http.request::<Value, _>(method, RawParams(Some(params.to_string())))
                .await
                .unwrap_err(),
            -32602,
        );
    }
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test]
async fn configured_subset_is_frozen_into_discovery_and_removed_methods_are_absent() {
    let service = Service::default();
    let mut module = NativeModule::new(service.clone()).unwrap();
    for name in ["getEvents", "faucet_requestAirdrop", "dev_fundAccount"] {
        assert!(module.remove_method(name));
        assert!(!module.remove_method(name));
    }
    let module = module.finalize();
    let expected = module.supported_methods().to_vec();
    assert_eq!(expected.len(), 17);
    let (http, handle) = start(module.into_rpc()).await;
    let client = NativeClient::new(http.clone());
    let DecodedResponse::Known(version) = client.get_version_info(EmptyRequest {}).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(version.supported_methods, expected);
    for method in ["getEvents", "faucet_requestAirdrop", "dev_fundAccount"] {
        assert_code(
            http.request::<Value, _>(
                method,
                RawParams(Some(fixtures::request(method).to_string())),
            )
            .await
            .unwrap_err(),
            -32601,
        );
    }
    assert_eq!(service.calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
    handle.stopped().await;
}

#[tokio::test]
async fn application_errors_keep_method_specific_classification_and_raw_evidence() {
    for (data, classified) in [
        (
            json!({"kind": "backendUnavailable", "futureDetail": {"token": "opaque"}}),
            true,
        ),
        (json!({"kind": "futureFailure", "evidence": [1, 2]}), false),
        (
            json!({"kind": "backendUnavailable", "retryAfterMs": 1}),
            false,
        ),
        // Admission evidence is not valid on getBalance.
        (
            json!({"kind": "backendUnavailable", "admission": "unknown"}),
            false,
        ),
    ] {
        let error = ErrorObjectOwned::owned(-32050, "Backend unavailable", Some(data.clone()));
        let service = Service {
            error: Some(error.clone()),
            ..Service::default()
        };
        let (http, handle) = start(
            NativeModule::new(service.clone())
                .unwrap()
                .finalize()
                .into_rpc(),
        )
        .await;
        let client = NativeClient::new(http);
        let request: AddressQuery =
            serde_json::from_value(fixtures::request("getBalance")).unwrap();
        let NativeClientError::Application {
            source,
            data: decoded,
        } = client.get_balance(request).await.unwrap_err()
        else {
            panic!()
        };
        assert_eq!(source, error);
        assert_eq!(service.calls.lock().unwrap().len(), 1);
        assert_eq!(matches!(*decoded, DecodedError::Known { .. }), classified);
        assert_eq!(
            decoded.raw(),
            &json!({"code": -32050, "message": "Backend unavailable", "data": data})
        );
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

#[tokio::test]
async fn client_tolerates_future_results_but_rejects_malformed_known_variants() {
    for (expected, raw) in [
        (
            "known",
            json!({"status": "committed", "blockHeight": "812", "future": 42}),
        ),
        (
            "unsupported",
            json!({"status": "futureStatus", "payload": {"keep": [1, 2]}}),
        ),
        (
            "malformed",
            json!({"status": "committed", "blockHeight": 812}),
        ),
    ] {
        // A raw peer is needed to emit malformed known results, which typed
        // producers cannot construct. Exercise the real client decoder here.
        let mut module = RpcModule::new(raw.clone());
        module
            .register_method("getTransactionStatus", |_, raw, _| raw.clone())
            .unwrap();
        let (http, handle) = start(module).await;
        let client = NativeClient::new(http);
        let query: TransactionQuery =
            serde_json::from_value(fixtures::request("getTransactionStatus")).unwrap();
        match client.get_transaction_status(query).await {
            Ok(DecodedResponse::Known(_)) => assert_eq!(expected, "known"),
            Ok(DecodedResponse::Unsupported { raw: preserved }) => {
                assert_eq!(expected, "unsupported");
                assert_eq!(preserved, raw);
            }
            Err(NativeClientError::Transport(jsonrpsee::core::client::Error::ParseError(_))) => {
                assert_eq!(expected, "malformed");
            }
            other => panic!("unexpected {other:?}"),
        }
        handle.stop().unwrap();
        handle.stopped().await;
    }
}

#[tokio::test]
async fn submission_and_faucet_errors_do_not_repeat_writes() {
    for method in ["sendTransaction", "faucet_requestAirdrop"] {
        let data = if method == "sendTransaction" {
            json!({"kind": "backendUnavailable", "admission": "unknown"})
        } else {
            json!({"kind": "backendUnavailable"})
        };
        let service = Service {
            error: Some(ErrorObjectOwned::owned(
                -32050,
                "Backend unavailable",
                Some(data),
            )),
            ..Service::default()
        };
        let (http, handle) = start(
            NativeModule::new(service.clone())
                .unwrap()
                .finalize()
                .into_rpc(),
        )
        .await;
        let client = NativeClient::new(http);
        let request = fixtures::request(method);
        let error = if method == "sendTransaction" {
            client
                .send_transaction(serde_json::from_value(request).unwrap())
                .await
                .unwrap_err()
        } else {
            client
                .request_airdrop(serde_json::from_value(request).unwrap())
                .await
                .unwrap_err()
        };
        let NativeClientError::Application { data, .. } = error else {
            panic!("{error:?}")
        };
        assert!(matches!(*data, DecodedError::Known { .. }));
        assert_eq!(
            service.calls.lock().unwrap().as_slice(),
            &[(method.into(), fixtures::request(method))]
        );
        handle.stop().unwrap();
        handle.stopped().await;
    }
}
