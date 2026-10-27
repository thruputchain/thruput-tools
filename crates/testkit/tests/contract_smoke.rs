//! Live contract smoke.
//!
//! Launches a real local node, deploys the `caller-counter` fixture, and
//! exercises the register-once / deploy-by-hash path plus a `bump()`/`get()`
//! read-after-write round-trip.
//!
//! Requires a `hog-devnode` binary and a built `caller-counter` artifact, so
//! it is `#[ignore]`d and driven by `scripts/smoke-testkit-local.sh`.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use thruput_testkit::rpc_types::responses::InvocationOutput;
use thruput_testkit::rpc_types::responses::ReceiptExecution;
use thruput_testkit::xdr::types::ContractAddress;
use thruput_testkit::{
    Agent, AgentContext, BlockDriverConfig, DeployOptions, LocalNetworkConfig, NetworkConfig,
    Result, Scenario, TestkitError, WalletInitCall, entrypoint_id,
};
use tokio::sync::Barrier;

/// `caller-counter`'s interface identity, from which method selectors derive.
const INTERFACE: &str = "CallerCounter";
const VERSION: &str = "0.1.0";

/// The built fixture. These tests only run when asked for with `--ignored`, so
/// a missing fixture is a setup error to report, not a reason to pass.
fn caller_counter_artifact() -> PathBuf {
    let root = std::env::var("THRUPUT_TESTKIT_ARTIFACT_ROOT").expect(
        "set THRUPUT_TESTKIT_ARTIFACT_ROOT to a directory containing caller_counter.rv32a \
         (see scripts/smoke-testkit-local.sh)",
    );
    PathBuf::from(root).join("caller_counter.rv32a")
}

/// Local-node config taken from the smoke script's environment: the devnode
/// binary. The fixture is registered by path, so the devnode keeps its default
/// contract artifact roots.
fn local_config() -> LocalNetworkConfig {
    let mut config = LocalNetworkConfig::default();
    // config.log_level = Some(thruput_testkit::LogLevel::Trace);
    if let Ok(path) = std::env::var("THRUPUT_DEVNODE_BINARY") {
        config.hog_devnode_binary = Some(path.into());
    }
    config
}

/// Read the caller's counter via `simulateInvocation`, decoding the CXS `u64`.
///
/// `get()` keys on `msg.sender`, so the view must run with `caller` set to the
/// same wallet that issued the `bump()`; the node installs it as a parent frame.
async fn read_counter(
    ctx: &AgentContext,
    contract: ContractAddress,
    caller: ContractAddress,
) -> Result<u64> {
    let get = entrypoint_id(INTERFACE, VERSION, "get");
    let response = ctx
        .network()
        .view_call(contract, get, &[], Some(caller))
        .await?;
    match response.output {
        InvocationOutput::Return { return_data } => {
            Ok(thruput_testkit::xdrs::cxs::decode::<u64>(&return_data.0)
                .expect("get() return decodes as a u64"))
        }
        other => panic!("get() did not return a value: {other:?}"),
    }
}

async fn bump_counter(ctx: &AgentContext, contract: ContractAddress) -> Result<()> {
    let receipt = ctx
        .call(contract, entrypoint_id(INTERFACE, VERSION, "bump"), &[])
        .await?;
    assert!(
        matches!(receipt.execution, ReceiptExecution::Return { .. }),
        "bump reverted: {:?}",
        receipt.execution
    );
    Ok(())
}

async fn fund_native_accounts(scenario: &Scenario, manual: bool) {
    let barrier = Arc::new(Barrier::new(2));
    let agents = (0..2).map(|index| {
        let barrier = Arc::clone(&barrier);
        Agent::new(format!("fund-{index}"), move |ctx| async move {
            let submitted = ctx.submit_fund(1_000_000_000).await?;
            if manual {
                submitted.await_pending().await?;
                barrier.wait().await;
                if index == 0 {
                    ctx.network().produce_block().await?;
                }
                barrier.wait().await;
            }
            let receipt = submitted.await_receipt().await?;
            assert!(matches!(receipt.execution, ReceiptExecution::Return { .. }));
            Ok(())
        })
    });
    tokio::time::timeout(Duration::from_mins(1), scenario.run_agents(agents))
        .await
        .expect("funding phase does not deadlock")
        .expect("explicit native funding succeeds");
}

