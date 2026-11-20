use std::net::SocketAddr;
use std::time::Duration;

use rpc_driver::{ProgramError, RpcServer, ServiceMap};
use thruput_json_rpc_server::native::read::NativeSimulationPlane;
use thruput_json_rpc_types::ContractAddress;
use thruput_json_rpc_types::hex::HexBytes;
use thruput_json_rpc_types::protocol_token::protocol_text;
use thruput_node_rpc::SchedulerGetTipResult;
use thruput_node_rpc::sched_coord::{
    SchedulerCoord_SchedulerCoord1, SchedulerCoord_SchedulerCoord1_Dispatcher,
};
use thruput_scp::network::ProtocolCommit as Protocol;
use thruput_view::{
    ViewGateway_ViewGateway1, ViewGateway_ViewGateway1_Dispatcher, ViewGatewayIntroOutcome,
    ViewGatewayPayload, ViewGatewayResult,
};
use thruput_xdr::PROTOCOL_VERSION;
use thruput_xdr::transaction::InvocationFrameOutput;
use thruput_xdr::view::TransactionViewQuery;
use tokio::net::TcpListener;
use xdr_common::{BytesM, VecM};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeViewGatewayMode {
    IntroRejected,
    ViewSlow,
    Batch,
    /// Return the timepoint a view ran at, from a snapshot at this height.
    EchoTimepoint(i64),
    /// Refuse a view stated before this timepoint, as hogd refuses one
    /// earlier than its snapshot's tip; otherwise echo it from this height.
    RefuseBefore(u64, i64),
    /// Echo each view's timepoint from a snapshot one block newer than the
    /// last, starting at this height, as if a block commits before each view.
    EchoRising(i64),
}

struct FakeViewGateway {
    mode: FakeViewGatewayMode,
    views: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[allow(refining_impl_trait)]
impl ViewGateway_ViewGateway1 for FakeViewGateway {
    async fn viewgateway_view_batch(
        &self,
        query: thruput_view::ViewGatewayBatchQuery,
    ) -> Result<thruput_view::ViewGatewayBatchResult, ProgramError> {
        use thruput_view::{
            ViewGatewayBatchPayload, ViewGatewayBatchResult, ViewGatewayHeightUnavailable,
        };
        assert!(query.budget_millis > 0);
        if self.mode != FakeViewGatewayMode::Batch {
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Ok(ViewGatewayBatchResult::Default {
                outcome: thruput_view::ViewGatewayBatchOutcome(6),
            });
        }
        assert!(query.budget_millis > 30_000);
        assert_eq!(query.max_response_bytes, 256);
        assert_ne!(
            query.calls.0[0].auth_claim,
            shared_view::public_view_auth_claim()
        );
        assert_ne!(query.calls.0[0].auth_claim, query.calls.0[1].auth_claim);
        if let Some(requested) = query.min_height.filter(|height| **height > 42) {
            return Ok(ViewGatewayBatchResult::ViewGatewayBatchHeightUnavailable {
                height_unavailable: ViewGatewayHeightUnavailable {
                    requested,
                    observed: 42.into(),
                },
            });
        }
        Ok(ViewGatewayBatchResult::ViewGatewayBatchOk {
            payload: ViewGatewayBatchPayload {
                height: 42.into(),
                results: VecM(
                    query
                        .calls
                        .0
                        .into_iter()
                        .map(|call| thruput_xdr::view::TransactionViewResult {
                            output: InvocationFrameOutput::Return {
                                return_buffer: BytesM(call.invocation.calldata.0),
                            },
                            gas_consumed: call.invocation.gas_limit,
                            events: VecM(vec![]),
                        })
                        .collect(),
                ),
            },
        })
    }
    async fn viewgateway_intro(
        &self,
        _arg0: Protocol,
    ) -> Result<ViewGatewayIntroOutcome, ProgramError> {
        if self.mode == FakeViewGatewayMode::IntroRejected {
            return Ok(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol);
        }
        Ok(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess)
    }

