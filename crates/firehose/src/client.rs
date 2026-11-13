//! This boundary exposes only the non-destructive committed-block procedure.
use crate::Error;
use std::{future::Future, net::SocketAddr, time::Duration};
use thruput_indexer::{
    EventIndexer_EventIndexer1_Client, EventIndexerPullOutcome, EventIndexerPullRequest,
    EventIndexerPullResult, IndexerProgress,
};
use thruput_xdr::network::ProtocolCommit;
use tokio::time::Instant;

/// Resolve an endpoint and share the remaining deadline across connection attempts.
pub async fn connect_endpoint<C, F, Fut>(
    endpoint: &str,
    deadline: Instant,
    connect: F,
) -> Result<C, Error>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = Result<C, thruput_x23_transport::ConnectError>>,
{
    connect_addresses(
        endpoint,
        tokio::net::lookup_host(endpoint).await?,
        deadline,
        connect,
    )
    .await
}

/// Try resolved addresses, sharing the remaining deadline across untried addresses.
pub async fn connect_addresses<C, F, Fut>(
    endpoint: &str,
    addresses: impl IntoIterator<Item = SocketAddr>,
    deadline: Instant,
    mut connect: F,
) -> Result<C, Error>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = Result<C, thruput_x23_transport::ConnectError>>,
{
    let mut attempts = Vec::new();
    let addresses: Vec<_> = addresses.into_iter().collect();
    for (index, address) in addresses.iter().copied().enumerate() {
        let remaining = u32::try_from(addresses.len() - index).unwrap_or(u32::MAX);
        let budget = deadline.saturating_duration_since(Instant::now()) / remaining;
        match tokio::time::timeout(budget, connect(address)).await {
            Ok(Ok(client)) => return Ok(client),
            Ok(Err(error)) => attempts.push(error),
            Err(_) => attempts.push(thruput_x23_transport::ConnectError::ConnectFailed {
                service: "endpoint",
                addr: address.to_string(),
                source: rpc_driver::RpcError::ConnectFailed,
            }),
        }
    }
    if attempts.is_empty() {
        return Err(crate::invalid("endpoint resolved to no addresses"));
    }
    Err(Error::AllAddressesFailed {
        endpoint: endpoint.to_owned(),
        attempts,
    })
}

async fn connect_event_indexer(
    address: SocketAddr,
) -> Result<Client, thruput_x23_transport::ConnectError> {
    thruput_x23_transport::connect_with_config(
        "event_indexer",
        &address.to_string(),
        1,
        rpc_driver::ConnectionConfig::new(Some(0x7fff_ffff), rpc_driver::ServiceMap::new()),
        EventIndexer_EventIndexer1_Client::new,
    )
    .await
    .map(Client)
}

/// The complete classified result space, including forward-incompatible values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A complete committed block is ready to verify and store.
    Block,
    /// The cursor is at the tip; the next request may park.
    CaughtUp,
    /// The outbox has not initialized yet.
    NotReady,
    /// The service rejected the requested protocol token.
    MismatchedProtocol,
    /// The server cannot continue the supplied durable cursor.
    ProgressConflict,
    /// The server failed to load or serve committed state.
    InternalFailure,
    /// A future or malformed discriminant requires operator intervention.
    Unknown,
}

impl Outcome {
    /// Stable metric label for each protocol outcome.
    pub fn label(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::CaughtUp => "caught_up",
            Self::NotReady => "not_ready",
            Self::MismatchedProtocol => "mismatched_protocol",
            Self::ProgressConflict => "progress_conflict",
            Self::InternalFailure => "internal_failure",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify all known discriminants; reject unknown ones in the ingest loop.
pub fn outcome(result: &EventIndexerPullResult) -> Outcome {
    match result.outcome() {
        EventIndexerPullOutcome::EventIndexerPullBlock => Outcome::Block,
        EventIndexerPullOutcome::EventIndexerPullCaughtUp => Outcome::CaughtUp,
        EventIndexerPullOutcome::EventIndexerPullNotReady => Outcome::NotReady,
        EventIndexerPullOutcome::EventIndexerPullMismatchedProtocol => Outcome::MismatchedProtocol,
        EventIndexerPullOutcome::EventIndexerPullProgressConflict => Outcome::ProgressConflict,
        EventIndexerPullOutcome::EventIndexerPullInternalFailure => Outcome::InternalFailure,
        _ => Outcome::Unknown,
    }
}

/// A single serial connection to `EventIndexer1`.
pub struct Client(EventIndexer_EventIndexer1_Client);

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Client(EventIndexer1)")
    }
}

