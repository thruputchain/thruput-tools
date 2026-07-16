//! `groundhog-testkit`: a harness for writing multi-agent integration tests as
//! ordinary application code.
//!
//! A test builds a [`Scenario`] against an explicit [`TargetConfig`], hands it
//! a set of [`Agent`]s, and asserts on what they observe. Scenarios are plain
//! `#[tokio::test]` functions; the fixture is an explicit value the test holds
//! and tears down.
//!
//! Each agent is assigned a funded dev account and can [`deploy`] contracts and
//! [`call`] them through its dev wallet, await receipts, and read chain state,
//! in addition to coordinating with other agents off-chain and using the
//! virtual clock and deterministic randomness.
//!
//! [`deploy`]: AgentContext::deploy
//! [`call`]: AgentContext::call
//!
//! Targets: [`TargetConfig::existing`] attaches to a running node,
//! [`TargetConfig::local`] launches and owns an ephemeral `hog-devnode`, and
//! [`TargetConfig::offline`] runs agents with locally-derived accounts and no
//! chain (for coordination/timing/messaging tests). See `docs/testkit/DESIGN.md`.

mod account;
pub mod agent;
mod artifact;
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
pub mod target;

pub use account::Deployed;
pub use agent::{Agent, AgentContext, AgentId};
pub use artifact::ArtifactKind;
pub use clock::Clock;
pub use contract::{Contract, ContractEntrypointID, entrypoint_id};
pub use error::{Result, TestkitError};
pub use groundhog_node_supervisor::LogLevel;
pub use logging::init_tracing;
pub use mailbox::{Envelope, Mailbox, Message};
pub use network::Network;
pub use rng::AgentRng;
pub use scenario::{AgentOutcome, Scenario, ScenarioBuilder, ScenarioReport};
pub use target::{LocalTargetConfig, TargetConfig};

// Re-exports so agent code can name RPC/XDR types and encode calldata without
// taking direct dependencies on these crates.
pub use groundhog_json_rpc_types as rpc_types;
pub use groundhog_xdr as xdr;
pub use xdrs;