    async fn viewgateway_view(
        &self,
        query: TransactionViewQuery,
    ) -> Result<ViewGatewayResult, ProgramError> {
        let earlier_views = self.views.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.mode == FakeViewGatewayMode::ViewSlow {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let echo = query.block_timepoint.to_be_bytes().to_vec();
        let (height, return_buffer) = match self.mode {
            FakeViewGatewayMode::RefuseBefore(tip, _) if *query.block_timepoint < tip => {
                return Ok(ViewGatewayResult::ViewGatewayInvalidInput {
                    error_code: thruput_xdr::syscall_error::SyscallErrorCode(7),
                });
            }
            FakeViewGatewayMode::EchoTimepoint(height)
            | FakeViewGatewayMode::RefuseBefore(_, height) => (height, echo),
            FakeViewGatewayMode::EchoRising(start) => {
                (start + i64::try_from(earlier_views).unwrap(), echo)
            }
            _ => (42, vec![1, 2, 3]),
        };

        Ok(ViewGatewayResult::ViewGatewayOk {
            payload: ViewGatewayPayload {
                gas_consumed: query.invocation.gas_limit,
                height: height.into(),
                output: InvocationFrameOutput::Return {
                    return_buffer: BytesM(return_buffer),
                },
                events: VecM(vec![]),
            },
        })
    }
}

async fn start_fake_view_gateway(
    mode: FakeViewGatewayMode,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let (addr, handle, _) = start_counting_fake_view_gateway(mode).await;
    (addr, handle)
}

/// Start a fake view gateway that counts the single views it runs.
async fn start_counting_fake_view_gateway(
    mode: FakeViewGatewayMode,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bug: test ViewGateway should bind");
    let addr = listener
        .local_addr()
        .expect("bug: test ViewGateway should have local addr");
    let views = std::sync::Arc::default();
    let mut services = ServiceMap::new();
    services.add_service(Box::new(ViewGateway_ViewGateway1_Dispatcher::new(
        Box::new(FakeViewGateway {
            mode,
            views: std::sync::Arc::clone(&views),
        }),
    )));
    let server = RpcServer::new(listener, services);
    let handle = server.start().await;
    (addr, handle, views)
}

fn invocation() -> thruput_json_rpc_types::requests::Invocation {
    thruput_json_rpc_types::requests::Invocation {
        address: ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])),
        entrypoint: HexBytes(vec![8; 8]),
        calldata: HexBytes(vec![1, 2, 3]),
        caller: None,
        assumed_auth: None,
        gas_limit: None,
    }
}

#[tokio::test]
async fn batch_pending_io_respects_the_public_deadline() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::ViewSlow).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(30),
        crate::block_driver::SharedBlockClock::default(),
        None,
    )
    .await
    .unwrap();
    let result = backend
        .simulate_batch(
            RpcRequestContext::new(Some(Duration::from_millis(10))),
            thruput_json_rpc_types::requests::SimulateInvocationsRequest {
                invocations: vec![invocation()],
                min_state_block_height: None,
            },
            10_000_000,
            thruput_json_rpc_server::ViewBatchConfig::default(),
        )
        .await;
    assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
}

#[tokio::test]
async fn connect_optional_without_endpoint_uses_stub() {
    let backend = HogdDevViewBackend::connect_optional(
        None,
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(1),
        crate::block_driver::SharedBlockClock::default(),
        None,
    )
    .await
    .expect("missing endpoint should not fail startup");

    assert_eq!(
        backend.plane_status(),
        crate::server::PlaneStatus::Stub,
        "missing ViewGateway endpoint should degrade to stub"
    );
}

#[tokio::test]
async fn connect_to_dead_view_gateway_port_fails_startup_probe() {
    let dead_addr = crate::test_support::DEAD_LOOPBACK_ADDR;

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        HogdDevViewBackend::connect_optional(
            Some(dead_addr),
            &protocol_text(&PROTOCOL_VERSION),
            None,
            Duration::from_secs(1),
            crate::block_driver::SharedBlockClock::default(),
            None,
        ),
    )
    .await
    .expect("bug: connect to dead port should return promptly");

    // A concurrent test can rebind the freed port before we connect, in
    // which case the probe reaches a live listener and fails at the intro
    // RPC instead of the connect.
    assert!(
        matches!(
            result,
            Err(crate::NodeSupervisorError::ViewGatewayConnect { .. }
                | crate::NodeSupervisorError::ViewGatewayRpc {
                    op: "viewgateway_intro",
                    ..
                })
        ),
        "expected ViewGateway startup connection failure, got {result:?}"
    );
}

