//! Rejection of JSON-RPC params a method does not declare.
//!
//! jsonrpsee decodes a method's params into its Rust arguments and stops: a
//! positional array longer than the argument list, or a by-name object with a
//! key that names no argument, is accepted and the extra value dropped. A caller
//! who adds a parameter the method lacks (a block height on a latest-state read)
//! would then get an answer to a question they did not ask. This layer answers
//! such a call with `InvalidParams` before it reaches the method.

use std::{collections::HashMap, fmt, future::ready, sync::LazyLock};

use futures_util::future::Either;
use groundhog_json_rpc_api::{AirdropApiOpenRpc, GroundhogApiOpenRpc};
use jsonrpsee::{
    core::{
        middleware::{Batch, BatchEntry, BatchEntryErr, Notification, RpcServiceT},
        server::MethodResponse,
    },
    types::{ErrorObjectOwned, Request},
};
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};

use crate::RpcServerError;

/// Declared parameter names for every method of the public RPC traits.
static DECLARED_PARAMS: LazyLock<HashMap<&'static str, &'static [&'static str]>> =
    LazyLock::new(|| {
        GroundhogApiOpenRpc::PARAM_NAMES
            .iter()
            .chain(AirdropApiOpenRpc::PARAM_NAMES)
            .copied()
            .collect()
    });

/// Answers a call whose params the method does not declare with `InvalidParams`.
///
/// Applies to the methods of `GroundhogApi` and `AirdropApi`; calls to any other
/// method pass through unchecked. Params that are not a JSON array or object, or
/// that do not parse, are left for the method's own decoding to reject. Checks
/// each call of a JSON-RPC array too, so its position among other layers does
/// not matter.
#[derive(Clone, Debug)]
pub struct RejectUndeclaredParams<S> {
    inner: S,
}

impl<S> RejectUndeclaredParams<S> {
    /// Wrap a service.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S> RpcServiceT for RejectUndeclaredParams<S>
where
    S: RpcServiceT<MethodResponse = MethodResponse> + Send + Sync,
{
    type MethodResponse = MethodResponse;
    type BatchResponse = S::BatchResponse;
    type NotificationResponse = S::NotificationResponse;

    fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = MethodResponse> + Send + 'a {
        match undeclared_params(&req) {
            Some(error) => Either::Left(ready(
                MethodResponse::error(req.id(), error).with_extensions(req.extensions().clone()),
            )),
            None => Either::Right(self.inner.call(req)),
        }
    }

    fn batch<'a>(
        &self,
        mut batch: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        for entry in batch.iter_mut() {
            if let Ok(BatchEntry::Call(req)) = entry
                && let Some(error) = undeclared_params(req)
            {
                *entry = Err(BatchEntryErr::new(req.id(), error));
            }
        }
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        self.inner.notification(n)
    }
}

/// The error for a call carrying params its method does not declare, if any.
fn undeclared_params(req: &Request<'_>) -> Option<ErrorObjectOwned> {
    let method = req.method_name();
    let declared = DECLARED_PARAMS.get(method)?;
    let raw = req.params.as_deref()?.get().trim_start();
    let message = if raw.starts_with('[') {
        let given = req.params().parse::<Vec<IgnoredAny>>().ok()?.len();
        (given > declared.len()).then(|| {
            format!(
                "{method} takes {} positional param{}, got {given}",
                declared.len(),
                if declared.len() == 1 { "" } else { "s" },
            )
        })?
    } else if raw.starts_with('{') {
        let ParamNames(keys) = req.params().parse().ok()?;
        let mut unknown: Vec<&str> = keys
            .iter()
            .map(String::as_str)
            .filter(|key| !declared.iter().any(|name| names_param(key, name)))
            .collect();
        if unknown.is_empty() {
            return None;
        }
        unknown.sort_unstable();
        format!(
            "{method} has no param named {}; it takes {}",
            quoted_list(&unknown),
            if declared.is_empty() {
                "none".to_owned()
            } else {
                quoted_list(declared)
            },
        )
    } else {
        return None;
    };
    Some(RpcServerError::InvalidParam(message).into())
}

/// The keys of a by-name params object, its values skipped unparsed.
struct ParamNames(Vec<String>);

