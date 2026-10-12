//! Agents and the context the harness hands them.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use thruput_json_rpc_types::transaction::TransactionStatus;
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::types::{ContractAddress, SHA256Hash};

use crate::account::{
    Account, DeployOptions, Deployed, PreparedDeploy, SubmittedDeploy, verify_deployment_receipt,
};
use crate::artifact::{
    ArtifactEntry, ArtifactKind, PendingRegistration, RegistrationOwner, Registry,
};
use xdrs::{ReadXDR, WriteXDR};

use crate::caller::Caller;
use crate::clock::Clock;
use crate::error::{Result, TestkitError};
use crate::mailbox::{Envelope, Mailbox, Message};
use crate::network::{Network, SubmittedTransaction};
use crate::rng::AgentRng;

enum RegistrationResolution {
    NeedsRegistration,
    Registered,
    Recovered(Box<SubmittedDeploy>),
}

/// Stable identity of an agent within a scenario run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId {
    index: usize,
    name: Arc<str>,
}

impl AgentId {
    pub(crate) fn new(index: usize, name: Arc<str>) -> Self {
        Self { index, name }
    }

    /// Position of the agent in the scenario's agent list.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Agent name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

type AgentFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
type AgentBody = Box<dyn FnOnce(AgentContext) -> AgentFuture + Send>;

/// An agent: a name plus the async body the scheduler runs.
pub struct Agent {
    name: Arc<str>,
    body: AgentBody,
}

impl Agent {
    /// Create an agent from a `name` and an async `body`.
    ///
    /// The body receives an [`AgentContext`] and returns `Ok(())` on success;
    /// a returned error or a panic fails the scenario.
    pub fn new<F, Fut>(name: impl Into<String>, body: F) -> Self
    where
        F: FnOnce(AgentContext) -> Fut + Send + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let name: Arc<str> = Arc::from(name.into());
        Self {
            name,
            body: Box::new(move |ctx| Box::pin(body(ctx))),
        }
    }

    /// Agent name.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn name_arc(&self) -> Arc<str> {
        Arc::clone(&self.name)
    }

    pub(crate) fn into_body(self) -> AgentBody {
        self.body
    }
}

impl fmt::Debug for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agent")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Everything an agent needs to interact with the world.
///
/// The context is the only channel through which an agent touches the network,
/// the clock, randomness, and other agents. Agents must not reach around it to
/// raw I/O or `std::time`.
///
/// Chain operations take `&self`: they go through the agent's [`Caller`],
/// which can also be cloned out to hold independently of the context. Only
/// receiving messages and drawing randomness take `&mut self`, since those
/// consume per-agent streams in order, along with artifact deployment and
/// registration. Those treat an in-flight registration from the same agent as
/// a retry of the same request, so overlapping requests from one agent must
/// not be possible.
#[derive(Debug)]
pub struct AgentContext {
    id: AgentId,
    caller: Caller,
    clock: Clock,
    rng: AgentRng,
    mailbox: Mailbox,
    artifacts: Arc<Registry>,
}

impl AgentContext {
    pub(crate) fn new(
        id: AgentId,
        network: Network,
        clock: Clock,
        rng: AgentRng,
        mailbox: Mailbox,
        account: Account,
        artifacts: Arc<Registry>,
    ) -> Self {
        Self {
            id,
            caller: Caller::new(account, network),
            clock,
            rng,
            mailbox,
            artifacts,
        }
    }

    /// The on-chain wallet address of this agent's assigned account.
    pub fn address(&self) -> ContractAddress {
        self.caller.account.address()
    }

    /// Index of this agent's assigned account.
    pub fn account_index(&self) -> u32 {
        self.caller.account.index()
    }