#[tokio::test]
async fn connect_to_intro_rejected_view_gateway_fails_startup_probe() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::IntroRejected).await;

    let result = HogdDevViewBackend::connect_optional(
        Some(addr),
        "hogp/sent00000000000",
        Some("hogp/expected0000000"),
        Duration::from_secs(1),
        crate::block_driver::SharedBlockClock::default(),
        None,
    )
    .await;

    let err = result.expect_err("intro rejection should fail startup");
    let message = err.to_string();
    match err {
        crate::NodeSupervisorError::ProtocolIntroRejected {
            service,
            outcome,
            outcome_name,
            is_protocol_mismatch,
            protocol,
            expected_protocol,
        } => {
            assert_eq!(service, "ViewGateway");
            assert_eq!(
                outcome,
                ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol.0
            );
            assert_eq!(outcome_name, "ViewGatewayIntroMismatchedProtocol");
            assert!(is_protocol_mismatch);
            assert_eq!(protocol, "hogp/sent00000000000");
            assert_eq!(expected_protocol.as_deref(), Some("hogp/expected0000000"));
            assert!(message.contains("ViewGateway intro mismatch"));
            assert!(message.contains("hogp/sent00000000000"));
            assert!(message.contains("hogp/expected0000000"));
            assert!(message.contains("ViewGatewayIntroMismatchedProtocol"));
        }
        other => panic!("expected ViewGateway intro rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn view_gateway_rpc_timeout_maps_to_backend_timeout() {
    let (addr, _server) = start_fake_view_gateway(FakeViewGatewayMode::ViewSlow).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_millis(10),
        crate::block_driver::SharedBlockClock::default(),
        None,
    )
    .await
    .expect("fake ViewGateway should connect");

    let result = backend
        .simulate(
            RpcRequestContext::unbounded(),
            thruput_json_rpc_types::requests::SimulateInvocationRequest {
                address: ContractAddress::from(thruput_xdr::types::ContractAddress::from([7; 32])),
                entrypoint: HexBytes(vec![8; 8]),
                calldata: HexBytes(vec![1, 2, 3]),
                caller: None,
                assumed_auth: None,
                gas_limit: None,
                min_state_block_height: None,
            },
            123,
        )
        .await;

    assert!(
        matches!(result, Err(RpcServerError::BackendTimeout)),
        "ViewGateway timeout should surface BackendTimeout, got {result:?}"
    );
}

#[tokio::test]
async fn native_local_simulation_preserves_snapshot_and_deadline() {
    use thruput_json_rpc_types::{requests, responses};
    for mode in [FakeViewGatewayMode::Batch, FakeViewGatewayMode::ViewSlow] {
        let (addr, task) = start_fake_view_gateway(mode).await;
        let backend = HogdDevViewBackend::connect_optional(
            Some(addr),
            &protocol_text(&PROTOCOL_VERSION),
            None,
            Duration::from_secs(30),
            crate::block_driver::SharedBlockClock::default(),
            None,
        )
        .await
        .unwrap();
        let request = requests::SimulateInvocationRequest {
            address: thruput_json_rpc_types::base58::Base58(
                thruput_xdr::types::ContractAddress::from([7; 32]),
            ),
            entrypoint: HexBytes(vec![8; 8]),
            calldata: HexBytes(vec![1, 2, 3]),
            caller: None,
            assumed_auth: None,
            gas_limit: Some(100),
            min_state_block_height: Some(42.into()),
        };
        // Only the slow gateway gets a short deadline; a tight one would also
        // cut off a healthy reply on a loaded machine.
        let context = if mode == FakeViewGatewayMode::ViewSlow {
            RpcRequestContext::new(Some(Duration::from_millis(50)))
        } else {
            RpcRequestContext::unbounded()
        };
        let result = backend.simulate(context, request, 100).await;
        if mode == FakeViewGatewayMode::ViewSlow {
            assert!(matches!(result, Err(RpcServerError::BackendTimeout)));
        } else {
            let result = result.unwrap();
            assert_eq!(*result.state_block_height, 42);
            assert_eq!(result.gas_used, 100);
            assert!(matches!(
                result.output,
                responses::InvocationOutput::Return { .. }
            ));
        }
        task.abort();
    }
}

