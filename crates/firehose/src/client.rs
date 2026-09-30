//! This boundary exposes only the non-destructive committed-block procedure.
use crate::Error;
use groundhog_indexer::{
    EventIndexer_EventIndexer1_Client, EventIndexerPullOutcome, EventIndexerPullRequest,
    EventIndexerPullResult, IndexerProgress,
};
use groundhog_xdr::network::ProtocolCommit;
use std::time::Duration;

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
        let addr = tokio::net::lookup_host(endpoint)
            .await?
            .next()
            .ok_or_else(|| crate::invalid("endpoint resolved to no addresses"))?;
        Ok(Self(
            groundhog_x23_transport::connect_with_config(
                "event_indexer",
                &addr.to_string(),
                1,
                rpc_driver::ConnectionConfig::new(Some(0x7fff_ffff), rpc_driver::ServiceMap::new()),
                EventIndexer_EventIndexer1_Client::new,
            )
            .await?,
        ))
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
            .pull(protocol, IndexerProgress::Default { state: 0 })
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
                    outcome: 2,
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
        let origin = groundhog_indexer::IndexerOutboxOrigin {
            protocol: groundhog_xdr::PROTOCOL_VERSION,
            epoch_sequence: 1,
            first_height: 1.into(),
            first_prev_hash: groundhog_xdr::types::ConsensusValueHash::new([0; 32].into()),
        };
        let initialized = groundhog_indexer::IndexerInitializedProgress {
            origin,
            contiguous_height: 0.into(),
            last_consensus_hash: groundhog_xdr::types::ConsensusValueHash::new([0; 32].into()),
        };
        for progress in [
            IndexerProgress::Default { state: 0 },
            IndexerProgress::IndexerProgressInitialized {
                initialized: initialized.clone(),
            },
            IndexerProgress::IndexerProgressCaughtUp { initialized },
        ] {
            assert_eq!(
                outcome(
                    &client
                        .pull(groundhog_xdr::PROTOCOL_VERSION, progress)
                        .await
                        .unwrap()
                ),
                Outcome::NotReady
            );
        }
        assert_eq!(*sender.procedures.lock().unwrap(), [10, 10, 10]);
    }
}
