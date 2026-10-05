use groundhog_firehose::client;
use groundhog_xdr::network::ProtocolCommit;
use serde::Serialize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Matched,
    NoMatch,
    ProbeError,
}

#[derive(Debug, Serialize)]
struct Candidate {
    address: String,
    outcome: Outcome,
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    #[serde(rename = "match")]
    matched: Option<String>,
    candidates: Vec<Candidate>,
}

impl Report {
    pub(super) async fn probe(addresses: Vec<String>, protocol: ProtocolCommit) -> Self {
        let mut candidates = Vec::new();
        for address in addresses {
            let (outcome, reason) = match client::discover(&address, protocol).await {
                Ok(true) => (Outcome::Matched, None),
                Ok(false) => (
                    Outcome::NoMatch,
                    Some("program, version or protocol mismatch".into()),
                ),
                Err(_) if is_http(&address).await => (
                    Outcome::NoMatch,
                    Some("HTTP listener, not EventIndexer1".into()),
                ),
                Err(error) => (Outcome::ProbeError, Some(error.to_string())),
            };
            candidates.push(Candidate {
                address,
                outcome,
                reason,
            });
        }
        candidates.sort_by_key(|candidate| candidate.outcome != Outcome::Matched);
        let matched = candidates
            .iter()
            .find(|candidate| candidate.outcome == Outcome::Matched)
            .map(|candidate| candidate.address.clone());
        Self {
            matched,
            candidates,
        }
    }

    pub(super) fn exit_code(&self) -> u8 {
        if self.matched.is_some() {
            0
        } else if self
            .candidates
            .iter()
            .any(|c| c.outcome == Outcome::ProbeError)
        {
            2
        } else {
            1
        }
    }

    pub(super) fn print(&self, json: bool) -> Result<(), serde_json::Error> {
        if json {
            println!("{}", serde_json::to_string(self)?);
        } else {
            for candidate in &self.candidates {
                let label = match candidate.outcome {
                    Outcome::Matched => "EventIndexer1 (matched)",
                    Outcome::NoMatch => "no match",
                    Outcome::ProbeError => "probe error",
                };
                let reason = candidate.reason.as_deref().unwrap_or("");
                // A server-controlled diagnostic must not create extra candidate lines.
                let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
                if reason.is_empty() {
                    println!("{} {label}", candidate.address);
                } else {
                    println!("{} {label} ({reason})", candidate.address);
                }
            }
        }
        Ok(())
    }
}

// rpc_driver reports both malformed framing and remote RPC system failures as
// SystemError. Only a confirmed HTTP response makes that a definitive negative.
// A bounded, read-only HTTP fallback leaves timeouts and broken x23 services
// inconclusive. All x23 probes still use the non-parking procedure-10 request.
async fn is_http(address: &str) -> bool {
    let probe = async {
        let mut socket = tokio::net::TcpStream::connect(address).await?;
        socket
            .write_all(b"HEAD / HTTP/1.0\r\nConnection: close\r\n\r\n")
            .await?;
        let mut prefix = [0; 12];
        socket.read_exact(&mut prefix).await?;
        Ok::<_, std::io::Error>(
            (prefix.starts_with(b"HTTP/1.0 ") || prefix.starts_with(b"HTTP/1.1 "))
                && prefix[9..12].iter().all(u8::is_ascii_digit),
        )
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(1), probe).await,
        Ok(Ok(true))
    )
}
