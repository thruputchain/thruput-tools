//! Agents and the context the harness hands them.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::transaction::TransactionStatus;
use groundhog_xdr::contract::ContractEntrypointID;
use groundhog_xdr::types::{ContractAddress, SHA256Hash};

use crate::account::{Account, Deployed, SubmittedDeploy};
use crate::artifact::{ArtifactEntry, ArtifactKind, PendingRegistration, Registry};
use xdrs::{ReadXDR, WriteXDR};

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
#[derive(Debug)]
pub struct AgentContext {
    id: AgentId,
    network: Network,
    clock: Clock,
    rng: AgentRng,
    mailbox: Mailbox,
    account: Account,
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
            network,
            clock,
            rng,
            mailbox,
            account,
            artifacts,
        }
    }

    /// The on-chain wallet address of this agent's assigned account.
    pub fn address(&self) -> ContractAddress {
        self.account.address()
    }

    /// Index of this agent's assigned bootstrap account.
    pub fn account_index(&self) -> u32 {
        self.account.index()
    }

    /// Deploy an instance of a registered artifact, registering its binary once
    /// (on the first deploy of that artifact, in the same transaction) and
    /// deploying by hash thereafter. Awaits the receipt; fails if execution is
    /// not successful.
    pub async fn deploy_artifact(&mut self, name: &str) -> Result<Deployed> {
        loop {
            match self.submit_deploy_artifact(name).await {
                Ok(submitted) => return submitted.await_receipt().await,
                Err(TestkitError::ArtifactRegistrationPending { .. }) => {
                    let entry = self.artifacts.get(name)?;
                    if let Some(deployed) =
                        await_pending_registration(&self.network, &entry, self.address()).await?
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
    pub async fn submit_deploy_artifact(&mut self, name: &str) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        let mut pending = entry.registration_guard().await;
        match reconcile_pending_registration(
            &self.network,
            name,
            &entry,
            &mut pending,
            self.address(),
        )
        .await?
        {
            RegistrationResolution::Registered => {
                drop(pending);
                return self
                    .account
                    .submit_deploy_entry(&self.network, &entry, false)
                    .await;
            }
            RegistrationResolution::Recovered(submitted) => return Ok(*submitted),
            RegistrationResolution::NeedsRegistration => {}
        }
        let prepared = self
            .account
            .prepare_deploy_entry(&self.network, &entry, true)?;
        *pending = Some(prepared.pending_registration());
        drop(pending);
        let submitted = prepared.submit().await?;
        Ok(submitted.with_registration(entry))
    }

    /// Explicitly register an artifact's binary, deploying its first instance in
    /// the same transaction. Prefer [`AgentContext::deploy_artifact`] unless a
    /// test needs to control when registration happens.
    pub async fn register_artifact(&mut self, name: &str) -> Result<Deployed> {
        loop {
            match self.submit_register_artifact(name).await {
                Ok(submitted) => return submitted.await_receipt().await,
                Err(TestkitError::ArtifactRegistrationPending { .. }) => {
                    let entry = self.artifacts.get(name)?;
                    if let Some(deployed) =
                        await_pending_registration(&self.network, &entry, self.address()).await?
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
    pub async fn submit_register_artifact(&mut self, name: &str) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        let mut pending = entry.registration_guard().await;
        match reconcile_pending_registration(
            &self.network,
            name,
            &entry,
            &mut pending,
            self.address(),
        )
        .await?
        {
            RegistrationResolution::Registered => {
                drop(pending);
                return self
                    .account
                    .submit_deploy_entry(&self.network, &entry, true)
                    .await;
            }
            RegistrationResolution::Recovered(submitted) => return Ok(*submitted),
            RegistrationResolution::NeedsRegistration => {}
        }
        let prepared = self
            .account
            .prepare_deploy_entry(&self.network, &entry, true)?;
        *pending = Some(prepared.pending_registration());
        drop(pending);
        let submitted = prepared.submit().await?;
        Ok(submitted.with_registration(entry))
    }

    /// Deploy an instance by hash only, without carrying the binary. The
    /// artifact must already be registered and committed, or the transaction
    /// fails. Prefer [`AgentContext::deploy_artifact`] unless a test is
    /// asserting on the hash-only path.
    pub async fn deploy_registered(&mut self, name: &str) -> Result<Deployed> {
        self.submit_deploy_registered(name)
            .await?
            .await_receipt()
            .await
    }

    /// Submit a hash-only artifact deployment without waiting for its receipt.
    pub async fn submit_deploy_registered(&mut self, name: &str) -> Result<SubmittedDeploy> {
        let entry = self.artifacts.get(name)?;
        self.account
            .submit_deploy_entry(&self.network, &entry, false)
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
    pub async fn deploy(&mut self, artifact: &[u8], kind: ArtifactKind) -> Result<Deployed> {
        self.submit_deploy(artifact, kind)
            .await?
            .await_receipt()
            .await
    }

    /// Submit a raw artifact deployment without waiting for its receipt.
    pub async fn submit_deploy(
        &mut self,
        artifact: &[u8],
        kind: ArtifactKind,
    ) -> Result<SubmittedDeploy> {
        self.account
            .submit_deploy(&self.network, artifact, kind)
            .await
    }

    /// Call a deployed contract method from this agent's account, awaiting the
    /// receipt. The receipt is returned even when execution reverted.
    pub async fn call(
        &mut self,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<TransactionReceipt> {
        self.submit_call(target, entrypoint, calldata)
            .await?
            .await_receipt()
            .await
    }

    /// Submit a contract call without waiting for its receipt.
    pub async fn submit_call(
        &mut self,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<SubmittedTransaction> {
        self.account
            .submit_call(&self.network, target, entrypoint, calldata)
            .await
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
        &self.network
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

async fn reconcile_pending_registration(
    network: &Network,
    name: &str,
    entry: &ArtifactEntry,
    pending: &mut Option<PendingRegistration>,
    caller: ContractAddress,
) -> Result<RegistrationResolution> {
    let Some(attempt) = pending.clone() else {
        return if entry.registered() {
            Ok(RegistrationResolution::Registered)
        } else {
            Ok(RegistrationResolution::NeedsRegistration)
        };
    };
    if entry.registered() && attempt.owner != caller {
        return Ok(RegistrationResolution::Registered);
    }
    let tx_hash = attempt.tx_hash;
    let Some(receipt) = network.receipt(tx_hash).await? else {
        return match network.transaction_status(tx_hash).await? {
            TransactionStatus::Invalid { .. } => {
                *pending = None;
                Ok(RegistrationResolution::NeedsRegistration)
            }
            TransactionStatus::Pending
            | TransactionStatus::Unknown
            | TransactionStatus::RecentFailure { .. } => {
                resubmit_registration(network, &attempt).await?;
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
        Ok(RegistrationResolution::NeedsRegistration)
    }
}

async fn await_pending_registration(
    network: &Network,
    entry: &ArtifactEntry,
    caller: ContractAddress,
) -> Result<Option<Deployed>> {
    let attempt = {
        let pending = entry.registration_guard().await;
        pending.clone()
    };
    let Some(attempt) = attempt else {
        return Ok(None);
    };
    let tx_hash = attempt.tx_hash;
    match network.await_receipt(tx_hash).await {
        Ok(receipt) => Ok(finish_registration_receipt(entry, &attempt, receipt, caller).await),
        Err(error) => match network.transaction_status(tx_hash).await {
            Ok(TransactionStatus::Invalid { .. }) => {
                entry.finish_registration(tx_hash, false).await;
                Ok(None)
            }
            Ok(
                TransactionStatus::Pending
                | TransactionStatus::Unknown
                | TransactionStatus::RecentFailure { .. },
            ) => {
                resubmit_registration(network, &attempt).await?;
                match network.await_receipt(tx_hash).await {
                    Ok(receipt) => {
                        Ok(finish_registration_receipt(entry, &attempt, receipt, caller).await)
                    }
                    Err(_) => Err(error),
                }
            }
            _ => Err(error),
        },
    }
}

async fn finish_registration_receipt(
    entry: &ArtifactEntry,
    attempt: &PendingRegistration,
    receipt: TransactionReceipt,
    caller: ContractAddress,
) -> Option<Deployed> {
    let succeeded = matches!(receipt.status, ExecStatus::Ok);
    if !succeeded || attempt.owner == caller {
        entry.finish_registration(attempt.tx_hash, succeeded).await;
    } else {
        entry.mark_registered();
    }
    (succeeded && attempt.owner == caller).then_some(Deployed {
        address: attempt.address,
        tx_hash: attempt.tx_hash,
        receipt,
        registered: true,
    })
}

async fn resubmit_registration(network: &Network, attempt: &PendingRegistration) -> Result<()> {
    let actual = network.submit_raw(attempt.tx_hex.clone()).await?;
    if actual != attempt.tx_hash {
        return Err(TestkitError::SubmissionHashMismatch {
            expected: format!("{:?}", attempt.tx_hash),
            actual: format!("{actual:?}"),
        });
    }
    Ok(())
}