impl<'de> Deserialize<'de> for ParamNames {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Keys;

        impl<'de> Visitor<'de> for Keys {
            type Value = ParamNames;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a params object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ParamNames, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(ParamNames(keys))
            }
        }

        deserializer.deserialize_map(Keys)
    }
}

/// Whether a by-name params key selects the argument `name`, under either
/// spelling jsonrpsee accepts: the argument's own `snake_case` name or its
/// `camelCase` form.
fn names_param(key: &str, name: &str) -> bool {
    key == name || key == lower_camel_case(name)
}

fn lower_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for c in snake.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn quoted_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use jsonrpsee::{RpcModule, core::middleware::RpcServiceBuilder, server::ServerHandle};
    use serde_json::{Value, json};

    use super::*;
    use crate::batch::{ConcurrentBatchService, RpcBatchConcurrency, tests::http_request};

    /// Where the layer sits relative to the concurrent array dispatcher.
    #[derive(Clone, Copy, Debug)]
    enum Placement {
        Alone,
        InsideBatching,
        OutsideBatching,
    }

    const PLACEMENTS: [Placement; 3] = [
        Placement::Alone,
        Placement::InsideBatching,
        Placement::OutsideBatching,
    ];

    fn batching<S>(inner: S) -> ConcurrentBatchService<S> {
        ConcurrentBatchService::new(inner, 1 << 20, RpcBatchConcurrency::default())
    }

    /// Serves stub methods that answer `"ok"` and count their invocations, behind
    /// `RejectUndeclaredParams` in the given placement.
    async fn start(placement: Placement) -> (SocketAddr, ServerHandle, Arc<AtomicUsize>) {
        let invoked = Arc::new(AtomicUsize::new(0));
        let mut module = RpcModule::new(invoked.clone());
        for method in [
            "groundhog_getStorageAt",
            "groundhog_getTransactionReceipt",
            "groundhog_view",
            "groundhog_chainTip",
            "other_method",
        ] {
            module
                .register_method(method, |_, invoked, _| {
                    invoked.fetch_add(1, Ordering::SeqCst);
                    "ok"
                })
                .unwrap();
        }
        macro_rules! serve {
            ($middleware:expr) => {{
                let server = jsonrpsee::server::Server::builder()
                    .set_rpc_middleware($middleware)
                    .build("127.0.0.1:0")
                    .await
                    .unwrap();
                (server.local_addr().unwrap(), server.start(module))
            }};
        }
        let (address, handle) = match placement {
            Placement::Alone => {
                serve!(RpcServiceBuilder::new().layer_fn(RejectUndeclaredParams::new))
            }
            Placement::InsideBatching => serve!(
                RpcServiceBuilder::new()
                    .layer_fn(batching)
                    .layer_fn(RejectUndeclaredParams::new)
            ),
            Placement::OutsideBatching => serve!(
                RpcServiceBuilder::new()
                    .layer_fn(RejectUndeclaredParams::new)
                    .layer_fn(batching)
            ),
        };
        (address, handle, invoked)
    }

    async fn post(address: SocketAddr, body: Value) -> Value {
        serde_json::from_str(&http_request(address, &body.to_string()).await).unwrap()
    }

    fn call(method: &str, params: &Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    }

    fn assert_rejected(response: &Value, message: &str) {
        assert_eq!(response["error"]["code"], -32602, "{response}");
        assert_eq!(
            response["error"]["message"],
            format!("invalid params: {message}"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn positional_params_beyond_the_declared_arguments_are_rejected() {
        let (address, _handle, invoked) = start(Placement::Alone).await;

        let accepted = post(
            address,
            call("groundhog_getStorageAt", &json!(["0xaddr", "0xkey"])),
        )
        .await;
        let rejected = post(
            address,
            call("groundhog_getStorageAt", &json!(["0xaddr", "0xkey", 62256])),
        )
        .await;
        let no_params = post(address, call("groundhog_chainTip", &json!([62256]))).await;

        assert_eq!(accepted["result"], "ok");
        assert_rejected(
            &rejected,
            "groundhog_getStorageAt takes 2 positional params, got 3",
        );
        assert_rejected(
            &no_params,
            "groundhog_chainTip takes 0 positional params, got 1",
        );
        assert_eq!(invoked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn named_params_the_method_does_not_declare_are_rejected() {
        let (address, _handle, invoked) = start(Placement::Alone).await;

        let accepted = post(address, call("groundhog_view", &json!({"request": {}}))).await;
        let rejected = post(
            address,
            call(
                "groundhog_view",
                &json!({"request": {}, "height": 62256, "atHeight": 62256}),
            ),
        )
        .await;
        let no_params = post(address, call("groundhog_chainTip", &json!({"height": 1}))).await;

        assert_eq!(accepted["result"], "ok");
        assert_rejected(
            &rejected,
            "groundhog_view has no param named `atHeight`, `height`; it takes `request`",
        );
        assert_rejected(
            &no_params,
            "groundhog_chainTip has no param named `height`; it takes none",
        );
        assert_eq!(invoked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn named_params_accept_both_spellings_jsonrpsee_decodes() {
        let (address, _handle, invoked) = start(Placement::Alone).await;

        for key in ["tx_hash", "txHash"] {
            let response = post(
                address,
                call("groundhog_getTransactionReceipt", &json!({key: "0x00"})),
            )
            .await;
            assert_eq!(response["result"], "ok", "{key}: {response}");
        }
        assert_eq!(invoked.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn absent_params_and_unlisted_methods_pass_through() {
        let (address, _handle, invoked) = start(Placement::Alone).await;

        let absent = post(
            address,
            json!({"jsonrpc": "2.0", "id": 1, "method": "groundhog_chainTip"}),
        )
        .await;
        let unlisted = post(address, call("other_method", &json!([1, 2, 3]))).await;

        assert_eq!(absent["result"], "ok");
        assert_eq!(unlisted["result"], "ok");
        assert_eq!(invoked.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn each_call_of_an_array_is_checked_wherever_the_layer_sits() {
        for placement in PLACEMENTS {
            let (address, _handle, invoked) = start(placement).await;

            let responses = post(
                address,
                json!([
                    {"jsonrpc": "2.0", "id": "valid", "method": "groundhog_chainTip"},
                    {"jsonrpc": "2.0", "id": "extra", "method": "groundhog_chainTip",
                        "params": [62256]},
                    {"jsonrpc": "2.0", "id": "named", "method": "groundhog_view",
                        "params": {"request": {}, "blockHeight": 62256}},
                ]),
            )
            .await;

            let responses = responses.as_array().unwrap();
            assert_eq!(responses.len(), 3, "{placement:?}");
            assert_eq!(responses[0]["id"], "valid", "{placement:?}");
            assert_eq!(responses[0]["result"], "ok", "{placement:?}");
            assert_eq!(responses[1]["id"], "extra", "{placement:?}");
            assert_rejected(
                &responses[1],
                "groundhog_chainTip takes 0 positional params, got 1",
            );
            assert_eq!(responses[2]["id"], "named", "{placement:?}");
            assert_rejected(
                &responses[2],
                "groundhog_view has no param named `blockHeight`; it takes `request`",
            );
            assert_eq!(invoked.load(Ordering::SeqCst), 1, "{placement:?}");
        }
    }

    #[test]
    fn declared_params_cover_every_public_method() {
        let names = GroundhogApiOpenRpc::PARAM_NAMES.len() + AirdropApiOpenRpc::PARAM_NAMES.len();
        assert_eq!(
            DECLARED_PARAMS.len(),
            names,
            "a method name is declared twice"
        );
        assert_eq!(
            DECLARED_PARAMS.get("groundhog_getStorageAt"),
            Some(&&["address", "key"][..])
        );
        assert_eq!(
            DECLARED_PARAMS.get("groundhog_view"),
            Some(&&["request"][..])
        );
        assert_eq!(DECLARED_PARAMS.get("groundhog_chainTip"), Some(&&[][..]));
    }

    #[test]
    fn lower_camel_case_matches_jsonrpsee_aliases() {
        assert_eq!(lower_camel_case("tx_hash"), "txHash");
        assert_eq!(lower_camel_case("request"), "request");
        assert_eq!(lower_camel_case("tx_hex"), "txHex");
    }
}
