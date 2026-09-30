//! Scenario builder, runner, and result types.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use groundhog_node_supervisor::{HogdDevConfig, NodeSupervisorConfig, NodeSupervisorHandle};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::account::Account;
use crate::agent::{Agent, AgentContext, AgentId};
use crate::artifact::{ArtifactKind, ArtifactSpec, Registry};
use crate::clock::Clock;
use crate::error::{AgentFailure, AgentFailures, Result, TestkitError};
use crate::mailbox::{Envelope, Mailbox};
use crate::manifest::{AccountSource, load_runtime_manifest};
use crate::network::{LocalNetworkConfig, Network, NetworkConfig};
use crate::nonce::NonceNamespace;
use crate::rng::AgentRng;

/// Builder for a [`Scenario`]. Every setting is explicit.
#[derive(Debug)]
pub struct ScenarioBuilder {
    seed: String,
    network: Option<NetworkConfig>,
    artifacts: Vec<ArtifactSpec>,
    legacy_accounts: bool,
}

impl ScenarioBuilder {
    /// Use the manifest's existing legacy dev-N wallet identities explicitly.
    /// These accounts support calls; deployment requires native accounts.
    #[must_use]
    pub fn legacy_bootstrap_accounts(mut self) -> Self {
        self.legacy_accounts = true;
        self
    }

    /// Set the network the scenario runs against. Required.
    #[must_use]
    pub fn network(mut self, network: NetworkConfig) -> Self {
        self.network = Some(network);
        self
    }

    /// Register a contract artifact loaded from a file, inferring its kind from
    /// the `.rv32a` extension. The file is read and validated at
    /// [`ScenarioBuilder::build`]. Agents deploy it by name.
    #[must_use]
    pub fn artifact(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.artifacts.push(ArtifactSpec::Path {
            name: name.into(),
            path: path.into(),
            kind: None,
        });
        self
    }

    /// Register a contract artifact from a file with an explicit kind.
    #[must_use]
    pub fn artifact_with_kind(
        mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        kind: ArtifactKind,
    ) -> Self {
        self.artifacts.push(ArtifactSpec::Path {
            name: name.into(),
            path: path.into(),
            kind: Some(kind),
        });
        self
    }

    /// Register a contract artifact from in-memory bytes (for example via
    /// `include_bytes!`) with an explicit kind.
    #[must_use]
    pub fn artifact_bytes(
        mut self,
        name: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        _kind: ArtifactKind,
    ) -> Self {
        self.artifacts.push(ArtifactSpec::Bytes {
            name: name.into(),
            bytes: bytes.into(),
        });
        self
    }

    /// Acquire the network, load artifacts, and produce a ready [`Scenario`].
    pub async fn build(self) -> Result<Scenario> {
        let config = self.network.ok_or(TestkitError::NoNetwork)?;
        let artifacts = Arc::new(Registry::from_specs(self.artifacts)?);
        let (network, accounts, guard) =
            acquire_network(config, &self.seed, self.legacy_accounts).await?;
        Ok(Scenario {
            seed: self.seed,
            network,
            clock: Clock::new(),
            accounts,
            artifacts,
            runs: AtomicU32::new(0),
            guard,
        })
    }
}

/// A ready scenario: an explicit fixture the test holds.
///
/// Dropping the scenario tears down anything the harness created for the
/// network (a managed local node is force-killed (`SIGKILL`) as a backstop);
/// [`Scenario::stop`] does the same gracefully and reports teardown errors.
#[derive(Debug)]
pub struct Scenario {
    seed: String,
    network: Network,
    clock: Clock,
    accounts: AccountSource,
    artifacts: Arc<Registry>,
    /// Number of [`run_agents`](Scenario::run_agents) calls so far; each run
    /// takes the next index for a fresh nonce namespace.
    runs: AtomicU32,
    guard: NetworkGuard,
}

impl Scenario {
    /// Start building a scenario with the given deterministic `seed`.
    #[must_use]
    pub fn builder(seed: impl Into<String>) -> ScenarioBuilder {
        ScenarioBuilder {
            seed: seed.into(),
            network: None,
            artifacts: Vec::new(),
            legacy_accounts: false,
        }
    }