async fn run_non_conflicting_manual_calls(scenario: &Scenario, addresses: [ContractAddress; 2]) {
    let barrier = Arc::new(Barrier::new(2));
    let call_height = Arc::new(AtomicI64::new(-1));

    let alice_barrier = Arc::clone(&barrier);
    let alice_height = Arc::clone(&call_height);
    let alice_address = addresses[0];
    let alice = Agent::new("alice", move |ctx| async move {
        let call = ctx
            .submit_call(
                alice_address,
                entrypoint_id(INTERFACE, VERSION, "bump"),
                &[],
            )
            .await?;
        call.await_pending().await?;
        alice_barrier.wait().await;
        let block = ctx.network().produce_block().await?;
        alice_height.store(block.height, Ordering::Release);
        alice_barrier.wait().await;
        let receipt = call.await_receipt().await?;
        assert_eq!(*receipt.block_height, block.height);
        assert!(matches!(receipt.execution, ReceiptExecution::Return { .. }));
        assert_eq!(read_counter(&ctx, alice_address, ctx.address()).await?, 1);
        Ok(())
    });

    let bob_barrier = Arc::clone(&barrier);
    let bob_height = Arc::clone(&call_height);
    let bob_address = addresses[1];
    let bob = Agent::new("bob", move |ctx| async move {
        let call = ctx
            .submit_call(bob_address, entrypoint_id(INTERFACE, VERSION, "bump"), &[])
            .await?;
        call.await_pending().await?;
        bob_barrier.wait().await;
        bob_barrier.wait().await;
        let height = bob_height.load(Ordering::Acquire);
        let receipt = call.await_receipt().await?;
        assert_eq!(*receipt.block_height, height);
        assert!(matches!(receipt.execution, ReceiptExecution::Return { .. }));
        assert_eq!(read_counter(&ctx, bob_address, ctx.address()).await?, 1);
        Ok(())
    });

    tokio::time::timeout(Duration::from_secs(30), scenario.run_agents([alice, bob]))
        .await
        .expect("manual call phase does not deadlock")
        .expect("non-conflicting calls commit in one manual block");
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY and a built caller-counter fixture in THRUPUT_TESTKIT_ARTIFACT_ROOT"]
async fn caller_counter_deploy_call_read() {
    let artifact = caller_counter_artifact();

    let config = local_config();

    let scenario = Scenario::builder("caller-counter-smoke")
        .network(NetworkConfig::local(config))
        .artifact("caller-counter", artifact)
        .build()
        .await
        .expect("local node starts and advertises a runtime manifest");

    fund_native_accounts(&scenario, false).await;
    scenario
        .run_agents([Agent::new("alice", |mut ctx| async move {
            // The first deploy carries the binary (registers it) and creates an
            // instance; the harness reports it registered.
            let instance1 = ctx
                .deploy_artifact(
                    "caller-counter",
                    DeployOptions {
                        amount: 100_000_000,
                        initializer: Some(WalletInitCall {
                            entrypoint: entrypoint_id(INTERFACE, VERSION, "bump"),
                            gas_limit: 2_000_000,
                            calldata: &[],
                        }),
                    },
                )
                .await?;
            assert!(instance1.registered, "first deploy registers the binary");
            assert!(ctx.is_registered("caller-counter")?);
            assert_eq!(
                read_counter(&ctx, instance1.address, ctx.address()).await?,
                1
            );

            // The second deploy is hash-only and yields a distinct instance.
            let instance2 = ctx
                .deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await?;
            assert!(!instance2.registered, "second deploy is hash-only");
            assert_ne!(
                instance1.address, instance2.address,
                "each deploy is a distinct instance"
            );

            // Write/read round-trip on the first instance as this agent's wallet.
            let caller = ctx.address();

            bump_counter(&ctx, instance1.address).await?;
            let after_first = read_counter(&ctx, instance1.address, caller).await?;

            bump_counter(&ctx, instance1.address).await?;
            let after_second = read_counter(&ctx, instance1.address, caller).await?;

            assert!(after_first >= 1, "counter is at least 1 after one bump");
            assert_eq!(
                after_second,
                after_first + 1,
                "each bump increments the caller's counter"
            );
            Ok(())
        })])
        .await
        .expect("agent runs to completion");

    scenario.stop().await.expect("clean teardown");
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY and a built caller-counter fixture in THRUPUT_TESTKIT_ARTIFACT_ROOT"]
async fn multiple_runs_are_possible() {
    let artifact = caller_counter_artifact();

    let config = local_config();

    let scenario = Scenario::builder("multiple-runs")
        .network(NetworkConfig::local(config))
        .artifact("caller-counter", artifact)
        .build()
        .await
        .expect("local node starts and advertises a runtime manifest");

    fund_native_accounts(&scenario, false).await;
    let make_agents = || {
        [Agent::new("alice", |mut ctx| async move {
            // The first deploy carries the binary (registers it) and creates an
            // instance; the harness reports it registered.
            let caller_counter = ctx
                .deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await?;

            bump_counter(&ctx, caller_counter.address).await?;

            Ok(())
        })]
    };

    scenario
        .run_agents(make_agents())
        .await
        .expect("agent runs to completion");

    scenario
        .run_agents(make_agents())
        .await
        .expect("agent runs to completion (second time against same scenario)");

    scenario.stop().await.expect("clean teardown");
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY and a built caller-counter fixture in THRUPUT_TESTKIT_ARTIFACT_ROOT"]
async fn later_run_does_not_recover_an_earlier_runs_pending_registration() {
    let artifact = caller_counter_artifact();

    let mut config = local_config();
    config.block_driver = BlockDriverConfig::manual();

    let scenario = Scenario::builder("registration-across-runs")
        .network(NetworkConfig::local(config))
        .artifact("caller-counter", artifact)
        .build()
        .await
        .expect("manual local node starts");

    fund_native_accounts(&scenario, true).await;
    let first_address = Arc::new(Mutex::new(None));
    let stored_address = Arc::clone(&first_address);
    scenario
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let registration = ctx
                .submit_deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await?;
            registration.await_pending().await?;
            *stored_address
                .lock()
                .expect("address mutex is not poisoned") = Some(registration.address());
            Ok(())
        })])
        .await
        .expect("first run stages registration");

    let original_address = first_address
        .lock()
        .expect("address mutex is not poisoned")
        .expect("first run records its deployment address");
    scenario
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let registration_hash = match ctx
                .submit_deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await
            {
                Err(TestkitError::ArtifactRegistrationPending { tx_hash, .. }) => tx_hash,
                other => panic!("expected earlier run's pending registration, got {other:?}"),
            };

            let registration_block = ctx.network().produce_block().await?;
            let registration_receipt = ctx.network().await_receipt(registration_hash).await?;
            assert_eq!(
                *registration_receipt.block_height,
                registration_block.height
            );

            let deployment = ctx
                .submit_deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await?;
            assert_ne!(
                deployment.address(),
                original_address,
                "the later run must submit its own hash-only deployment"
            );
            deployment.await_pending().await?;
            let deployment_block = ctx.network().produce_block().await?;
            let deployed = deployment.await_receipt().await?;
            assert_eq!(*deployed.receipt.block_height, deployment_block.height);
            assert!(!deployed.registered);
            Ok(())
        })])
        .await
        .expect("later run deploys by hash after the earlier registration commits");

    scenario.stop().await.expect("clean manual-node teardown");
}

