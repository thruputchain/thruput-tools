//! A harness for writing multi-agent integration tests as ordinary application
//! code.
//!
//! A test builds a [`Scenario`] against an explicit [`NetworkConfig`], hands it
//! a set of [`Agent`]s, and asserts on what they observe. Test scenarios are
//! run in `#[tokio::test]` functions.
//!
//! Each agent receives a deterministic native account derived from the scenario
//! seed. Agents [`deploy`] contracts with explicit funding and optional
//! initialization, [`call`] them, await receipts, and read chain state, in
//! addition to coordinating with other agents off-chain and using the virtual
//! clock and deterministic randomness.
//!
//! [`deploy`]: AgentContext::deploy_artifact
//! [`call`]: crate::contract
//!
//! ```rust,no_run
//! use thruput_testkit::{Agent, DeployOptions, LocalNetworkConfig, NetworkConfig, Scenario};
//!
//! # async fn example() -> thruput_testkit::Result<()> {
//! let mut config = LocalNetworkConfig::default();
//! config.hog_devnode_binary = Some("/path/to/supporting/hog-devnode".into());
//! config.contract_artifact_directories.push("/path/to/built_contracts".into());
//! let scenario = Scenario::builder("counter-test")
//!     .network(NetworkConfig::local(config))
//!     .artifact("counter", "target/thruput/counter.rv32a")
//!     .build()
//!     .await?;
//!
//! scenario.run_agents([Agent::new("alice", |mut ctx| async move {
//!     ctx.fund(1_000_000_000).await?; // 100 native tokens from the bootstrap faucet
//!     let deployed = ctx.deploy_artifact("counter", DeployOptions::funded(100_000_000)).await?;
//!     assert!(deployed.registered);
//!     Ok(())
//! })]).await?;
//! scenario.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Networks and accounts
//!
//! [`NetworkConfig::existing`] attaches to a running node,
//! [`NetworkConfig::local`] launches and owns an ephemeral `hog-devnode`, and
//! [`NetworkConfig::offline`] runs agents with locally-derived accounts and no chain
//! (for coordination/timing/messaging tests). [`Scenario::run_agents`] returns an
//! error when any agent returns an error, so the ordinary `.await?` test path cannot
//! silently ignore agent failures.
//!
//! Before using a live network, the scenario checks that the endpoint is
//! compatible and serves the network hash named in its runtime manifest, so
//! nothing is signed for a different network.
//!
//! The scenario owns its fixture accounts' keys, derived from its seed;
//! [`ScenarioBuilder::accounts`] sets how many there are. A local node receives
//! only their public allocations, of
//! [`LocalNetworkConfig::initial_account_tokens`] raw native token units each
//! (100 native tokens by default). On an existing network the accounts start
//! empty: fund them explicitly with [`AgentContext::fund`] or
//! [`AgentContext::submit_fund`].
//!
//! # Manual blocks and uncertain submissions
//!
//! [`AgentContext::submit_fund`], [`AgentContext::submit_deploy_artifact`] and
//! the `submit` forms of calls return handles without waiting, for tests that
//! drive blocks by hand (see [`BlockDriverConfig`]): await pending, produce a
//! block, then await the receipt. Receipt waits are read-only.
//!
//! Each submission reserves its nonce before dispatch. When a submission's
//! outcome is uncertain, the harness never resends it: the error retains the
//! original transaction hash and exact signed bytes for reconciliation
//! ([`TestkitError::Submission`], or [`TestkitError::ReceiptWait`] for a wait
//! that stopped), as does a [`SubmittedTransaction`]. In particular:
//!
//! - A deployment is accepted only once its successful receipt matches the
//!   original hash and predicted address; until then
//!   [`TestkitError::UnverifiedDeployment`] retains both. A new request for
//!   the same artifact with different funding or initialization is refused
//!   ([`TestkitError::DeploymentRequestMismatch`]) until the retained attempt
//!   is resolved, and scheduler statuses never replay or discard it.
//! - Local funding is retained on the [`Network`] handle before it is sent.
//!   After an uncertain response, retry the same recipient and amount on that
//!   handle, and keep it alive during recovery; a changed amount is refused
//!   ([`TestkitError::FundingRequestMismatch`]). Conflicting or invalid funding
//!   evidence is kept inspectable in [`TestkitError::InvalidFundingEvidence`]
//!   or [`TestkitError::InvalidFundingResponse`]; it does not prove admission.
//!
//! # Block production and block time
//!
//! On a local node, in every [`BlockDriverMode`], [`Network`] can produce
//! blocks and move block time forward, so a test can cover days of chain time
//! in seconds. Contracts read the block timepoint in Unix nanoseconds.
//!
//! - [`Network::produce_block`] produces one block now, which can be empty.
//! - [`Network::produce_block_at`] produces one at an exact timepoint.
//! - [`Network::produce_blocks`] produces several, optionally an interval apart.
//! - [`Network::set_next_block_timepoint`] pins the next block's timepoint.
//! - [`Network::increase_time`] moves the clock forward without a block.
//! - [`Network::set_block_driver`] switches between instant, interval and
//!   manual block production.
//!
//! After a time jump, later blocks continue from the new time. Blocks never
//! move backward: a timepoint earlier than the chain tip's is refused. These
//! controls call the node's `dev_` JSON-RPC methods, so they work the same on
//! a harness-owned node and on an attached `thruput node`. Hosted networks do
//! not offer them.
//!
//! ```rust,no_run
//! # async fn example(ctx: thruput_testkit::AgentContext) -> thruput_testkit::Result<()> {
//! use std::time::Duration;
//!
//! // Thirty days pass; an empty block makes the new time visible to views.
//! ctx.network().increase_time(Duration::from_hours(30 * 24)).await?;
//! let block = ctx.network().produce_block().await?;
//! println!("block {} at {}", block.height, *block.timepoint);
//! # Ok(())
//! # }
//! ```
//!
//! # Typed calls
//!
//! With `thruput-sdk`'s `remote` feature (typically on a dev-dependency),
//! `#[contract]` generates an `<Iface>Remote` proxy for each interface. Bound
//! to an agent's [`Caller`], each of its methods encodes the arguments and
//! returns a prepared invocation: a [`ContractView`] for a method declared
//! `pure` or `readOnly`, which awaiting simulates with the agent as
//! `msg.sender`, or otherwise a [`ContractCall`], which awaiting submits
//! through the agent's wallet. Either decodes the method's return type. A
//! method with no effect annotation gets the same proxy method as one declared
//! `stateChanging`, so the annotation can be omitted; the omission is only
//! visible in the contract's spec, where the effect is recorded as undeclared.
//!
//! ```rust,no_run
//! # #![allow(unexpected_cfgs)]
//! mod counter {
//!     use thruput_sdk::contract;
//!
//!     #[contract(version = "0.1.0")]
//!     pub trait Counter {
//!         /// Add `by` to the count, returning the new count.
//!         fn increment(&mut self, by: u64) -> u64;
//!
//!         #[contract(effect = "readOnly")]
//!         fn get(&self) -> u64;
//!     }
//! }
//!
//! use counter::CounterRemote;
//! use thruput_testkit::Agent;
//! use thruput_testkit::xdr::types::ContractAddress;
//!
//! fn alice(address: ContractAddress) -> Agent {
//!     Agent::new("alice", move |ctx| async move {
//!         let counter = CounterRemote::new(ctx.caller().clone(), &address);
//!         let count = counter.increment(1).await?; // submitted
//!         assert_eq!(counter.get().await?, count); // simulated
//!         Ok(())
//!     })
//! }
//! # fn main() {}
//! ```
//!
//! See [`contract`] for receipts, deferred submission, and reads through
//! methods without a declared effect, and [`Contract`] for calling a method by
//! name when no interface crate is at hand.

