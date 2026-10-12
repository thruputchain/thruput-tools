//! Contract method helpers.
//!
//! [`entrypoint_id`] computes a method selector from an interface identity.
//! [`Contract`] binds an address plus that identity and offers typed
//! `call`/`view`, encoding arguments and decoding returns with `xdrs` so tests
//! name methods and pass Rust values instead of hand-building calldata.
//! [`ContractCall`] is a prepared, typed call: the building block for typed
//! clients whose methods mirror a contract interface.

pub use thruput_xdr::contract::ContractEntrypointID;

use std::fmt;
use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::Pin;

use thruput_contract_util::semver::try_major_version_from_string;
use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use thruput_json_rpc_types::transaction::TxHash;
use thruput_json_rpc_types::view::RootInvocationOutput;
use thruput_signing::default_wallet::decode_default_wallet_call_return;
use thruput_xdr::types::ContractAddress;

use xdrs::{ReadXDR, WriteXDR};

use crate::caller::Caller;
use crate::error::{Result, TestkitError};
use crate::network::SubmittedTransaction;

/// A typed contract call submitted without waiting for block production.
#[derive(Debug, Clone)]
#[must_use = "submitted contract calls should be awaited or intentionally retained"]
pub struct SubmittedContractCall {
    transaction: SubmittedTransaction,
    method: String,
}

impl SubmittedContractCall {
    /// Canonical transaction hash returned by submission.
    pub fn tx_hash(&self) -> TxHash {
        self.transaction.tx_hash()
    }

    /// Wait until the scheduler reports this call as pending.
    pub async fn await_pending(&self) -> Result<()> {
        self.transaction.await_pending().await
    }

    /// Wait for the receipt and require successful execution.
    pub async fn await_receipt(&self) -> Result<TransactionReceipt> {
        let receipt = self.transaction.await_receipt().await?;
        match receipt.status {
            ExecStatus::Ok => Ok(receipt),
            ref status => Err(TestkitError::Reverted {
                method: self.method.clone(),
                status: format!("{status:?}"),
            }),
        }
    }
}

/// A typed state-changing contract call, prepared but not yet submitted.
///
/// Built by [`Contract::prepare_call`], and the natural return type of a typed
/// client's call methods. Awaiting it submits the call, requires successful
/// execution, and decodes the return as `R`. To see the receipt as well, use
/// [`with_receipt`](Self::with_receipt); to submit without waiting, use
/// [`submit`](Self::submit).
#[must_use = "a contract call does nothing until it is awaited or submitted"]
pub struct ContractCall<R> {
    caller: Caller,
    target: ContractAddress,
    method: String,
    prepared: Result<(ContractEntrypointID, Vec<u8>)>,
    returns: PhantomData<fn() -> R>,
}

impl<R> fmt::Debug for ContractCall<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContractCall")
            .field("caller", &self.caller.address())
            .field("target", &self.target)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl<R> ContractCall<R> {
    /// Submit the call without waiting for its receipt.
    pub async fn submit(self) -> Result<SubmittedContractCall> {
        let (selector, calldata) = self.prepared?;
        let transaction = self
            .caller
            .submit_call(self.target, selector, &calldata)
            .await?;
        Ok(SubmittedContractCall {
            transaction,
            method: self.method,
        })
    }

    /// Submit the call and await its receipt, requiring successful execution
    /// but ignoring the return value.
    pub async fn receipt(self) -> Result<TransactionReceipt> {
        self.submit().await?.await_receipt().await
    }
}

impl<R> ContractCall<R>
where
    R: for<'r> ReadXDR<'r>,
{
    /// Submit the call, require successful execution, and return the decoded
    /// return value together with the receipt (for its events or gas).
    pub async fn with_receipt(self) -> Result<(R, TransactionReceipt)> {
        let receipt = self.receipt().await?;
        let returned = decode_call_return(&receipt)?;
        Ok((returned, receipt))
    }
}

impl<R> IntoFuture for ContractCall<R>
where
    R: for<'r> ReadXDR<'r> + Send + 'static,
{
    type Output = Result<R>;
    type IntoFuture = Pin<Box<dyn Future<Output = Result<R>> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { Ok(self.with_receipt().await?.0) })
    }
}

/// Compute the entrypoint selector for a contract method.
///
/// It is derived from the contract's interface `name`, `version`, and `method`
/// name exactly as the `thruput` CLI derives selectors, so tests can name
/// methods instead of pasting raw hex (which drifts whenever a fixture is
/// rebuilt).
///
/// ```
/// let bump = thruput_testkit::entrypoint_id("CallerCounter", "0.1.0", "bump");
/// ```
#[must_use]
pub fn entrypoint_id(interface: &str, version: &str, method: &str) -> ContractEntrypointID {
    thruput_client::codec::method_entrypoint_id(interface, version, method)
}