#[tokio::test]
#[ignore = "requires THRUPUT_DEVNODE_BINARY and a built caller-counter fixture in THRUPUT_TESTKIT_ARTIFACT_ROOT"]
async fn manual_block_production_stages_deploys_and_calls() {
    let artifact = caller_counter_artifact();

    let mut config = local_config();
    config.block_driver = BlockDriverConfig::manual();

    let scenario = Scenario::builder("manual-block-production")
        .network(NetworkConfig::local(config))
        .artifact("caller-counter", artifact)
        .build()
        .await
        .expect("manual local node starts");

    fund_native_accounts(&scenario, true).await;
    let addresses = Arc::new(Mutex::new(Vec::new()));
    let deployment_addresses = Arc::clone(&addresses);
    scenario
        .run_agents([Agent::new("alice", move |mut ctx| async move {
            let genesis = ctx.network().chain_tip().await?;
            let registration = ctx
                .submit_deploy_artifact("caller-counter", DeployOptions::funded(100_000_000))
                .await?;
            registration.await_pending().await?;
            assert_eq!(
                ctx.network().chain_tip().await?.block_height,
                genesis.block_height,
                "manual mode must not commit the submitted deployment"
            );

            let registration_block = ctx.network().produce_block().await?;
            let registered = registration.await_receipt().await?;
            assert_eq!(*registered.receipt.block_height, registration_block.height);
            assert!(registered.registered);
            deployment_addresses
                .lock()
                .expect("address mutex is not poisoned")
                .push(registered.address);

            let second = ctx
                .submit_deploy_registered("caller-counter", DeployOptions::funded(100_000_000))
                .await?;
            second.await_pending().await?;
            assert_eq!(
                *ctx.network().chain_tip().await?.block_height,
                registration_block.height,
                "staged deployment must remain pending"
            );

            let deployment_block = ctx.network().produce_block().await?;
            let second = second.await_receipt().await?;
            assert_eq!(*second.receipt.block_height, deployment_block.height);
            deployment_addresses
                .lock()
                .expect("address mutex is not poisoned")
                .push(second.address);
            Ok(())
        })])
        .await
        .expect("manual deployment phase completes");

    let addresses: [ContractAddress; 2] = addresses
        .lock()
        .expect("address mutex is not poisoned")
        .clone()
        .try_into()
        .expect("deployment phase should produce two addresses");
    run_non_conflicting_manual_calls(&scenario, addresses).await;

    scenario.stop().await.expect("clean manual-node teardown");
}