#[tokio::test]
async fn native_local_batch_preserves_order_and_freshness_evidence() {
    use thruput_json_rpc_server::ViewBatchConfig;
    use thruput_json_rpc_types::{requests as input, responses};
    let (addr, task) = start_fake_view_gateway(FakeViewGatewayMode::Batch).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_mins(1),
        crate::block_driver::SharedBlockClock::default(),
        None,
    )
    .await
    .unwrap();
    for floor in [42, 43] {
        let request = input::SimulateInvocationsRequest {
            min_state_block_height: Some(floor.into()),
            invocations: [3, 4]
                .into_iter()
                .map(|weight| input::Invocation {
                    address: thruput_json_rpc_types::base58::Base58(
                        thruput_xdr::types::ContractAddress::from([7; 32]),
                    ),
                    entrypoint: HexBytes(vec![8; 8]),
                    calldata: HexBytes(vec![u8::try_from(weight).unwrap()]),
                    caller: None,
                    assumed_auth: Some(input::AssumedAuth::SourceAddress {
                        allowed_keys: 5,
                        required_weight: weight,
                    }),
                    gas_limit: Some(100 * u64::from(weight)),
                })
                .collect(),
        };
        let result = backend
            .simulate_batch(
                RpcRequestContext::unbounded(),
                request,
                1000,
                ViewBatchConfig {
                    max_response_bytes: 256,
                    ..Default::default()
                },
            )
            .await;
        if floor == 42 {
            let result = result.unwrap();
            assert_eq!(result.results.len(), 2);
            assert_eq!(*result.state_block_height, 42);
            for (index, item) in result.results.iter().enumerate() {
                assert_eq!(item.gas_used, 300 + 100 * u64::try_from(index).unwrap());
                assert!(
                    matches!(&item.output,responses::InvocationOutput::Return { return_data } if return_data.0 == vec![u8::try_from(index).unwrap() + 3])
                );
            }
        } else {
            assert!(matches!(
                result,
                Err(RpcServerError::ViewHeightUnavailable(_))
            ));
        }
    }
    task.abort();
}

/// A scheduler that serves only its tip: a height and timepoint, or an error
/// when `None`.
#[derive(Clone)]
struct FakeTipScheduler(Option<(i64, u64)>);

#[allow(refining_impl_trait)]
impl SchedulerCoord_SchedulerCoord1 for FakeTipScheduler {
    async fn schedcoord_get_tip(&self) -> Result<SchedulerGetTipResult, ProgramError> {
        let Some((height, timepoint)) = self.0 else {
            return Err(ProgramError::msg("fake get_tip failure"));
        };
        Ok(SchedulerGetTipResult::SchedulerGetTipOK {
            db_tip: db_tip(height, timepoint),
        })
    }
    async fn schedulercoord_null(&self) -> Result<(), ProgramError> {
        Ok(())
    }
    async fn schedulercoord_intro_coord(
        &self,
        _card: thruput_node_rpc::CoordCard,
    ) -> Result<thruput_node_rpc::CoordinatorRegistrationOutcome, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_submit_unvetted(
        &self,
        _batch: thruput_node_rpc::ParallelSignedTransactionBatch,
    ) -> Result<(), ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_submit_vetted(
        &self,
        _validator_id: thruput_xdr::validator::ValidatorID,
        _batch: thruput_node_rpc::ParallelSignedTransactionBatch,
    ) -> Result<(), ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_propose_block(
        &self,
        _onto: thruput_node_rpc::BlockID,
        _context: thruput_xdr::header::ProposalContext,
    ) -> Result<thruput_node_rpc::SchedulerProposalResult, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_validate_block(
        &self,
        _onto: thruput_node_rpc::BlockID,
        _proposal: thruput_node_rpc::Proposal,
    ) -> Result<thruput_node_rpc::SchedulerValidationResult, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_commit_block(
        &self,
        _block_id: thruput_node_rpc::BlockID,
        _value: thruput_node_rpc::ConsensusValue,
    ) -> Result<thruput_node_rpc::SchedulerCommitOutcome, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedcoord_load_genesis_configuration(
        &self,
        _config: thruput_xdr::genesis::GenesisConfiguration,
    ) -> Result<thruput_node_rpc::DbSchedResult, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedulercoord_commit_genesis_ceremony(
        &self,
        _request: thruput_node_rpc::GenesisCeremonyRequest,
    ) -> Result<thruput_node_rpc::GenesisCeremonyResult, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedcoord_lookup_transactions(
        &self,
        _hashes: thruput_xdr::transaction::SignedTransactionHashBatch,
    ) -> Result<thruput_xdr::transaction::OptSignedTransactionBatch, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
    async fn schedcoord_lookup_transaction_outcomes(
        &self,
        _hashes: thruput_xdr::transaction::TransactionHashBatch,
    ) -> Result<thruput_node_rpc::SchedulerTransactionOutcomeBatch, ProgramError> {
        Err(ProgramError::msg("not served"))
    }
}

