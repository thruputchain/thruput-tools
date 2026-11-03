//! Bounded HTTP response classification and RTT calibration.

use std::future::Future;
use std::time::{Duration, Instant};

use serde::Deserialize;
use thruput_json_rpc_types::{
    responses::{ResponseData, TransactionReceipt, TransactionStatus},
    transaction::TxHash,
};

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
                matches!(TransactionStatus::deserialize(value), Ok(TransactionStatus::Committed { block_height }) if *block_height == expected)
            }
            Self::Receipt { hash, height } => {
                matches!(TransactionReceipt::deserialize(value), Ok(receipt) if receipt.transaction_hash == hash && *receipt.block_height == height && receipt.is_supported())
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
/// Always probes with `getChainTip` — it's cheap, read-only, always valid,
/// and avoids the trap of sending a dummy empty payload through
/// `sendTransaction`'s validator. The result is only a rough sizing
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
    let body = make_request("getChainTip", "", 0);
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
        let hash = TxHash(thruput_xdr::types::SHA256Hash::from([0xaa; 32]).into());
        let status = ExpectedResult::Committed { height: 7 };
        for value in [
            json!(null),
            json!({"status":"pending"}),
            json!({"status":"unknown"}),
            json!({"status":"committed","blockHeight":"8"}),
        ] {
            assert!(!status.validate(&value));
        }
        assert!(status.validate(&json!({"status":"committed","blockHeight":"7"})));
        let expected = ExpectedResult::Receipt { hash, height: 7 };
        assert!(!expected.validate(&json!(null)));
        let mut receipt = json!({"transactionHash":hash,"blockHeight":"7","execution":{"result":"fault"},"invokedAddresses":[],"events":[],"gasUsed":"1500"});
        assert!(
            expected.validate(&receipt),
            "committed execution failures are valid read records"
        );
        receipt["blockHeight"] = json!("8");
        assert!(!expected.validate(&receipt));
        receipt["blockHeight"] = json!("7");
        receipt["transactionHash"] = json!(TxHash(
            thruput_xdr::types::SHA256Hash::from([0xbb; 32]).into()
        ));
        assert!(!expected.validate(&receipt));
    }
}