    /// Deploy an instance of a registered artifact, registering its binary once
    /// (on the first deploy of that artifact, in the same transaction) and
    /// deploying by hash thereafter. Awaits the receipt; fails if execution is
    /// not successful.
    ///
    /// Requests from one agent cannot overlap, since a second request would
    /// be taken as a retry of the first:
    ///
    /// ```compile_fail,E0499
    /// # async fn overlap(ctx: &mut thruput_testkit::AgentContext) {
    /// let options = thruput_testkit::DeployOptions::funded(0);
    /// let first = ctx.deploy_artifact("counter", options);
    /// let second = ctx.deploy_artifact("counter", options);
    /// let _ = (first.await, second.await);
    /// # }
    /// ```
    pub async fn deploy_artifact(
        &mut self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<Deployed> {
        loop {
            match self.submit_deploy_artifact(name, options).await {
                Ok(submitted) => return submitted.await_receipt().await,
                Err(TestkitError::ArtifactRegistrationPending { .. }) => {
                    let entry = self.artifacts.get(name)?;
                    if let Some(deployed) = await_pending_registration(
                        &self.caller.network,
                        &entry,
                        self.caller.account.registration_owner(),
                    )
                    .await?
                    {
                        return Ok(deployed);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Submit an artifact deployment without waiting for its receipt.
    ///
    /// Returns [`TestkitError::ArtifactRegistrationPending`] when another agent
    /// is registering the same artifact. The error carries that transaction's
    /// hash so a manual scenario can produce its block and await its receipt
    /// before retrying this deployment.
    /// A scheduler `Retry` replays the retained registration's signed bytes;
    /// a `Dropped` outcome clears that attempt and returns its failure.
    pub async fn submit_deploy_artifact(
        &mut self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        let mut pending = entry.registration_guard().await;
        match reconcile_pending_registration(
            &self.caller.network,
            name,
            &entry,
            &mut pending,
            self.caller.account.registration_owner(),
            options,
        )
        .await?
        {
            RegistrationResolution::Registered => {
                drop(pending);
                return self
                    .caller
                    .account
                    .submit_deploy_entry(&self.caller.network, &entry, false, options)
                    .await;
            }
            RegistrationResolution::Recovered(submitted) => return Ok(*submitted),
            RegistrationResolution::NeedsRegistration => {}
        }
        let prepared = self.caller.account.prepare_deploy_entry(
            &self.caller.network,
            &entry,
            true,
            options,
        )?;
        *pending = Some(prepared.pending_registration());
        drop(pending);
        let submitted = prepared.submit().await?;
        Ok(submitted.with_registration(entry))
    }

    /// Explicitly register an artifact's binary, deploying its first instance in
    /// the same transaction. Prefer [`AgentContext::deploy_artifact`] unless a
    /// test needs to control when registration happens.
    pub async fn register_artifact(
        &mut self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<Deployed> {
        loop {
            match self.submit_register_artifact(name, options).await {
                Ok(submitted) => return submitted.await_receipt().await,
                Err(TestkitError::ArtifactRegistrationPending { .. }) => {
                    let entry = self.artifacts.get(name)?;
                    if let Some(deployed) = await_pending_registration(
                        &self.caller.network,
                        &entry,
                        self.caller.account.registration_owner(),
                    )
                    .await?
                    {
                        return Ok(deployed);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Submit an explicit artifact registration without waiting for its receipt.
    ///
    /// Returns [`TestkitError::ArtifactRegistrationPending`] when a registration
    /// transaction is already in flight.
    pub async fn submit_register_artifact(
        &mut self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        let mut pending = entry.registration_guard().await;
        match reconcile_pending_registration(
            &self.caller.network,
            name,
            &entry,
            &mut pending,
            self.caller.account.registration_owner(),
            options,
        )
        .await?
        {
            RegistrationResolution::Registered => {
                drop(pending);
                return self
                    .caller
                    .account
                    .submit_deploy_entry(&self.caller.network, &entry, true, options)
                    .await;
            }
            RegistrationResolution::Recovered(submitted) => return Ok(*submitted),
            RegistrationResolution::NeedsRegistration => {}
        }
        let prepared = self.caller.account.prepare_deploy_entry(
            &self.caller.network,
            &entry,
            true,
            options,
        )?;
        *pending = Some(prepared.pending_registration());
        drop(pending);
        let submitted = prepared.submit().await?;
        Ok(submitted.with_registration(entry))
    }

    /// Deploy an instance by hash only, without carrying the binary. The
    /// artifact must already be registered and committed, or the transaction
    /// fails. Prefer [`AgentContext::deploy_artifact`] unless a test is
    /// asserting on the hash-only path.
    pub async fn deploy_registered(
        &self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<Deployed> {
        self.submit_deploy_registered(name, options)
            .await?
            .await_receipt()
            .await
    }

    /// Submit a hash-only artifact deployment without waiting for its receipt.
    pub async fn submit_deploy_registered(
        &self,
        name: &str,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        self.caller
            .account
            .submit_deploy_entry(&self.caller.network, &entry, false, options)
            .await
    }

    /// The network identity (contract hash) of a registered artifact.
    pub fn contract_hash(&self, name: &str) -> Result<SHA256Hash> {
        Ok(self.artifacts.get(name)?.hash())
    }

    /// Whether the harness has observed this artifact registered this run.
    pub fn is_registered(&self, name: &str) -> Result<bool> {
        Ok(self.artifacts.get(name)?.registered())
    }

    /// Deploy raw contract bytes not held in the registry, awaiting the receipt.
    /// The binary is carried in every such transaction. Fails if execution is
    /// not successful.
    pub async fn deploy(
        &self,
        artifact: &[u8],
        kind: ArtifactKind,
        options: DeployOptions<'_>,
    ) -> Result<Deployed> {
        self.submit_deploy(artifact, kind, options)
            .await?
            .await_receipt()
            .await
    }

    /// Submit a raw artifact deployment without waiting for its receipt.
    pub async fn submit_deploy(
        &self,
        artifact: &[u8],
        _kind: ArtifactKind,
        options: DeployOptions<'_>,
    ) -> Result<SubmittedDeploy> {
        self.caller
            .account
            .submit_deploy(&self.caller.network, artifact, options)
            .await
    }

    /// Submit an exact raw HOG transfer from the runtime's bootstrap faucet.
    /// Existing-network fixtures start empty; local fixtures receive explicit genesis allocations.
    /// Retry uncertain local funding with the same amount on this scenario/network handle.
    pub async fn submit_fund(&self, amount: u128) -> Result<SubmittedTransaction> {
        self.caller
            .network
            .submit_fund(self.address(), amount)
            .await
    }

    /// Fund this account explicitly and require a successful matching receipt.
    /// In manual mode, use `submit_fund`, produce a block, then await the receipt.
    pub async fn fund(&self, amount: u128) -> Result<TransactionReceipt> {
        let receipt = self.submit_fund(amount).await?.await_receipt().await?;
        if !matches!(receipt.status, ExecStatus::Ok) {
            return Err(TestkitError::Reverted {
                method: "fund".to_owned(),
                status: format!("{:?}", receipt.status),
            });
        }
        Ok(receipt)
    }

    /// Call a deployed contract method from this agent's account, awaiting the
    /// receipt. The receipt is returned even when execution reverted.
    pub async fn call(
        &self,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<TransactionReceipt> {
        self.caller.call(target, entrypoint, calldata).await
    }

    /// Submit a contract call without waiting for its receipt.
    pub async fn submit_call(
        &self,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<SubmittedTransaction> {
        self.caller.submit_call(target, entrypoint, calldata).await
    }

    /// This agent's identity.
    pub fn id(&self) -> &AgentId {
        &self.id
    }

    /// This agent's name.
    pub fn name(&self) -> &str {
        self.id.name()
    }

    /// Handle to the network the scenario is running against.
    pub fn network(&self) -> &Network {
        &self.caller.network
    }

    /// This agent's account bound to the network, for submitting transactions
    /// independently of the context. Clone it to keep it past the borrow.
    pub fn caller(&self) -> &Caller {
        &self.caller
    }

    /// The scenario's virtual clock.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// This agent's deterministic random stream.
    pub fn rng(&mut self) -> &mut AgentRng {
        &mut self.rng
    }

    /// CXS-encode `msg` and send it to the agent named `to`.
    pub fn send<T>(&self, to: &str, msg: &T) -> Result<()>
    where
        T: WriteXDR + ?Sized,
    {
        let bytes = xdrs::cxs::encode(msg).map_err(TestkitError::encode)?;
        self.mailbox.send_to(to, bytes)
    }

    /// Wait for the next inbound message, decoding its CXS payload as `T`.
    ///
    /// Returns `None` once all peers finish; the inner `Result` is `Err` when
    /// the payload does not decode as `T`.
    pub async fn recv<T>(&mut self) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.recv().await.map(Envelope::decode)
    }

    /// Take a pending inbound message without waiting, decoding its payload
    /// as `T`.
    pub fn try_recv<T>(&mut self) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.try_recv().map(Envelope::decode)
    }

    /// Wait for the next message from the agent named `from`, decoding its CXS
    /// payload as `T`.
    ///
    /// Messages from other senders that arrive first are held for later
    /// [`recv`](Self::recv) / `recv_from` calls rather than dropped. Returns
    /// `None` once all peers finish without a matching message; the inner
    /// `Result` is `Err` when the payload does not decode as `T`.
    pub async fn recv_from<T>(&mut self, from: &str) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.recv_from(from).await.map(Envelope::decode)
    }

    /// Take a pending message from the agent named `from` without waiting,
    /// decoding its payload as `T`.
    pub fn try_recv_from<T>(&mut self, from: &str) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.try_recv_from(from).map(Envelope::decode)
    }

    /// Wait for the next message from any of the agents named in `from`,
    /// decoding its CXS payload as `T`. [`Message::from`] identifies which one.
    ///
    /// Like [`recv_from`](Self::recv_from), messages from senders outside the
    /// set are held for later calls rather than dropped.
    pub async fn recv_from_any<T>(&mut self, from: &[&str]) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.recv_from_any(from).await.map(Envelope::decode)
    }

    /// Take a pending message from any of the agents named in `from` without
    /// waiting, decoding its payload as `T`.
    pub fn try_recv_from_any<T>(&mut self, from: &[&str]) -> Option<Result<Message<T>>>
    where
        T: for<'r> ReadXDR<'r>,
    {
        self.mailbox.try_recv_from_any(from).map(Envelope::decode)
    }
}

impl AsRef<Caller> for AgentContext {
    fn as_ref(&self) -> &Caller {
        &self.caller
    }
}

async fn reconcile_pending_registration(
    network: &Network,
    name: &str,
    entry: &Arc<ArtifactEntry>,
    pending: &mut Option<PendingRegistration>,
    caller: RegistrationOwner,
    options: DeployOptions<'_>,
) -> Result<RegistrationResolution> {
    let Some(attempt) = pending.clone() else {
        return if entry.registered() {
            Ok(RegistrationResolution::Registered)
        } else {
            Ok(RegistrationResolution::NeedsRegistration)
        };
    };
    if attempt.owner == caller && attempt.effects != options.effects()? {
        return Err(TestkitError::DeploymentRequestMismatch {
            name: name.to_owned(),
        });
    }
    if entry.registered() && attempt.owner != caller {
        return Ok(RegistrationResolution::Registered);
    }
    let tx_hash = attempt.tx_hash;
    let Some(receipt) = network.receipt(tx_hash).await? else {
        return match network.transaction_status(tx_hash).await? {
            TransactionStatus::Retry { .. } => {
                let submitted = PreparedDeploy::from_registration(network.clone(), &attempt)
                    .submit()
                    .await?
                    .with_registration(Arc::clone(entry));
                if attempt.owner == caller {
                    Ok(RegistrationResolution::Recovered(Box::new(submitted)))
                } else {
                    Err(TestkitError::ArtifactRegistrationPending {
                        name: name.to_owned(),
                        tx_hash,
                    })
                }
            }
            TransactionStatus::Dropped { error } => {
                *pending = None;
                Err(TestkitError::Deploy {
                    detail: format!(
                        "retained registration dropped: {:?} code {}",
                        error.source, error.code
                    ),
                })
            }
            TransactionStatus::Invalid { reason } => {
                *pending = None;
                Err(TestkitError::Deploy {
                    detail: format!("retained registration rejected: {reason}"),
                })
            }
            TransactionStatus::Pending
            | TransactionStatus::Unknown
            | TransactionStatus::RecentFailure { .. } => {
                Err(TestkitError::ArtifactRegistrationPending {
                    name: name.to_owned(),
                    tx_hash,
                })
            }
            _ => Err(TestkitError::ArtifactRegistrationPending {
                name: name.to_owned(),
                tx_hash,
            }),
        };
    };
    verify_deployment_receipt(tx_hash, attempt.address, &attempt.tx_hex, &receipt)?;
    if matches!(receipt.status, ExecStatus::Ok) {
        entry.mark_registered();
        if attempt.owner == caller {
            *pending = None;
            Ok(RegistrationResolution::Recovered(Box::new(
                SubmittedDeploy::recovered(network.clone(), &attempt, receipt),
            )))
        } else {
            Ok(RegistrationResolution::Registered)
        }
    } else {
        *pending = None;
        Err(TestkitError::Deploy {
            detail: format!("{:?}", receipt.status),
        })
    }
}

async fn await_pending_registration(
    network: &Network,
    entry: &ArtifactEntry,
    caller: RegistrationOwner,
) -> Result<Option<Deployed>> {
    let attempt = {
        let pending = entry.registration_guard().await;
        pending.clone()
    };
    let Some(attempt) = attempt else {
        return Ok(None);
    };
    let tx_hash = attempt.tx_hash;
    let receipt = network.await_receipt(tx_hash).await?;
    finish_registration_receipt(entry, &attempt, receipt, caller).await
}

async fn finish_registration_receipt(
    entry: &ArtifactEntry,
    attempt: &PendingRegistration,
    receipt: TransactionReceipt,
    caller: RegistrationOwner,
) -> Result<Option<Deployed>> {
    verify_deployment_receipt(attempt.tx_hash, attempt.address, &attempt.tx_hex, &receipt)?;
    let succeeded = matches!(receipt.status, ExecStatus::Ok);
    if !succeeded || attempt.owner == caller {
        entry.finish_registration(attempt.tx_hash, succeeded).await;
    } else {
        entry.mark_registered();
    }
    if !succeeded {
        return Err(TestkitError::Deploy {
            detail: format!("{:?}", receipt.status),
        });
    }
    Ok((attempt.owner == caller).then_some(Deployed {
        address: attempt.address,
        tx_hash: attempt.tx_hash,
        receipt,
        registered: true,
    }))
}
