//! Repeated connections to one live network must each execute explicit funding.

use std::sync::{Arc, Mutex};

use groundhog_json_rpc_client::GroundhogApiClient;
use groundhog_node_supervisor::{HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle};
use groundhog_testkit::rpc_types::hex::Hex0x;
use groundhog_testkit::{Agent, NetworkConfig, Scenario};

#[tokio::test]
#[ignore = "requires GROUNDHOG_DEVNODE_BINARY"]
async fn repeated_existing_scenarios_fund_the_same_native_account() {
    const AMOUNT: u128 = 100_000_000;
    let mut hogd = HogdDevConfig::default();
    if let Ok(binary) = std::env::var("GROUNDHOG_DEVNODE_BINARY") {
        hogd = hogd.with_binary_path(binary);
    }
    if let Ok(path) = std::env::var("GROUNDHOG_TESTKIT_CONTRACT_ARTIFACTS_ROOT") {
        hogd = hogd.with_contract_artifact_directories(vec![std::path::PathBuf::from(path)]);
    }
    let mut config = NodeSupervisorConfig::default()
        .with_rpc_listen_address("127.0.0.1:0")
        .with_hogd_dev(hogd);
    if let Ok(protocol) = std::env::var("HOGD_PROTOCOL_OVERRIDE") {
        config = config.with_hogd_protocol_override(protocol);
    }
    let node = NodeSupervisorHandle::start(config).await.unwrap();
    let rpc_url = format!("http://{}", node.rpc_addr());
    let manifest = node.hogd_readiness().manifest_path.as_ref().unwrap();
    let observations = Arc::new(Mutex::new(Vec::new()));

    for _ in 0..2 {
        let scenario = Scenario::builder("repeated-existing-funding")
            .network(NetworkConfig::existing(&rpc_url, manifest))
            .build()
            .await
            .unwrap();
        let observations = observations.clone();
        scenario
            .run_agents([Agent::new("alice", move |ctx| async move {
                let receipt = ctx.fund(AMOUNT).await?;
                let balance = ctx
                    .network()
                    .client()
                    .unwrap()
                    .get_balance(Hex0x(ctx.address()))
                    .await
                    .unwrap();
                observations.lock().unwrap().push((
                    ctx.address(),
                    receipt.tx_hash,
                    balance.raw_balance.parse::<u128>().unwrap(),
                ));
                Ok(())
            })])
            .await
            .unwrap();
        scenario.stop().await.unwrap();
    }
    {
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].0, observations[1].0);
        assert_ne!(observations[0].1, observations[1].1);
        assert_eq!(observations[1].2 - observations[0].2, AMOUNT);
    }
    node.stop().await.unwrap();
}