fn db_tip(height: i64, timepoint: u64) -> thruput_node_rpc::DbTip {
    use thruput_xdr::types::{
        BlockHeight, ConfigHash, ConsensusValueHash, EventsHash, ExtensionPoint, NetworkHash,
        Protocol, SHA256Hash, StorageShortHash, TimePoint, TxsHash,
    };
    let hash = || SHA256Hash::from([0; 32]);
    thruput_node_rpc::DbTip {
        id: 0,
        storage_long_hash: [0u8; 2048].into(),
        default_wallet: thruput_xdr::contract::DeployedContractHash {
            hash: hash(),
            contract_size: 0,
            contract_type: thruput_xdr::contract::ContractType::kNative,
        },
        network_config: thruput_xdr::network_config::NetworkConfig::_0 {
            v0: thruput_xdr::network_config::NetworkConfigV0 {
                params: VecM(vec![]),
            },
        },
        value: thruput_node_rpc::ConsensusValue {
            protocol: Protocol::new(0),
            ext: ExtensionPoint::_0,
            prev_hash: ConsensusValueHash::new(hash()),
            proposal_header: thruput_xdr::header::ProposalHeader {
                ext: ExtensionPoint::_0,
                network_hash: NetworkHash::new(hash()),
                config_hash: ConfigHash::new(hash()),
                proposer: thruput_xdr::validator::ValidatorID::default(),
                block_timepoint: TimePoint::new(timepoint),
                block_height: BlockHeight::from(height),
                txs_hash: TxsHash::new(hash()),
                storage_hash: StorageShortHash::new(hash()),
                events_hash: EventsHash::new(hash()),
            },
            proposal_txs_hash: thruput_node_rpc::ProposalTxsHash::new(hash()),
            upgrades: VecM::default(),
        },
    }
}

async fn start_fake_tip_scheduler(
    height: i64,
    timepoint: u64,
) -> (
    SchedulerCoord_SchedulerCoord1_Client,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    start_scheduler(FakeTipScheduler(Some((height, timepoint)))).await
}

async fn start_scheduler(
    scheduler: FakeTipScheduler,
) -> (
    SchedulerCoord_SchedulerCoord1_Client,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut services = ServiceMap::new();
    services.add_service(Box::new(SchedulerCoord_SchedulerCoord1_Dispatcher::new(
        Box::new(scheduler),
    )));
    let handle = RpcServer::new(listener, services).start().await;
    let socket = rpc_driver::FixedSerialPool::connect(&addr, 1)
        .await
        .unwrap();
    (
        SchedulerCoord_SchedulerCoord1_Client::new(std::sync::Arc::new(socket)),
        handle,
    )
}

