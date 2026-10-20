//! Error evidence for explicit native RPC qualification. No recovery action is inferred.

use serde_json::{Value, json};
use thruput_client::native::NativeRpcError;
use thruput_json_rpc_client::native::NativeClientError;

use crate::CliError;

pub(crate) fn failure(message: impl Into<String>, evidence: Value) -> CliError {
    CliError::NativeRpcFailure {
        message: message.into(),
        evidence,
    }
}

pub(crate) fn client_evidence(error: &NativeClientError) -> Value {
    match error {
        NativeClientError::Application { source, .. } => json!({"rpcError": source}),
        NativeClientError::Transport(source) => json!({"transportError": source.to_string()}),
        _ => json!({"unsupportedClientError": error.to_string()}),
    }
}

pub(crate) fn client_error(error: &NativeClientError) -> CliError {
    failure(error.to_string(), client_evidence(error))
}

pub(crate) fn rpc_error(error: &NativeRpcError) -> CliError {
    let evidence = match error {
        NativeRpcError::Call { method, source } => {
            json!({"method": method, "cause": client_evidence(source)})
        }
        NativeRpcError::InvalidDiscovery { source } => {
            json!({"method": "getVersionInfo", "cause": client_evidence(source)})
        }
        NativeRpcError::Unsupported { method, raw } => json!({"method": method, "result": raw}),
        NativeRpcError::NetworkMismatch { expected, actual } => json!({
            "preflightReason": "networkMismatch",
            "expectedNetworkHash": thruput_json_rpc_types::NetworkHash::from(expected.clone().into_inner()),
            "actualNetworkHash": thruput_json_rpc_types::NetworkHash::from(actual.clone().into_inner()),
        }),
        NativeRpcError::MissingMethods { methods } => json!({
            "preflightReason": "missingMethods", "missingMethods": methods,
        }),
        NativeRpcError::WireFormat { actual } => json!({
            "preflightReason": "wireFormat", "actualWireFormat": actual, "expectedMajor": 14,
        }),
        NativeRpcError::InvalidExpiry { tip, offset } => json!({
            "preflightReason": "invalidExpiry", "blockHeight": tip.to_string(), "offset": offset.to_string(),
        }),
        NativeRpcError::ResponseRepresentation { method } => json!({
            "preflightReason": "unsupportedRepresentation", "method": method,
        }),
        _ => json!({"preflightError": error.to_string()}),
    };
    failure(error.to_string(), evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_evidence_retains_typed_identity_and_decimal_heights() {
        let expected =
            thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([1; 32]));
        let actual =
            thruput_xdr::types::NetworkHash::from(thruput_xdr::types::SHA256Hash::from([2; 32]));
        let error = rpc_error(&NativeRpcError::NetworkMismatch {
            expected: expected.clone(),
            actual,
        });
        let details = error.details_json().unwrap();
        assert_eq!(details["preflightReason"], "networkMismatch");
        assert_eq!(
            details["expectedNetworkHash"],
            serde_json::to_value(thruput_json_rpc_types::NetworkHash::from(
                expected.into_inner()
            ))
            .unwrap()
        );
        let error = rpc_error(&NativeRpcError::InvalidExpiry {
            tip: i64::MAX,
            offset: 1,
        });
        let details = error.details_json().unwrap();
        assert_eq!(details["blockHeight"], i64::MAX.to_string());
        assert_eq!(details["offset"], "1");
    }
}