/// Fallible form of [`entrypoint_id`] for interface identities from untrusted input.
pub fn try_entrypoint_id(
    interface: &str,
    version: &str,
    method: &str,
) -> Result<ContractEntrypointID> {
    if try_major_version_from_string(version).is_none() {
        return Err(TestkitError::InvalidContractVersion {
            version: version.to_owned(),
        });
    }
    Ok(entrypoint_id(interface, version, method))
}

/// A deployed contract instance bound to its interface identity.
///
/// `call` and `view` encode the argument value (typically a tuple of the
/// method's arguments) with the contract (CXS) codec and decode the return the
/// same way, so a test works in Rust values rather than raw calldata. The
/// argument and return types must implement `xdrs` `WriteXDR` / `ReadXDR` (the
/// method's own argument/return types from a contract's interface crate do).
///
/// Each method takes anything that can reach a [`Caller`]: an agent's `&ctx`,
/// or a `Caller` cloned out of it with
/// [`AgentContext::caller`](crate::AgentContext::caller).
#[derive(Debug, Clone)]
pub struct Contract {
    address: ContractAddress,
    interface: String,
    version: String,
}

impl Contract {
    /// Bind a handle to a deployed `address` and its interface identity.
    pub fn new(
        address: ContractAddress,
        interface: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            address,
            interface: interface.into(),
            version: version.into(),
        }
    }

    /// The deployed contract address.
    #[must_use]
    pub fn address(&self) -> ContractAddress {
        self.address
    }

    fn selector(&self, method: &str) -> Result<ContractEntrypointID> {
        try_entrypoint_id(&self.interface, &self.version, method)
    }

    /// Prepare a state-changing call to `method` from `ctx`'s account, whose
    /// return decodes as `R`. Nothing is submitted until the [`ContractCall`]
    /// is awaited or submitted.
    ///
    /// The arguments are encoded now, so the call borrows nothing: it can be
    /// held, moved into another task, or returned from a typed client method.
    pub fn prepare_call<R, A>(
        &self,
        ctx: &(impl AsRef<Caller> + ?Sized),
        method: &str,
        args: &A,
    ) -> ContractCall<R>
    where
        A: WriteXDR + ?Sized,
    {
        let prepared = self.selector(method).and_then(|selector| {
            let calldata = xdrs::cxs::encode(args).map_err(TestkitError::encode)?;
            Ok((selector, calldata))
        });
        ContractCall {
            caller: ctx.as_ref().clone(),
            target: self.address,
            method: method.to_owned(),
            prepared,
            returns: PhantomData,
        }
    }

    /// Submit a state-changing call to `method` through the agent's wallet,
    /// awaiting the receipt and requiring successful execution.
    ///
    /// Returns [`TestkitError::Reverted`] if the transaction executed but its
    /// status was not `Ok`, so callers can `?` a call they expect to succeed
    /// instead of asserting on the receipt. Add call-site context with
    /// `map_err` where the method name alone does not identify the call.
    pub async fn call<A>(
        &self,
        ctx: &(impl AsRef<Caller> + ?Sized),
        method: &str,
        args: &A,
    ) -> Result<TransactionReceipt>
    where
        A: WriteXDR + ?Sized,
    {
        self.prepare_call::<(), _>(ctx, method, args)
            .receipt()
            .await
    }

    /// Submit a state-changing call without waiting for its receipt.
    pub async fn submit_call<A>(
        &self,
        ctx: &(impl AsRef<Caller> + ?Sized),
        method: &str,
        args: &A,
    ) -> Result<SubmittedContractCall>
    where
        A: WriteXDR + ?Sized,
    {
        self.prepare_call::<(), _>(ctx, method, args).submit().await
    }

    /// Call `method`, requiring success, and decode the return value to `R`.
    pub async fn call_returning<A, R>(
        &self,
        ctx: &(impl AsRef<Caller> + ?Sized),
        method: &str,
        args: &A,
    ) -> Result<R>
    where
        A: WriteXDR + ?Sized,
        R: for<'r> ReadXDR<'r>,
    {
        let (returned, _) = self
            .prepare_call::<R, _>(ctx, method, args)
            .with_receipt()
            .await?;
        Ok(returned)
    }

    /// Simulate a read-only `view` of `method`, decoding the return to `R`.
    ///
    /// `caller` sets the simulated `msg.sender`; pass an agent's own address for
    /// state keyed by the caller.
    pub async fn view<A, R>(
        &self,
        ctx: &(impl AsRef<Caller> + ?Sized),
        method: &str,
        args: &A,
        caller: Option<ContractAddress>,
    ) -> Result<R>
    where
        A: WriteXDR + ?Sized,
        R: for<'r> ReadXDR<'r>,
    {
        let calldata = xdrs::cxs::encode(args).map_err(TestkitError::encode)?;
        let response = ctx
            .as_ref()
            .network()
            .view_call(self.address, self.selector(method)?, &calldata, caller)
            .await?;
        match response.output {
            RootInvocationOutput::Return { return_data } => decode_return(&return_data.0),
            other => Err(TestkitError::NoReturn {
                kind: "view",
                detail: format!("{other:?}"),
            }),
        }
    }
}

