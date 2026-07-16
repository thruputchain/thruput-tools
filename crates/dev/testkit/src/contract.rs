//! Contract method helpers.
//!
//! [`entrypoint_id`] computes a method selector from an interface identity.
//! [`Contract`] binds an address plus that identity and offers typed
//! `call`/`view`, encoding arguments and decoding returns with `xdrs` so tests
//! name methods and pass Rust values instead of hand-building calldata.

pub use groundhog_xdr::contract::ContractEntrypointID;

use groundhog_contract_util::{EntrypointSignature, NormalizedMethodSignature};
use groundhog_json_rpc_types::receipt::{ExecStatus, TransactionReceipt};
use groundhog_json_rpc_types::view::RootInvocationOutput;
use groundhog_xdr::types::ContractAddress;
use xdrs::{ReadXDR, WriteXDR};

use crate::agent::AgentContext;
use crate::error::{Result, TestkitError};

/// Compute the entrypoint selector for a contract method.
///
/// It is derived from the contract's interface `name`, `version`, and `method`
/// name exactly as the `groundhog` CLI derives selectors, so tests can name
/// methods instead of pasting raw hex (which drifts whenever a fixture is
/// rebuilt).
///
/// ```
/// let bump = groundhog_testkit::entrypoint_id("CallerCounter", "0.1.0", "bump");
/// ```
#[must_use]
pub fn entrypoint_id(interface: &str, version: &str, method: &str) -> ContractEntrypointID {
    let sig = EntrypointSignature {
        interface_name: Some(interface.to_string()),
        version_str: version.to_string(),
        method_sig: NormalizedMethodSignature::new(method),
    };

    sig.id()
}

/// A deployed contract instance bound to its interface identity.
///
/// `call` and `view` encode the argument value (typically a tuple of the
/// method's arguments) with the contract (CXS) codec and decode the return the
/// same way, so a test works in Rust values rather than raw calldata. The
/// argument and return types must implement `xdrs` `WriteXDR` / `ReadXDR` (the
/// method's own argument/return types from a contract's interface crate do).
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

    fn selector(&self, method: &str) -> ContractEntrypointID {
        entrypoint_id(&self.interface, &self.version, method)
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
        ctx: &mut AgentContext,
        method: &str,
        args: &A,
    ) -> Result<TransactionReceipt>
    where
        A: WriteXDR + ?Sized,
    {
        let calldata = xdrs::cxs::encode(args).map_err(TestkitError::encode)?;
        let receipt = ctx
            .call(self.address, self.selector(method), &calldata)
            .await?;
        match receipt.status {
            ExecStatus::Ok => Ok(receipt),
            status => Err(TestkitError::Reverted {
                method: method.to_owned(),
                status: format!("{status:?}"),
            }),
        }
    }

    /// Call `method`, requiring success, and decode the return value to `R`.
    pub async fn call_returning<A, R>(
        &self,
        ctx: &mut AgentContext,
        method: &str,
        args: &A,
    ) -> Result<R>
    where
        A: WriteXDR + ?Sized,
        R: for<'r> ReadXDR<'r>,
    {
        let receipt = self.call(ctx, method, args).await?;
        let bytes = receipt
            .return_data
            .as_ref()
            .map_or(&[][..], |data| data.as_ref());
        decode_return(bytes)
    }

    /// Simulate a read-only `view` of `method`, decoding the return to `R`.
    ///
    /// `caller` sets the simulated `msg.sender`; pass an agent's own address for
    /// state keyed by the caller.
    pub async fn view<A, R>(
        &self,
        ctx: &AgentContext,
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
            .network()
            .view_call(self.address, self.selector(method), &calldata, caller)
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

fn decode_return<R>(bytes: &[u8]) -> Result<R>
where
    R: for<'r> ReadXDR<'r>,
{
    xdrs::cxs::decode::<R>(bytes).map_err(TestkitError::decode)
}

#[cfg(test)]
mod tests {
    use super::{ContractEntrypointID, decode_return, entrypoint_id};

    #[test]
    fn matches_known_erc20_selector() {
        // Same known-answer vector the CLI pins for `method_entrypoint_id`.
        assert_eq!(
            entrypoint_id("ERC20", "0.1.0", "transfer"),
            ContractEntrypointID::from([0x63, 0x28, 0x5b, 0x35, 0x23, 0xf9, 0xdc, 0x3f])
        );
    }

    #[test]
    fn cxs_round_trip_matches_helper_bounds() {
        // Confirms the encode/decode plumbing and trait bounds the `Contract`
        // helper relies on (a tuple of method arguments).
        let encoded = xdrs::cxs::encode(&(7u64, 9u32)).expect("encode");
        let (a, b): (u64, u32) = decode_return(&encoded).expect("decode");
        assert_eq!((a, b), (7, 9));
    }
}
