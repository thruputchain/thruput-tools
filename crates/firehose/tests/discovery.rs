use groundhog_indexer::{EventIndexerPullRequest, EventIndexerPullResult};
use rpc_driver::{ProgVers, RpcError, RpcServer, ServiceMap, handler::RpcHandler};
use serde_json::Value;
use tokio::{io::AsyncWriteExt, net::TcpListener, process::Command};

struct Reply(i32);

#[async_trait::async_trait]
impl RpcHandler for Reply {
    fn prog_vers(&self) -> ProgVers {
        ProgVers::new(0x4321_aaac, 1)
    }

    async fn dispatch(&self, procedure: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
        assert_eq!(procedure, 10, "never drain the procedure-11 mailbox");
        let request: EventIndexerPullRequest = xdrs::txs::decode(data).unwrap();
        assert_eq!(request.progress.state().0, 0, "discovery must not park");
        if self.0 == -1 {
            return Err(RpcError::SystemError);
        }
        Ok(xdrs::txs::encode(&EventIndexerPullResult::Default {
            outcome: self.0,
            diagnostic: "test".try_into().unwrap(),
        })
        .unwrap())
    }
}

async fn run(addresses: &[&str], json: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hog-firehose"));
    command.env_remove("RUST_LOG").arg("discover");
    if json {
        command.arg("--json");
    }
    command.args(addresses).output().await.unwrap()
}

#[tokio::test]
async fn exit_codes_and_json_cover_matches_negatives_and_errors() {
    let mut servers = Vec::new();
    let mut addresses = Vec::new();
    for reply in [Some(2), Some(3), None, Some(-1)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addresses.push(listener.local_addr().unwrap().to_string());
        let mut services = ServiceMap::new();
        if let Some(reply) = reply {
            services.add_service(Box::new(Reply(reply)));
        }
        servers.push(RpcServer::new(listener, services).start().await);
    }
    let [matched, mismatch, other_program, broken] = addresses.as_slice() else {
        unreachable!()
    };
    for (candidates, expected_code, outcomes) in [
        (vec![matched.as_str()], 0, vec!["matched"]),
        (
            vec![mismatch.as_str(), other_program],
            1,
            vec!["no_match", "no_match"],
        ),
        (
            vec![mismatch.as_str(), broken],
            2,
            vec!["no_match", "probe_error"],
        ),
        (
            vec![broken.as_str(), matched, mismatch],
            0,
            vec!["matched", "probe_error", "no_match"],
        ),
    ] {
        let output = run(&candidates, true).await;
        assert_eq!(output.status.code(), Some(expected_code));
        assert!(output.stderr.is_empty(), "{output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["match"],
            if expected_code == 0 {
                Value::from(matched.as_str())
            } else {
                Value::Null
            }
        );
        let results = report["candidates"].as_array().unwrap();
        assert_eq!(results.len(), candidates.len());
        for (candidate, outcome) in results.iter().zip(outcomes) {
            assert_eq!(candidate["outcome"], outcome);
            assert!(candidates.contains(&candidate["address"].as_str().unwrap()));
            assert_eq!(candidate["reason"].is_null(), outcome == "matched");
        }
    }
    let output = run(&[broken, matched, mismatch], false).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<_> = stdout.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], format!("{matched} EventIndexer1 (matched)"));
    assert!(lines[1].contains("probe error ("));
    assert!(lines[2].contains("no match ("));
    for server in servers {
        server.abort();
    }
}

#[tokio::test]
async fn http_listener_is_a_quiet_clean_negative() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    let output = run(&[&address], false).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{address} no match (HTTP listener, not EventIndexer1)\n")
    );
    server.abort();
}

#[test]
fn help_documents_all_exit_codes() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_hog-firehose"))
        .args(["discover", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for text in [
        "0 = at least one match",
        "1 = no match",
        "2 = no match",
        "inconclusive",
    ] {
        assert!(help.contains(text), "{help}");
    }
}