/// Run one local view against a fake view gateway in `mode`. Returns the
/// result and how many views the gateway ran.
async fn local_view(
    mode: FakeViewGatewayMode,
    clock: crate::block_driver::SharedBlockClock,
    scheduler: Option<SchedulerCoord_SchedulerCoord1_Client>,
) -> (
    Result<thruput_json_rpc_types::responses::SimulationResult, RpcServerError>,
    usize,
) {
    use thruput_json_rpc_types::requests;
    let (addr, task, views) = start_counting_fake_view_gateway(mode).await;
    let backend = HogdDevViewBackend::connect_optional(
        Some(addr),
        &protocol_text(&PROTOCOL_VERSION),
        None,
        Duration::from_secs(30),
        clock,
        scheduler,
    )
    .await
    .unwrap();
    let request = requests::SimulateInvocationRequest {
        address: thruput_json_rpc_types::base58::Base58(thruput_xdr::types::ContractAddress::from(
            [7; 32],
        )),
        entrypoint: HexBytes(vec![8; 8]),
        calldata: HexBytes(vec![1, 2, 3]),
        caller: None,
        assumed_auth: None,
        gas_limit: Some(100),
        min_state_block_height: None,
    };
    let result = backend
        .simulate(RpcRequestContext::unbounded(), request, 100)
        .await;
    task.abort();
    (result, views.load(std::sync::atomic::Ordering::SeqCst))
}

/// The timepoint an echoing fake returns for a local view.
fn echoed_timepoint(result: thruput_json_rpc_types::responses::SimulationResult) -> u64 {
    let thruput_json_rpc_types::responses::InvocationOutput::Return { return_data } = result.output
    else {
        panic!("bug: the fake returns the view's timepoint");
    };
    u64::from_be_bytes(return_data.0.as_slice().try_into().unwrap())
}

/// The timepoint a local view runs at, read back from the echoing fake.
async fn local_view_timepoint(
    snapshot_height: i64,
    clock: crate::block_driver::SharedBlockClock,
    scheduler: Option<SchedulerCoord_SchedulerCoord1_Client>,
) -> u64 {
    let mode = FakeViewGatewayMode::EchoTimepoint(snapshot_height);
    echoed_timepoint(local_view(mode, clock, scheduler).await.0.unwrap())
}

/// 2000-01-01, far behind the wall clock.
const Y2K: u64 = 946_684_800_000_000_000;
const SECOND: u64 = 1_000_000_000;

#[tokio::test]
async fn a_local_view_runs_at_the_next_block_time() {
    let clock = crate::block_driver::SharedBlockClock::frozen_at(Y2K);
    assert_eq!(local_view_timepoint(2, clock, None).await, Y2K);
}

#[tokio::test]
async fn the_first_view_on_a_restarted_chain_runs_at_the_saved_tip() {
    // Restarted frozen, with the saved tip at height 5 in 2000 and no block
    // produced since. The tip validated at startup dates the view, with no
    // scheduler read that could fail.
    let clock = crate::block_driver::SharedBlockClock::unseeded(0);
    clock.seed_tip(crate::block_driver::Parent {
        height: 5,
        timepoint: Y2K,
    });
    assert_eq!(local_view_timepoint(5, clock, None).await, Y2K);
}

#[tokio::test]
async fn without_a_known_tip_a_view_reads_it_from_the_scheduler() {
    let (scheduler, task) = start_fake_tip_scheduler(5, Y2K).await;
    let clock = crate::block_driver::SharedBlockClock::unseeded(0);
    assert_eq!(local_view_timepoint(5, clock, Some(scheduler)).await, Y2K);
    task.abort();
}

#[tokio::test]
async fn a_refusal_the_tip_does_not_explain_is_sent_once() {
    // A running clock gives a new time on every read, but the tip has not
    // moved past the stated time, so the refusal is returned as is.
    let (scheduler, task) = start_fake_tip_scheduler(2, Y2K).await;
    let clock = crate::block_driver::SharedBlockClock::default();
    clock.seed_tip(crate::block_driver::Parent {
        height: 2,
        timepoint: Y2K,
    });
    let mode = FakeViewGatewayMode::RefuseBefore(u64::MAX, 2);
    let (result, views) = local_view(mode, clock, Some(scheduler)).await;
    assert!(matches!(result, Err(RpcServerError::InvalidParam(_))));
    assert_eq!(views, 1);
    task.abort();
}

