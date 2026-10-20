//! A cloneable handle for submitting transactions from an agent's account.
//!
//! [`AgentContext`](crate::AgentContext) holds the parts of an agent that must
//! stay exclusive (its mailbox and random stream). [`Caller`] is the part that
//! need not: the agent's account and the network it submits to. Clone one out
//! with [`AgentContext::caller`](crate::AgentContext::caller) to hold it in a
//! typed contract client, store it in a struct, or move it into a spawned task.

use thruput_json_rpc_types::responses::TransactionReceipt;
use thruput_xdr::contract::ContractEntrypointID;
use thruput_xdr::types::ContractAddress;

use crate::account::Account;
use crate::error::Result;
use crate::network::{Network, SubmittedTransaction};

/// An agent's account bound to the network it submits to.
///
/// Clones share the account's nonce sequence, so transactions submitted
/// through any clone never collide. Nonces (and so the addresses of deployed
/// contracts) are assigned in submission order: submissions made one after
/// another are deterministic for a given seed, while submissions raced from
/// concurrent tasks are numbered in whatever order the tasks reach them.
#[derive(Debug, Clone)]
pub struct Caller {
    pub(crate) account: Account,
    pub(crate) network: Network,
}

impl Caller {
    pub(crate) fn new(account: Account, network: Network) -> Self {
        Self { account, network }
    }

    /// The on-chain wallet address transactions are submitted from.
    pub fn address(&self) -> ContractAddress {
        self.account.address()
    }

    /// Handle to the network transactions are submitted to.
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// Call a deployed contract method from this account, awaiting the
    /// receipt. The receipt is returned even when execution reverted.
    pub async fn call(
        &self,
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
        &self,
        target: ContractAddress,
        entrypoint: ContractEntrypointID,
        calldata: &[u8],
    ) -> Result<SubmittedTransaction> {
        self.account
            .submit_call(&self.network, target, entrypoint, calldata)
            .await
    }
}

impl AsRef<Caller> for Caller {
    fn as_ref(&self) -> &Caller {
        self
    }
}
