# thruput-testkit

Run concurrent contract tests against an existing RPC or a harness-owned local
node. Each agent receives a deterministic native account derived from the
scenario seed. Managed local nodes allocate the configured genesis balance to
these accounts (100 native tokens by default). On existing networks, accounts
start empty and funding is explicit.

```rust,no_run
use thruput_testkit::{Agent, DeployOptions, LocalNetworkConfig, NetworkConfig, Scenario};

# async fn example() -> thruput_testkit::Result<()> {
let mut config = LocalNetworkConfig::default();
config.hog_devnode_binary = Some("/path/to/supporting/hog-devnode".into());
config.contract_artifact_directories.push("/path/to/built_contracts".into());
let scenario = Scenario::builder("counter-test")
    .network(NetworkConfig::local(config))
    .artifact("counter", "target/thruput/counter.rv32a")
    .build()
    .await?;

scenario.run_agents([Agent::new("alice", |mut ctx| async move {
    ctx.fund(1_000_000_000).await?; // 100 native tokens from the bootstrap faucet
    let deployed = ctx.deploy_artifact("counter", DeployOptions::funded(100_000_000)).await?;
    assert!(deployed.registered);
    Ok(())
})]).await?;
scenario.stop().await?;
# Ok(())
# }
```

Native deployment uses `DefaultWallet.deploy(nonce, contract_hash, amount, initializer)`.
Local nodes emit schema-4 public allocations. The scenario owns fixture keys derived from its seed and supplies their public allocations before startup. `accounts(count)` selects the fixture count (default 10); `LocalNetworkConfig.initial_account_tokens` selects their initial raw native token allocation.

Every deploy helper takes `DeployOptions`. Amounts are exact raw native token
units, including explicit zero. An optional `WalletInitCall` supplies an
entrypoint, gas budget, and encoded calldata; the initializer runs as the wallet
against the new contract. Funding, deployment, and initialization commit
atomically. Registered artifacts carry their code once, then deploy by hash.

`submit_fund`, `submit_deploy_artifact`, and `submit_call` return handles for manual
block tests: await pending, produce a block, then await the receipt. Submissions
reserve their nonce before dispatch. Ambiguous submission errors retain the
original hash and signed bytes, and pending recovery never automatically resends.
Unverified deployment errors also retain the predicted address and signed bytes.
A deployment is accepted only after its successful receipt matches the original
hash and predicted address. Changing a retained attempt's funding or initializer
requires resolving that attempt first.

`bootstrap_accounts()` is an explicit hosted schema-3 adapter. Local manifests contain public keys only and cannot supply signing keys. For existing local nodes, use scenario-owned accounts and explicitly call `fund` or `submit_fund` as needed.

Local funding uses `dev_fundAccount`. The network handle retains each request before sending. After an uncertain response, retry the same recipient and amount on that handle; a changed amount is refused until the request is reconciled. Keep the handle alive during recovery. The handle retains admitted hash and signed bytes until an authoritative receipt, including errors or cancellation in `fund()`. `SubmittedTransaction` also exposes those bytes. `TestkitError::Submission` exposes retained hash and exact bytes. Conflicting or invalid funding evidence stays inspectable in `InvalidFundingEvidence` or `InvalidFundingResponse`. This evidence does not prove admission. This preserves submit → await pending → produce block → receipt in manual mode.

The network requires native wire major 14 and verifies the manifest network hash. Receipt waits are read-only. `TestkitError::ReceiptWait` retains commitment, scheduler observations and read failures. Retry or dropped statuses never resubmit or discard a retained artifact registration; reconcile its original hash.

The live tests in `tests/contract_smoke.rs` cover atomic initialization,
registration and hash-only deployment, native calls, manual blocks, and recovery
across scenario runs. Run `scripts/smoke-testkit-local.sh` with a supporting
`THRUPUT_DEVNODE_BINARY` and a built caller-counter fixture.
