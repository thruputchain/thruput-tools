//! Bounded HTTP response classification and RTT calibration.

use std::future::Future;
use std::time::{Duration, Instant};

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
}

fn classify_json_rpc_value(value: &serde_json::Value, expected_id: u64) -> RequestOutcome {
    let Some(object) = value.as_object() else {
        return RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse);
    };
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0")
        || object.get("id").and_then(serde_json::Value::as_u64) != Some(expected_id)
    {
        return RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse);
    }
    match (object.get("result"), object.get("error")) {
        (Some(_), None) => RequestOutcome::Success,
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
            RequestOutcome::Error(RequestErrorKind::JsonRpc)
        }
        _ => RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse),
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

async fn classify_response(
    response: Result<reqwest::Response, reqwest::Error>,
    expected_id: u64,
    max_response_bytes: usize,
) -> RequestOutcome {
    let mut response = match response {
        Ok(response) => response,
        Err(_) => return RequestOutcome::Error(RequestErrorKind::Transport),
    };
    if !response.status().is_success() {
        return RequestOutcome::Error(RequestErrorKind::HttpStatus);
    }

    let max_response_bytes_u64 = u64::try_from(max_response_bytes).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes_u64)
    {
        return RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse);
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
            Err(_) => return RequestOutcome::Error(RequestErrorKind::Transport),
        };
        if let Err(kind) = append_response_chunk(&mut body, &chunk, max_response_bytes) {
            return RequestOutcome::Error(kind);
        }
    }

    match serde_json::from_slice(&body) {
        Ok(value) => classify_json_rpc_value(&value, expected_id),
        Err(_) => RequestOutcome::Error(RequestErrorKind::InvalidJsonRpcResponse),
    }
}

pub(super) async fn classify_request<F>(
    request: F,
    expected_id: u64,
    request_timeout: Duration,
    max_response_bytes: usize,
) -> RequestOutcome
where
    F: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let request = async {
        let response = request.await;
        classify_response(response, expected_id, max_response_bytes).await
    };
    match tokio::time::timeout(request_timeout, request).await {
        Ok(outcome) => outcome,
        Err(_) => RequestOutcome::Error(RequestErrorKind::Transport),
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
        if classify_request(request, 0, request_timeout, max_response_bytes).await
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

        let outcome = classify_request(request, 0, Duration::from_millis(1), 1024).await;

        assert_eq!(outcome, RequestOutcome::Error(RequestErrorKind::Transport));
    }
}