#[tokio::test]
async fn a_refusal_behind_a_later_tip_reruns_once_at_its_next_block_time() {
    // The time was chosen from block 4 in 2000. Block 5, a second later,
    // committed first, and hogd refuses a time before it.
    let (scheduler, task) = start_fake_tip_scheduler(5, Y2K + SECOND).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(0, 4, Y2K);
    let mode = FakeViewGatewayMode::RefuseBefore(Y2K + SECOND, 5);
    let (result, views) = local_view(mode, clock, Some(scheduler)).await;
    assert_eq!(echoed_timepoint(result.unwrap()), Y2K + SECOND);
    assert_eq!(views, 2);
    task.abort();
}

#[tokio::test]
async fn a_view_on_a_newer_snapshot_reruns_at_its_next_block_time() {
    // The time was chosen from height 4, stepping 1 s; block 5 committed
    // before hogd opened the snapshot. The view reruns at block 6's time.
    let (scheduler, task) = start_fake_tip_scheduler(5, Y2K + SECOND).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(SECOND, 4, Y2K);
    assert_eq!(
        local_view_timepoint(5, clock, Some(scheduler)).await,
        Y2K + 2 * SECOND
    );
    task.abort();
}

#[tokio::test]
async fn a_view_on_an_older_snapshot_keeps_its_time() {
    // The view gateway still serves block 4 after block 5 committed. The time
    // from block 5 is right, so the view does not rerun, even though the
    // scheduler would now give a different time.
    let (scheduler, task) = start_fake_tip_scheduler(6, Y2K + 2 * SECOND).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(SECOND, 5, Y2K + SECOND);
    assert_eq!(
        local_view_timepoint(4, clock, Some(scheduler)).await,
        Y2K + 2 * SECOND
    );
    task.abort();
}

#[tokio::test]
async fn a_rerun_overtaken_by_another_block_fails_instead_of_using_a_stale_time() {
    // The time was chosen from block 4, stepping 1 s. Block 5 committed before
    // the view's snapshot, so it reruns at block 6's time. Block 6 then
    // committed before the rerun's snapshot, so that time is stale too.
    let (scheduler, task) = start_fake_tip_scheduler(5, Y2K + SECOND).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(SECOND, 4, Y2K);
    let (result, views) =
        local_view(FakeViewGatewayMode::EchoRising(5), clock, Some(scheduler)).await;
    assert!(
        matches!(&result, Err(RpcServerError::BackendUnavailable(message)) if message.contains("retry")),
        "{result:?}"
    );
    assert_eq!(views, 2);
    task.abort();
}

#[tokio::test]
async fn a_view_on_a_newer_snapshot_fails_when_the_tip_cannot_be_read() {
    // Block 5 committed before the view's snapshot, so its time may be stale.
    // Without the tip, nothing confirms it, so the view fails.
    let (scheduler, task) = start_scheduler(FakeTipScheduler(None)).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(SECOND, 4, Y2K);
    let (result, views) = local_view(
        FakeViewGatewayMode::EchoTimepoint(5),
        clock,
        Some(scheduler),
    )
    .await;
    assert!(
        matches!(&result, Err(RpcServerError::BackendUnavailable(message)) if message.contains("tip could not be read")),
        "{result:?}"
    );
    assert_eq!(views, 1);
    task.abort();
}

#[tokio::test]
async fn a_refusal_stands_when_the_tip_cannot_be_read() {
    let (scheduler, task) = start_scheduler(FakeTipScheduler(None)).await;
    let clock = crate::block_driver::SharedBlockClock::stepped_from(SECOND, 4, Y2K);
    let mode = FakeViewGatewayMode::RefuseBefore(u64::MAX, 4);
    let (result, views) = local_view(mode, clock, Some(scheduler)).await;
    assert!(
        matches!(result, Err(RpcServerError::InvalidParam(_))),
        "{result:?}"
    );
    assert_eq!(views, 1);
    task.abort();
}
