//! Phase 1 harness tests.
//!
//! The offline tests exercise the harness machinery — explicit fixture,
//! concurrent agents, account assignment, deterministic randomness, and
//! off-chain messaging — with no node, so they run in CI. The live smoke needs
//! a real `hog-devnode` and is `#[ignore]`d.

use groundhog_testkit::{
    Agent, ArtifactKind, LocalTargetConfig, Scenario, TargetConfig, TestkitError,
};

#[tokio::test]
async fn offline_two_agents_exchange_a_message() {
    let scenario = Scenario::builder("two-agents-v1")
        .target(TargetConfig::offline(2))
        .build()
        .await
        .expect("offline scenario builds");

    let report = scenario
        .run_agents([
            Agent::new("alice", |mut ctx| async move {
                // Draw from the deterministic per-agent stream, then greet Bob.
                let nonce = ctx.rng().next_u64();
                ctx.send("bob", &nonce)?;
                Ok(())
            }),
            Agent::new("bob", |mut ctx| async move {
                let msg = ctx
                    .recv::<u64>()
                    .await
                    .expect("alice sends exactly one message")?;
                assert_eq!(msg.from, "alice");
                Ok(())
            }),
        ])
        .await
        .expect("agents run to completion");

    assert!(report.all_ok(), "every agent returned Ok");
    scenario.stop().await.expect("clean teardown");
}

#[tokio::test]
async fn offline_agents_get_distinct_accounts() {
    let scenario = Scenario::builder("accounts-v1")
        .target(TargetConfig::offline(3))
        .build()
        .await
        .expect("offline scenario builds");

    let report = scenario
        .run_agents([
            Agent::new("a0", |ctx| async move {
                assert_eq!(ctx.account_index(), 0);
                Ok(())
            }),
            Agent::new("a1", |ctx| async move {
                assert_eq!(ctx.account_index(), 1);
                Ok(())
            }),
        ])
        .await
        .expect("agents run to completion");

    assert!(report.all_ok());
    scenario.stop().await.expect("clean teardown");
}

#[tokio::test]
async fn offline_chain_verbs_report_offline() {
    let scenario = Scenario::builder("offline-verbs")
        .target(TargetConfig::offline(1))
        .build()
        .await
        .expect("offline scenario builds");

    let report = scenario
        .run_agents([Agent::new("solo", |mut ctx| async move {
            let err = ctx
                .deploy(b"not-a-real-artifact", ArtifactKind::Wasm)
                .await
                .expect_err("deploy against an offline target must fail");
            assert!(matches!(err, TestkitError::Offline { .. }));
            Ok(())
        })])
        .await
        .expect("agents run to completion");

    assert!(report.all_ok());
}

#[tokio::test]
async fn too_many_agents_for_available_accounts_is_an_error() {
    let scenario = Scenario::builder("too-few")
        .target(TargetConfig::offline(1))
        .build()
        .await
        .expect("offline scenario builds");

    let result = scenario
        .run_agents([
            Agent::new("a", |ctx| async move {
                let _ = ctx;
                Ok(())
            }),
            Agent::new("b", |ctx| async move {
                let _ = ctx;
                Ok(())
            }),
        ])
        .await;

    assert!(matches!(
        result,
        Err(TestkitError::NotEnoughAccounts {
            needed: 2,
            available: 1
        })
    ));
}

#[tokio::test]
async fn existing_target_missing_manifest_fails_build() {
    let result = Scenario::builder("missing-manifest")
        .target(TargetConfig::existing(
            "http://127.0.0.1:9545",
            "/nonexistent/runtime-manifest.json",
        ))
        .build()
        .await;

    assert!(matches!(result, Err(TestkitError::ManifestLoad { .. })));
}

#[tokio::test]
async fn registry_exposes_hash_and_registration_state() {
    let scenario = Scenario::builder("registry-v1")
        .target(TargetConfig::offline(1))
        .artifact_bytes(
            "counter",
            b"fake-contract-bytes".to_vec(),
            ArtifactKind::Wasm,
        )
        .build()
        .await
        .expect("offline scenario builds with a registered artifact");

    let report = scenario
        .run_agents([Agent::new("solo", |mut ctx| async move {
            // The contract hash is available before any deploy; nothing is
            // registered on-network yet.
            ctx.contract_hash("counter")?;
            assert!(!ctx.is_registered("counter")?);
            assert!(matches!(
                ctx.contract_hash("missing"),
                Err(TestkitError::UnknownArtifact { .. })
            ));
            // Deploying needs a live target.
            assert!(matches!(
                ctx.deploy_artifact("counter").await,
                Err(TestkitError::Offline { .. })
            ));
            Ok(())
        })])
        .await
        .expect("agents run to completion");

    assert!(report.all_ok());
}

#[tokio::test]
async fn duplicate_artifact_name_fails_build() {
    let result = Scenario::builder("dupe")
        .target(TargetConfig::offline(1))
        .artifact_bytes("c", b"a".to_vec(), ArtifactKind::Wasm)
        .artifact_bytes("c", b"b".to_vec(), ArtifactKind::Rv32)
        .build()
        .await;

    assert!(matches!(
        result,
        Err(TestkitError::DuplicateArtifact { .. })
    ));
}

#[tokio::test]
async fn unknown_artifact_extension_fails_build() {
    let result = Scenario::builder("ext")
        .target(TargetConfig::offline(1))
        .artifact("weird", "contract.txt")
        .build()
        .await;

    assert!(matches!(
        result,
        Err(TestkitError::UnknownArtifactExtension { .. })
    ));
}

#[tokio::test]
async fn missing_artifact_file_fails_build() {
    let result = Scenario::builder("missing-file")
        .target(TargetConfig::offline(1))
        .artifact("counter", "/nonexistent/counter.rv32a")
        .build()
        .await;

    assert!(matches!(result, Err(TestkitError::ArtifactLoad { .. })));
}

/// Live end-to-end smoke: launch a real local node, run two agents that read
/// chain state, coordinate off-chain, and receive distinct funded accounts.
/// Requires a `hog-devnode` binary (via `GROUNDHOG_DEVNODE_BINARY` or `PATH`).
#[tokio::test]
#[ignore = "requires a hog-devnode binary (set GROUNDHOG_DEVNODE_BINARY)"]
async fn local_two_agents_live_smoke() {
    let mut config = LocalTargetConfig::default();
    if let Ok(path) = std::env::var("GROUNDHOG_DEVNODE_BINARY") {
        config.hog_devnode_binary = Some(path.into());
    }

    let scenario = Scenario::builder("local-live-smoke")
        .target(TargetConfig::local(config))
        .build()
        .await
        .expect("local node starts and advertises a runtime manifest");

    let report = scenario
        .run_agents([
            Agent::new("alice", |ctx| async move {
                // Prove the RPC client works end to end.
                let _tip = ctx.network().chain_tip().await?;
                ctx.send("bob", &"hi".to_string())?;
                Ok(())
            }),
            Agent::new("bob", |mut ctx| async move {
                let _tip = ctx.network().chain_tip().await?;
                let msg = ctx.recv::<String>().await.expect("alice greets bob")?;
                assert_eq!(msg.from, "alice");
                assert_eq!(msg.body, "hi");
                Ok(())
            }),
        ])
        .await
        .expect("agents run to completion");

    assert!(report.all_ok());
    scenario.stop().await.expect("clean teardown");
}