mod account;
pub mod agent;
mod artifact;
pub mod caller;
pub mod clock;
pub mod contract;
pub mod error;
mod logging;
pub mod mailbox;
mod manifest;
pub mod network;
mod nonce;
pub mod rng;
pub mod scenario;

pub use account::{DeployOptions, Deployed, SubmittedDeploy};
pub use agent::{Agent, AgentContext, AgentId};
pub use artifact::ArtifactKind;
pub use caller::Caller;
pub use clock::Clock;
pub use contract::{
    Contract, ContractCall, ContractEntrypointID, ContractView, SubmittedContractCall,
    entrypoint_id, try_entrypoint_id,
};
pub use error::{AgentFailure, AgentFailures, Result, TestkitError};
pub use logging::init_tracing;
pub use mailbox::{Envelope, Mailbox, Message};
pub use network::{
    LocalNetworkConfig, Network, NetworkConfig, ProducedBlock, SubmittedTransaction,
};
pub use rng::AgentRng;
pub use scenario::{AgentOutcome, Scenario, ScenarioBuilder, ScenarioReport};
pub use thruput_node_supervisor::{BlockDriverConfig, BlockDriverMode, LogLevel};
pub use thruput_signing::default_wallet::WalletInitCall;

// Re-exports so agent code can name RPC/XDR types and encode calldata without
// taking direct dependencies on these crates.
pub use thruput_json_rpc_types as rpc_types;
pub use thruput_xdr as xdr;
pub use xdrs;
