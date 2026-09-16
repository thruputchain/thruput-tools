//! Bounded HTTP response classification and RTT calibration.

use std::future::Future;
use std::time::{Duration, Instant};

use groundhog_json_rpc_types::{
    receipt::TransactionReceipt,
    transaction::{TransactionStatus, TxHash},
};
use serde::Deserialize;

use crate::BenchError;
use crate::workload::make_request;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestOutcome {
    Success,
    Error(RequestErrorKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestErrorKind {
    Transport,
    HttpStatus,
    InvalidJsonRpcResponse,
    JsonRpc,
    Semantic,
}

fn json_rpc_result(
    value: serde_json::Value,
    expected_id: u64,
) -> Result<serde_json::Value, RequestErrorKind> {
    let serde_json::Value::Object(mut object) = value else {
        return Err(RequestErrorKind::InvalidJsonRpcResponse);
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || object.get("id").and_then(serde_json::Value::as_u64) != Some(expected_id)
    {
        return Err(RequestErrorKind::InvalidJsonRpcResponse);
    }
    match (object.remove("result"), object.remove("error")) {
        (Some(value), None) => Ok(value),
        (None, Some(serde_json::Value::Object(error)))
            if error
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .is_some()
                && error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .is_some() =>
        {
            Err(RequestErrorKind::JsonRpc)
        }
        _ => Err(RequestErrorKind::InvalidJsonRpcResponse),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ExpectedResult {
    Any,
    Committed { height: i64 },
    Receipt { hash: TxHash, height: i64 },
}

impl ExpectedResult {
    pub(super) fn validate(self, value: &serde_json::Value) -> bool {
        match self {
            Self::Any => true,
            Self::Committed { height: expected } => {
                matches!(TransactionStatus::deserialize(value), Ok(TransactionStatus::Committed { height }) if *height == expected)
            }
            Self::Receipt { hash, height } => {
                matches!(TransactionReceipt::deserialize(value), Ok(receipt) if receipt.tx_hash == hash && *receipt.height == height)
            }
        }
    }
}

fn append_response_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_response_bytes: usize,
) -> Result<(), RequestErrorKind> {
    if chunk.len() > max_response_bytes.saturating_sub(body.len()) {
        return Err(RequestErrorKind::InvalidJsonRpcResponse);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

async fn response_value(
    response: Result<reqwest::Response, reqwest::Error>,
    expected_id: u64,
    max_response_bytes: usize,
) -> Result<serde_json::Value, RequestErrorKind> {
    let mut response = match response {
        Ok(response) => response,
        Err(_) => return Err(RequestErrorKind::Transport),
    };
    if !response.status().is_success() {
        return Err(RequestErrorKind::HttpStatus);
    }

    let max_response_bytes_u64 = u64::try_from(max_response_bytes).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes_u64)
    {
        return Err(RequestErrorKind::InvalidJsonRpcResponse);
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(max_response_bytes_u64) as usize,
    );
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return Err(RequestErrorKind::Transport),
        };
        append_response_chunk(&mut body, &chunk, max_response_bytes)?;
    }

    match serde_json::from_slice(&body) {
        Ok(value) => json_rpc_result(value, expected_id),
        Err(_) => Err(RequestErrorKind::InvalidJsonRpcResponse),
    }
}

pub(super) async fn request_value<F>(
    request: F,
    expected_id: u64,
    request_timeout: Duration,
    max_response_bytes: usize,
) -> Result<serde_json::Value, RequestErrorKind>
where
    F: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let request = async {
        let response = request.await;
        response_value(response, expected_id, max_response_bytes).await
    };
    match tokio::time::timeout(request_timeout, request).await {
        Ok(outcome) => outcome,
        Err(_) => Err(RequestErrorKind::Transport),
    }
}

pub(super) async fn classify_request<F>(
    request: F,
    expected_id: u64,
    request_timeout: Duration,
    max_response_bytes: usize,
    expected: ExpectedResult,
) -> RequestOutcome
where
    F: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    match request_value(request, expected_id, request_timeout, max_response_bytes).await {
        Ok(value) if expected.validate(&value) => RequestOutcome::Success,
        Ok(_) => RequestOutcome::Error(RequestErrorKind::Semantic),
        Err(error) => RequestOutcome::Error(error),
    }
}

/// Calibrate connection pool size by measuring RTT with a small burst.
///
/// Always probes with `chainTip` — it's cheap, read-only, always valid,
/// and avoids the trap of sending a dummy empty payload through
/// `sendRawTransaction`'s validator. The result is only a rough sizing
/// hint for Little's Law; server-side processing time on the real
/// benchmark method may be larger, which is why the caller inflates
/// the connection count before clamping. An HTTP/2 origin (or a
/// reverse proxy fronting the gateway) would obsolete this heuristic
/// entirely by making per-connection serialization a non-issue.
pub(super) async fn calibrate_rtt(
    client: &reqwest::Client,
    url: &str,
    request_timeout: Duration,
    max_response_bytes: usize,
) -> Result<Duration, BenchError> {
    let body = make_request("chainTip", "", 0);
    let mut rtts = Vec::with_capacity(20);

    for _ in 0..20 {
        let start = Instant::now();
        let request = client.post(url).json(&body).send();
        if classify_request(
            request,
            0,
            request_timeout,
            max_response_bytes,
            ExpectedResult::Any,
        )
        .await
            == RequestOutcome::Success
        {
            rtts.push(start.elapsed());
        }
    }

    if rtts.is_empty() {
        return Err(BenchError::Calibration {
            url: url.to_string(),
        });
    }

    rtts.sort();
    // Use p50 as the representative RTT
    Ok(rtts[rtts.len() / 2])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn classify_json_rpc_value(value: &serde_json::Value, expected_id: u64) -> RequestOutcome {
        match json_rpc_result(value.clone(), expected_id) {
            Ok(_) => RequestOutcome::Success,
            Err(error) => RequestOutcome::Error(error),
        }
    }

    #[test]
    fn json_rpc_result_is_successful() {
        assert_eq!(
            classify_json_rpc_value(&json!({"jsonrpc": "2.0", "id": 7, "result": null}), 7,),
            RequestOutcome::Success
        );
    }

    #[test]
    fn json_rpc_error_is_not_successful() {
        assert_eq!(
            classify_json_rpc_value(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "error": {"code": -32000, "message": "rejected"}
                }),
                7,
            ),
            RequestOutcome::Error(RequestErrorKind::JsonRpc)
        );
    }

    #[test]
    fn mismatched_or_incomplete_json_rpc_response_is_invalid() {
        for value in [
            json!({"jsonrpc": "2.0", "id": 8, "result": true}),
            json!({"jsonrpc": "2.0", "id": 7}),
            json!({"id": 7, "result": true}),
            json!({"jsonrpc": "2.0", "id": 7, "result": true, "error": null}),
            json!({"jsonrpc": "2.0", "id": 7, "error": "rejected"}),
            json!([]),
        ] {
            assert_eq!(
                classify_json_rpc_value(&value, 7),
                RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse)
            );
        }
    }

    #[test]
    fn response_body_limit_accepts_exact_boundary_and_rejects_overflow() {
        let mut body = vec![0x01, 0x02];

        append_response_chunk(&mut body, &[0x03, 0x04], 4)
            .expect("bug: exact response limit should be accepted");
        assert_eq!(body, vec![0x01, 0x02, 0x03, 0x04]);

        assert_eq!(
            append_response_chunk(&mut body, &[0x05], 4),
            Err(RequestErrorKind::InvalidJsonRpcResponse)
        );
        assert_eq!(body, vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[tokio::test]
    async fn request_timeout_bounds_pending_transport() {
        let request = std::future::pending::<Result<reqwest::Response, reqwest::Error>>();

        let outcome = classify_request(
            request,
            0,
            Duration::from_millis(1),
            1024,
            ExpectedResult::Any,
        )
        .await;

        assert_eq!(outcome, RequestOutcome::Error(RequestErrorKind::Transport));
    }
    #[test]
    fn confirmed_results_reject_missing_wrong_height_and_wrong_hash() {
        let hash: TxHash = serde_json::from_value(json!(format!("0x{}", "aa".repeat(32)))).unwrap();
        let status = ExpectedResult::Committed { height: 7 };
        for value in [
            json!(null),
            json!({"status":"pending"}),
            json!({"status":"unknown"}),
            json!({"status":"committed","height":8}),
        ] {
            assert!(!status.validate(&value));
        }
        assert!(status.validate(&json!({"status":"committed","height":7})));
        let expected = ExpectedResult::Receipt { hash, height: 7 };
        assert!(!expected.validate(&json!(null)));
        let mut receipt = json!({"txHash":hash,"height":7,"status":{"result":"deterministicError","kind":"fault"},"events":[]});
        assert!(
            expected.validate(&receipt),
            "committed execution failures are valid read records"
        );
        receipt["height"] = json!(8);
        assert!(!expected.validate(&receipt));
        receipt["height"] = json!(7);
        receipt["txHash"] = json!(format!("0x{}", "bb".repeat(32)));
        assert!(!expected.validate(&receipt));
    }
}