/// Decode a successful call's return from its receipt. The account's wallet
/// wraps the method's return, one buffer per call it made; absent return data
/// is rejected rather than read as an empty return.
fn decode_call_return<R>(receipt: &TransactionReceipt) -> Result<R>
where
    R: for<'r> ReadXDR<'r>,
{
    let data = receipt
        .return_data
        .as_ref()
        .map_or(&[][..], |data| data.as_ref());
    let bytes = decode_default_wallet_call_return(data).map_err(TestkitError::decode)?;
    decode_return(&bytes)
}

fn decode_return<R>(bytes: &[u8]) -> Result<R>
where
    R: for<'r> ReadXDR<'r>,
{
    xdrs::cxs::decode::<R>(bytes).map_err(TestkitError::decode)
}

#[cfg(test)]
mod tests {
    use thruput_xdr::types::ContractAddress;

    use crate::account::Account;
    use crate::caller::Caller;
    use crate::error::TestkitError;
    use crate::manifest::derive_native_accounts;
    use crate::network::Network;
    use crate::nonce::NonceNamespace;

    use thruput_json_rpc_types::BlockHeight;
    use thruput_json_rpc_types::hex::{Hex0x, HexBytes};
    use thruput_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
    use thruput_json_rpc_types::transaction::TxHash;
    use thruput_xdr::types::SHA256Hash;

    use super::{
        Contract, ContractEntrypointID, decode_call_return, decode_return, entrypoint_id,
        try_entrypoint_id,
    };

    fn receipt(return_data: Option<Vec<u8>>) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash: TxHash(Hex0x(SHA256Hash::from([1; 32]))),
            height: BlockHeight::from(1),
            status: ExecStatus::Ok,
            events: Vec::new(),
            return_data: return_data.map(HexBytes),
            abort_data: None,
            gas_used: 1,
        }
    }

    #[test]
    fn call_return_requires_exactly_one_wallet_result() {
        let wrap = |results: Vec<Vec<u8>>| Some(xdrs::cxs::encode(&results).unwrap());

        // One empty child result is a valid `()` return.
        decode_call_return::<()>(&receipt(wrap(vec![Vec::new()]))).unwrap();
        let value = xdrs::cxs::encode(&7u64).unwrap();
        assert_eq!(
            decode_call_return::<u64>(&receipt(wrap(vec![value.clone()]))).unwrap(),
            7
        );

        // Missing data, or other than one child result, is not.
        for return_data in [None, wrap(Vec::new()), wrap(vec![value.clone(), value])] {
            assert!(matches!(
                decode_call_return::<()>(&receipt(return_data)),
                Err(TestkitError::Decode { .. })
            ));
        }
    }

    #[test]
    fn matches_known_erc20_selector() {
        // Same known-answer vector the CLI pins for `method_entrypoint_id`.
        assert_eq!(
            entrypoint_id("ERC20", "0.1.0", "transfer"),
            ContractEntrypointID::from([0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f])
        );
    }

    #[test]
    fn invalid_version_returns_an_error_instead_of_panicking() {
        assert!(matches!(
            try_entrypoint_id("ERC20", "latest", "transfer"),
            Err(TestkitError::InvalidContractVersion { version }) if version == "latest"
        ));
    }

    #[test]
    fn cxs_round_trip_matches_helper_bounds() {
        // Confirms the encode/decode plumbing and trait bounds the `Contract`
        // helper relies on (a tuple of method arguments).
        let encoded = xdrs::cxs::encode(&(7u64, 9u32)).expect("encode");
        let (a, b): (u64, u32) = decode_return(&encoded).expect("decode");
        assert_eq!((a, b), (7, 9));
    }

    #[tokio::test]
    async fn prepared_call_reports_a_bad_identity_when_awaited() {
        let dev = derive_native_accounts(1, "seed", None).remove(0);
        let caller = Caller::new(
            Account::new(dev, NonceNamespace::for_run("seed", 0)),
            Network::offline(),
        );
        let contract = Contract::new(ContractAddress::from([7; 32]), "ERC20", "latest");

        // Preparing never fails; the error surfaces once the call is used.
        let call = contract.prepare_call::<u64, _>(&caller, "get", &());
        assert!(matches!(
            call.await,
            Err(TestkitError::InvalidContractVersion { version }) if version == "latest"
        ));
    }
}