    /// Handle onto the network the scenario runs against.
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// The scenario's virtual clock.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// Run a set of agents concurrently, returning once all have finished.
    ///
    /// Each agent is assigned a distinct account (by position), its own
    /// deterministic random stream, and a mailbox. After every non-panicking
    /// agent finishes, returned errors are aggregated in
    /// [`TestkitError::AgentsFailed`]. An agent panic (for example a failed
    /// assertion) propagates immediately.
    ///
    /// May be called more than once on the same scenario: each call runs in a
    /// fresh nonce namespace, so a later run against the same accounts does
    /// not collide with an earlier run's transactions.
    pub async fn run_agents<I>(&self, agents: I) -> Result<ScenarioReport>
    where
        I: IntoIterator<Item = Agent>,
    {
        let agents: Vec<Agent> = agents.into_iter().collect();
        let count = agents.len();

        let names: Vec<Arc<str>> = agents.iter().map(Agent::name_arc).collect();
        validate_unique_agent_names(&names)?;

        let mut test_accounts = self.accounts.accounts()?;
        if count > test_accounts.len() {
            return Err(TestkitError::NotEnoughAccounts {
                needed: count,
                available: test_accounts.len(),
            });
        }
        test_accounts.truncate(count);

        // Each run writes in its own nonce namespace so a rerun against the
        // same accounts does not replay an earlier run's nonces.
        let namespace =
            NonceNamespace::for_run(&self.seed, self.runs.fetch_add(1, Ordering::Relaxed));

        let mut senders = Vec::with_capacity(count);
        let mut receivers = Vec::with_capacity(count);
        for _ in 0..count {
            let (tx, rx) = mpsc::unbounded_channel::<Envelope>();
            senders.push(tx);
            receivers.push(rx);
        }

        let mut set: JoinSet<AgentOutcome> = JoinSet::new();
        for (index, ((agent, inbox), dev)) in agents
            .into_iter()
            .zip(receivers)
            .zip(test_accounts)
            .enumerate()
        {
            let mut peers = HashMap::with_capacity(count.saturating_sub(1));
            for (peer_index, peer_name) in names.iter().enumerate() {
                if peer_index != index {
                    peers.insert(peer_name.to_string(), senders[peer_index].clone());
                }
            }
            let name = Arc::clone(&names[index]);
            let mailbox = Mailbox::new(Arc::clone(&name), inbox, peers);
            let ctx = AgentContext::new(
                AgentId::new(index, Arc::clone(&name)),
                self.network.clone(),
                self.clock.clone(),
                AgentRng::derive(&self.seed, index, &name),
                mailbox,
                Account::new(dev, namespace),
                Arc::clone(&self.artifacts),
            );
            let body = agent.into_body();
            set.spawn(async move {
                let result = body(ctx).await;
                AgentOutcome {
                    name,
                    index,
                    result,
                }
            });
        }
        // Drop the harness's own senders so an agent's inbox closes once every
        // peer has finished.
        drop(senders);

        let mut outcomes: Vec<Option<AgentOutcome>> = (0..count).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(outcome) => {
                    let index = outcome.index;
                    outcomes[index] = Some(outcome);
                }
                Err(join_error) if join_error.is_panic() => {
                    std::panic::resume_unwind(join_error.into_panic());
                }
                Err(join_error) => {
                    return Err(TestkitError::AgentJoin {
                        detail: join_error.to_string(),
                    });
                }
            }
        }

        let outcomes = outcomes
            .into_iter()
            .map(|outcome| outcome.expect("bug: every agent produced an outcome"))
            .collect();
        let report = ScenarioReport { outcomes };
        if report.all_ok() {
            return Ok(report);
        }

        let failures = report
            .outcomes
            .into_iter()
            .filter_map(|outcome| {
                outcome
                    .result
                    .err()
                    .map(|error| AgentFailure::new(outcome.name.to_string(), outcome.index, error))
            })
            .collect();
        Err(TestkitError::AgentsFailed {
            failures: AgentFailures::new(failures),
        })
    }

    /// Tear the network down gracefully, reporting any teardown error.
    pub async fn stop(self) -> Result<()> {
        let Scenario { guard, .. } = self;
        match guard {
            NetworkGuard::Local(handle) => {
                handle
                    .stop()
                    .await
                    .map_err(|e| TestkitError::NodeSupervisor {
                        detail: e.to_string(),
                    })?;
            }
            NetworkGuard::Existing => {}
        }
        Ok(())
    }
}

