use std::time::Duration;

use groundhog_json_rpc_types::{error::outcome_for_code, transaction::TransactionStatus};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", content = "code", rename_all = "snake_case")]
pub(super) enum Outcome {
    Unknown,
    Overloaded(i32),
    RpcError(i32),
    Transport,
    Timeout,
    HttpStatus,
    InvalidResponse,
    UnexpectedStatus,
}

pub(super) fn corpus(seed: &str, count: u32) -> Vec<String> {
    (0..count)
        .map(|index| {
            let mut hash = Sha256::new();
            hash.update(b"groundhog-lookup-bench-v1\0");
            hash.update(seed.as_bytes());
            hash.update(index.to_be_bytes());
            format!("0x{}", hex::encode(hash.finalize()))
        })
        .collect()
}

pub(super) async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Outcome> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            Outcome::Timeout
        } else {
            Outcome::Transport
        }
    })? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(Outcome::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn classify(value: Value, id: u64) -> Outcome {
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_u64) != Some(id)
    {
        return Outcome::InvalidResponse;
    }
    match (value.get("result"), value.get("error")) {
        (Some(result), None) => match serde_json::from_value::<TransactionStatus>(result.clone()) {
            Ok(TransactionStatus::Unknown) => Outcome::Unknown,
            Ok(_) => Outcome::UnexpectedStatus,
            Err(_) => Outcome::InvalidResponse,
        },
        (None, Some(error)) => {
            let Some(code) = error.get("code").and_then(Value::as_i64) else {
                return Outcome::InvalidResponse;
            };
            let Ok(code) = i32::try_from(code) else {
                return Outcome::InvalidResponse;
            };
            if error.get("message").and_then(Value::as_str).is_none() {
                return Outcome::InvalidResponse;
            }
            if outcome_for_code(code) == "overloaded" {
                Outcome::Overloaded(code)
            } else {
                Outcome::RpcError(code)
            }
        }
        _ => Outcome::InvalidResponse,
    }
}

pub(super) async fn request(
    client: &reqwest::Client,
    url: &str,
    hash: &str,
    id: u64,
    timeout: Duration,
) -> Outcome {
    let operation = async {
        let response = client
            .post(url)
            .json(&json!({"jsonrpc":"2.0", "id":id,
                "method":"groundhog_getTransactionStatus", "params":[hash]}))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    Outcome::Timeout
                } else {
                    Outcome::Transport
                }
            })?;
        if !response.status().is_success() {
            return Err(Outcome::HttpStatus);
        }
        let body = bounded_body(response, 64 * 1024).await?;
        let value = serde_json::from_slice(&body).map_err(|_| Outcome::InvalidResponse)?;
        Ok(classify(value, id))
    };
    match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(outcome)) => outcome,
        Err(_) => Outcome::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_envelope_and_status_and_canonical_overload() {
        assert_eq!(
            classify(
                json!({"jsonrpc":"2.0","id":1,
            "result":{"status":"unknown"}}),
                1
            ),
            Outcome::Unknown
        );
        assert_eq!(
            classify(
                json!({"jsonrpc":"2.0","id":2,
            "result":{"status":"unknown"}}),
                1
            ),
            Outcome::InvalidResponse
        );
        assert_eq!(
            classify(
                json!({"jsonrpc":"2.0","id":1,
            "result":{"status":"pending"}}),
                1
            ),
            Outcome::UnexpectedStatus
        );
        for (code, expected) in [
            (-32053, Outcome::Overloaded(-32053)),
            (-32011, Outcome::RpcError(-32011)),
        ] {
            assert_eq!(
                classify(
                    json!({"jsonrpc":"2.0","id":1,
                "error":{"code":code,"message":"test"}}),
                    1
                ),
                expected
            );
        }
    }
    #[tokio::test]
    async fn stalled_response_body_is_a_timeout() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let outcome = request(
            &client,
            &url,
            &corpus("a", 1)[0],
            1,
            Duration::from_millis(500),
        )
        .await;
        assert_eq!(outcome, Outcome::Timeout);
        server.abort();
    }

    #[test]
    fn corpus_is_deterministic_and_seeded() {
        assert_eq!(corpus("a", 2), corpus("a", 2));
        assert_ne!(corpus("a", 2), corpus("b", 2));
        assert_ne!(corpus("a", 2)[0], corpus("a", 2)[1]);
        assert_eq!(corpus("a", 1)[0].len(), 66);
    }
}