impl Client {
    /// Resolve an explicit address and connect with the protocol's 31-bit ceiling.
    #[tracing::instrument(skip_all, fields(endpoint))]
    pub async fn connect(endpoint: &str) -> Result<Self, Error> {
        connect_endpoint(
            endpoint,
            Instant::now() + Duration::from_secs(3),
            connect_event_indexer,
        )
        .await
    }

    /// Pull one block or park. Never impose a request deadline on a parked pull.
    #[tracing::instrument(skip_all)]
    pub async fn pull(
        &self,
        protocol: ProtocolCommit,
        progress: IndexerProgress,
    ) -> Result<EventIndexerPullResult, Error> {
        // Procedure 10 ONLY. Procedure 11 drains and clears hog-event's failure
        // mailbox, stealing hog-indexer's diagnostics. Never call it, even for discovery.
        Ok(self
            .0
            .event_indexer_pull_next(&EventIndexerPullRequest { protocol, progress })
            .await?)
    }
}

/// Probe program 0x4321aaac without parking or changing any server cursor.
/// `EventIndexer1` has no separate intro: protocol negotiation is in procedure 10.
#[tracing::instrument(skip_all, fields(endpoint))]
pub async fn discover(endpoint: &str, protocol: ProtocolCommit) -> Result<bool, Error> {
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        Client::connect(endpoint)
            .await?
            .pull(
                protocol,
                IndexerProgress::Default {
                    state: thruput_indexer::IndexerProgressState(0),
                },
            )
            .await
    })
    .await
    .map_err(|_| crate::invalid("discovery timed out"))?;
    match result {
        Ok(result) => Ok(outcome(&result) != Outcome::MismatchedProtocol),
        Err(Error::Rpc(
            rpc_driver::RpcError::ProgUnavail
            | rpc_driver::RpcError::InvalidVersion { .. }
            | rpc_driver::RpcError::RpcMismatch
            | rpc_driver::RpcError::InvalidProcedure,
        )) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpc_driver::{CallBuf, ProgVers, RpcError, RpcResponse, RpcSender};
    use std::sync::{Arc, Mutex};

    struct RecordingSender {
        procedures: Mutex<Vec<u32>>,
    }

    struct NotReadyServer;

    #[async_trait::async_trait]
    impl rpc_driver::handler::RpcHandler for NotReadyServer {
        fn prog_vers(&self) -> ProgVers {
            ProgVers::new(0x4321_aaac, 1)
        }

        async fn dispatch(&self, procedure: u32, data: &[u8]) -> Result<Vec<u8>, RpcError> {
            assert_eq!(procedure, 10);
            let request: EventIndexerPullRequest = xdrs::txs::decode(data).unwrap();
            assert_eq!(request.progress.state().0, 0);
            Ok(xdrs::txs::encode(&EventIndexerPullResult::Default {
                outcome: thruput_indexer::EventIndexerPullOutcome(2),
                diagnostic: "not ready".try_into().unwrap(),
            })
            .unwrap())
        }
    }

    // Keep the port reserved without listening, so connection refusal cannot race
    // another test acquiring an ephemeral port that was just released.
    fn dead_socket() -> tokio::net::TcpSocket {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        socket
    }

    #[tokio::test]
    async fn later_address_serves_after_connection_refused() {
        let dead = dead_socket();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        let mut services = rpc_driver::ServiceMap::new();
        services.add_service(Box::new(NotReadyServer));
        let server = rpc_driver::RpcServer::new(listener, services).start().await;
        let mut attempted = Vec::new();

        let client = connect_addresses(
            "test.invalid:1234",
            [dead.local_addr().unwrap(), live, dead.local_addr().unwrap()],
            Instant::now() + Duration::from_secs(3),
            |address| {
                attempted.push(address);
                connect_event_indexer(address)
            },
        )
        .await
        .unwrap();
        let result = client
            .pull(
                thruput_xdr::PROTOCOL_VERSION,
                IndexerProgress::Default {
                    state: thruput_indexer::IndexerProgressState(0),
                },
            )
            .await
            .unwrap();

        assert_eq!(outcome(&result), Outcome::NotReady);
        assert_eq!(attempted, [dead.local_addr().unwrap(), live]);
        server.abort();
    }

    #[tokio::test]
    async fn all_failed_addresses_are_reported_together() {
        let dead = [dead_socket(), dead_socket()];
        let addresses = dead.each_ref().map(|socket| socket.local_addr().unwrap());
        let error = connect_addresses(
            "test.invalid:1234",
            addresses,
            Instant::now() + Duration::from_secs(3),
            connect_event_indexer,
        )
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("all 2 resolved addresses failed for test.invalid:1234"));
        for address in addresses {
            assert!(message.contains(&address.to_string()), "{message}");
        }
        assert!(matches!(error, Error::AllAddressesFailed { attempts, .. } if attempts.len() == 2));
    }

    #[tokio::test]
    async fn hung_attempts_share_budget_and_report_every_address() {
        let addresses = [
            "127.0.0.1:1".parse().unwrap(),
            "127.0.0.1:2".parse().unwrap(),
        ];
        let start = Instant::now();
        let error = tokio::time::timeout(
            Duration::from_millis(200),
            connect_addresses::<(), _, _>(
                "test.invalid:1234",
                addresses,
                start + Duration::from_millis(100),
                |_| std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        let Error::AllAddressesFailed { ref attempts, .. } = error else {
            panic!("unexpected error: {error}");
        };
        assert_eq!(attempts.len(), 2);
        let details = attempts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        assert_eq!(
            error.to_string(),
            format!("all 2 resolved addresses failed for test.invalid:1234: {details}")
        );
        for address in addresses {
            assert!(error.to_string().contains(&address.to_string()));
        }
    }

    #[tokio::test]
    async fn empty_resolution_is_distinct_from_connection_failure() {
        let error = connect_addresses(
            "test.invalid:1234",
            [],
            Instant::now() + Duration::from_secs(3),
            connect_event_indexer,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Invalid(message)
                if message == "endpoint resolved to no addresses"));
    }

    #[async_trait::async_trait]
    impl RpcSender for RecordingSender {
        fn call_buf(&self) -> CallBuf {
            CallBuf::new()
        }
        async fn send_call(
            &self,
            program: ProgVers,
            procedure: u32,
            call: CallBuf,
        ) -> Result<RpcResponse, RpcError> {
            assert_eq!(program, ProgVers::new(0x4321_aaac, 1));
            assert_eq!(
                procedure, 10,
                "destructive procedure 11 must never be issued"
            );
            let _: EventIndexerPullRequest = xdrs::txs::decode(call.payload()).unwrap();
            self.procedures.lock().unwrap().push(procedure);
            Ok(RpcResponse::from_payload(
                xdrs::txs::encode(&EventIndexerPullResult::Default {
                    outcome: thruput_indexer::EventIndexerPullOutcome(2),
                    diagnostic: "not ready".try_into().unwrap(),
                })
                .unwrap(),
            ))
        }
    }

    #[tokio::test]
    async fn client_only_ever_issues_procedure_10() {
        let sender = Arc::new(RecordingSender {
            procedures: Mutex::new(Vec::new()),
        });
        let client = Client(EventIndexer_EventIndexer1_Client::new(sender.clone()));
        let origin = thruput_indexer::IndexerOutboxOrigin {
            protocol: thruput_xdr::PROTOCOL_VERSION,
            epoch_sequence: 1,
            first_height: 1.into(),
            first_prev_hash: thruput_xdr::types::ConsensusValueHash::new([0; 32].into()),
        };
        let initialized = thruput_indexer::IndexerInitializedProgress {
            origin,
            contiguous_height: 0.into(),
            last_consensus_hash: thruput_xdr::types::ConsensusValueHash::new([0; 32].into()),
        };
        for progress in [
            IndexerProgress::Default {
                state: thruput_indexer::IndexerProgressState(0),
            },
            IndexerProgress::IndexerProgressInitialized {
                initialized: initialized.clone(),
            },
            IndexerProgress::IndexerProgressCaughtUp { initialized },
        ] {
            assert_eq!(
                outcome(
                    &client
                        .pull(thruput_xdr::PROTOCOL_VERSION, progress)
                        .await
                        .unwrap()
                ),
                Outcome::NotReady
            );
        }
        assert_eq!(*sender.procedures.lock().unwrap(), [10, 10, 10]);
    }
}