fn validate_unique_agent_names(names: &[Arc<str>]) -> Result<()> {
    let mut unique_names = HashSet::with_capacity(names.len());
    for name in names {
        if !unique_names.insert(name.as_ref()) {
            return Err(TestkitError::DuplicateAgent {
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

async fn acquire_network(
    config: NetworkConfig,
    seed: &str,
    legacy_accounts: bool,
) -> Result<(Network, AccountSource, NetworkGuard)> {
    match config {
        NetworkConfig::Existing {
            rpc_url,
            runtime_manifest,
        } => {
            let manifest = load_runtime_manifest(&runtime_manifest)?;
            let network = Network::connect(&rpc_url, &manifest, None, seed)?;
            Ok((
                network,
                account_source(manifest, seed, legacy_accounts),
                NetworkGuard::Existing,
            ))
        }
        NetworkConfig::Local(config) => {
            let handle = start_local(config).await?;
            let rpc_url = format!("http://{}", handle.rpc_addr());
            let manifest = handle
                .hogd_readiness()
                .runtime_manifest
                .clone()
                .ok_or(TestkitError::MissingRuntimeManifest)?;
            let manual_block_producer = handle.manual_block_producer();
            let network = Network::connect(&rpc_url, &manifest, manual_block_producer, seed)?;
            Ok((
                network,
                account_source(manifest, seed, legacy_accounts),
                NetworkGuard::Local(Box::new(handle)),
            ))
        }
        NetworkConfig::Offline { accounts } => Ok((
            Network::offline(),
            AccountSource::Native {
                count: accounts,
                seed: seed.to_owned(),
                network_hash: None,
            },
            NetworkGuard::Existing,
        )),
    }
}

fn account_source(
    manifest: groundhog_json_rpc_types::runtime_manifest::RuntimeManifest,
    seed: &str,
    legacy: bool,
) -> AccountSource {
    if legacy {
        AccountSource::Manifest(Box::new(manifest))
    } else {
        AccountSource::Native {
            count: manifest.bootstrap_accounts.iter().filter(|account| account.role == groundhog_json_rpc_types::runtime_manifest::BOOTSTRAP_ACCOUNT_ROLE_LOCAL_USER).count(),
            seed: seed.to_owned(), network_hash: Some(manifest.network_hash.to_network_hash()),
        }
    }
}

async fn start_local(config: LocalNetworkConfig) -> Result<NodeSupervisorHandle> {
    let block_driver = config.block_driver;
    let mut hogd = HogdDevConfig::default()
        .with_dev_account_count(config.dev_account_count)
        .with_dev_account_seed(config.dev_account_seed)
        .with_dev_account_tokens(config.dev_account_tokens)
        .with_startup_timeout(config.startup_timeout)
        .with_contract_artifact_directories(config.contract_artifact_directories);
    if let Some(binary) = config.hog_devnode_binary {
        hogd = hogd.with_binary_path(binary);
    }
    if let Some(log_level) = config.log_level {
        // Print forwarded devnode logs to stderr at the requested verbosity.
        crate::logging::install_for_level(log_level);
        hogd = hogd.with_log_level(log_level);
    }
    let mut supervisor = NodeSupervisorConfig::default()
        .with_rpc_listen_address(format!("127.0.0.1:{}", config.rpc_port))
        .with_hogd_dev(hogd)
        .with_block_driver(block_driver);
    if let Some(protocol) = config.hogd_protocol {
        supervisor = supervisor.with_hogd_protocol_override(protocol);
    }
    NodeSupervisorHandle::start(supervisor)
        .await
        .map_err(|e| TestkitError::NodeSupervisor {
            detail: e.to_string(),
        })
}

/// Owns resources the harness created for the network and releases them on
/// drop.
///
/// The `existing` and `offline` networks own nothing. The `local` network owns
/// the node handle; dropping it force-kills the child (`SIGKILL`) as a
/// backstop, while [`Scenario::stop`] shuts it down gracefully.
#[derive(Debug)]
enum NetworkGuard {
    Existing,
    Local(Box<NodeSupervisorHandle>),
}

/// Outcome of a single agent.
#[derive(Debug)]
pub struct AgentOutcome {
    name: Arc<str>,
    index: usize,
    result: Result<()>,
}

impl AgentOutcome {
    /// Agent name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Agent index within the run.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The agent's returned result.
    pub fn result(&self) -> &Result<()> {
        &self.result
    }
}

/// Aggregate result of a [`Scenario::run_agents`] call.
#[derive(Debug)]
pub struct ScenarioReport {
    outcomes: Vec<AgentOutcome>,
}

impl ScenarioReport {
    /// Per-agent outcomes, indexed by agent position.
    pub fn outcomes(&self) -> &[AgentOutcome] {
        &self.outcomes
    }

    /// Whether every agent returned `Ok`.
    ///
    /// Reports returned by [`Scenario::run_agents`] always satisfy this; the
    /// method remains useful for code that receives a report abstractly.
    pub fn all_ok(&self) -> bool {
        self.outcomes.iter().all(|outcome| outcome.result.is_ok())
    }
}
