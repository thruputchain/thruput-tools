//! A harness for writing multi-agent integration tests as ordinary application
//! code.
//!
//! A test builds a [`Scenario`] against an explicit [`NetworkConfig`], hands it
//! a set of [`Agent`]s, and asserts on what they observe. Test scenarios are
//! run in `#[tokio::test]` functions.
//!
//! Each agent receives a deterministic native account: a local node funds it
//! at genesis, and an existing network leaves it empty until [`AgentContext::fund`]
//! tops it up. Then [`deploy`] contracts with explicit funding
//! and optional initialization, [`call`] them, await receipts, and read chain state,
//! in addition to coordinating with other agents off-chain and using the
//! virtual clock and deterministic randomness.
//!
//! [`deploy`]: AgentContext::deploy
//! [`call`]: AgentContext::call
//!
//! Networks: [`NetworkConfig::existing`] attaches to a running node,
//! [`NetworkConfig::local`] launches and owns an ephemeral `hog-devnode`, and
//! [`NetworkConfig::offline`] runs agents with locally-derived accounts and no
//! chain (for coordination/timing/messaging tests). See the
//! [testkit design](https://github.com/thruputchain/thruput-tools/blob/main/docs/testkit/DESIGN.md).
//! [`Scenario::run_agents`] returns an error when any agent returns an error, so
//! the ordinary `.await?` test path cannot silently ignore agent failures.

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
pub use network::{LocalNetworkConfig, Network, NetworkConfig, SubmittedTransaction};
pub use rng::AgentRng;
pub use scenario::{AgentOutcome, Scenario, ScenarioBuilder, ScenarioReport};
pub use thruput_node_supervisor::{
    BlockDriverConfig, BlockDriverMode, BlockProductionError, LogLevel, ProducedBlock,
};
pub use thruput_signing::default_wallet::WalletInitCall;

// Re-exports so agent code can name RPC/XDR types and encode calldata without
// taking direct dependencies on these crates.
pub use thruput_json_rpc_types as rpc_types;
pub use thruput_xdr as xdr;
pub use xdrs;
