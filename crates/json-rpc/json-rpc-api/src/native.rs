//! Candidate native contracts. These declarations do not activate wire 14.
//!
//! The catalog owns names, request/result associations and documentation for the
//! explicit server/client adapters. jsonrpsee's generated positional/wrapped
//! argument adapters are intentionally not used for whole-object parameters.

/// Target version for the candidate schema, independent of the serving version.
pub const CANDIDATE_WIRE_FORMAT_VERSION: &str = "14.0.0";

/// Expand the native method catalog into a consumer macro.
///
/// Each entry supplies documentation, a Rust method, a wire name, parsing mode
/// (`empty` or `named`), result decoration (`version` or `plain`), and the request
/// and result types. This shared declaration also drives the transport tests.
#[macro_export]
macro_rules! native_methods {
    ($consumer:ident) => {
        $consumer! {
            /// Describe compatibility and configured methods.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_version_info("getVersionInfo", empty, version): $crate::types::requests::EmptyRequest => $crate::types::responses::VersionInfo;
            /// Read the signing network identity.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_network_info("getNetworkInfo", empty, plain): $crate::types::requests::EmptyRequest => $crate::types::responses::NetworkInfo;
            /// Read the committed chain tip.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_chain_tip("getChainTip", empty, plain): $crate::types::requests::EmptyRequest => $crate::types::responses::ChainTip;
            /// Read the chain configuration and observation height.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_chain_config("getChainConfig", empty, plain): $crate::types::requests::EmptyRequest => $crate::types::responses::ChainConfig;
            /// Read configured request limits.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_server_limits("getServerLimits", empty, plain): $crate::types::requests::EmptyRequest => $crate::types::responses::ServerLimits;
            /// Submit a signed envelope for authoritative admission.
            send_transaction("sendTransaction", named, plain): $crate::types::requests::SendTransactionRequest => $crate::types::responses::SendTransactionResult;
            /// Read retained transaction observations.
            get_transaction_status("getTransactionStatus", named, plain): $crate::types::requests::TransactionQuery => $crate::types::responses::TransactionStatus;
            /// Read indexed execution, or null when no receipt is available.
            get_transaction_receipt("getTransactionReceipt", named, plain): $crate::types::requests::TransactionQuery => $crate::types::responses::ReceiptResult;
            /// Simulate an invocation without committing state.
            simulate_invocation("simulateInvocation", named, plain): $crate::types::requests::SimulateInvocationRequest => $crate::types::responses::SimulationResult;
            /// Simulate a batch against one state snapshot.
            simulate_invocations("simulateInvocations", named, plain): $crate::types::requests::SimulateInvocationsRequest => $crate::types::responses::SimulationResults;
            /// Read serialized storage at an address.
            get_storage("getStorage", named, plain): $crate::types::requests::StorageQuery => $crate::types::responses::StorageResult;
            /// Read native HOG balance and its state height.
            get_balance("getBalance", named, plain): $crate::types::requests::AddressQuery => $crate::types::responses::BalanceResult;
            /// Read registered authorization at an address.
            get_registered_auth("getRegisteredAuth", named, plain): $crate::types::requests::AddressQuery => $crate::types::responses::RegisteredAuthResult;
            /// Scan indexed events matching the supplied filters.
            get_events("getEvents", named, plain): $crate::types::requests::EventQuery => $crate::types::responses::EventPage;
            /// Read transactions whose root invocation targets the address, excluding internal calls and incoming transfers.
            get_transactions_by_root_address("getTransactionsByRootAddress", named, plain): $crate::types::requests::RootAddressQuery => $crate::types::responses::TransactionRefPage;
            /// Read committed block metadata, or null when unavailable.
            get_block_by_height("getBlockByHeight", named, plain): $crate::types::requests::BlockQuery => $crate::types::responses::BlockResult;
            /// Read a page of transactions in a committed block.
            get_transactions_by_block("getTransactionsByBlock", named, plain): $crate::types::requests::BlockPageRequest => $crate::types::responses::BlockTransactionPage;
            /// Request one faucet funding attempt. Receipt polling is a separate operation.
            request_airdrop("faucet_requestAirdrop", named, plain): $crate::types::requests::AirdropRequest => $crate::types::responses::AirdropAttempt;
            /// Submit a local funding transfer.
            fund_account("dev_fundAccount", named, plain): $crate::types::requests::FundAccountRequest => $crate::types::responses::FundingSubmission;
            /// Read the local scheduler pending count.
            /// Accepts omitted params, `{}`, `[]`, or `null`; clients send `{}`.
            get_pending_transaction_count("dev_getPendingTransactionCount", empty, plain): $crate::types::requests::EmptyRequest => $crate::types::responses::PendingTransactionCount;
        }
    };
}

macro_rules! declare_api {
    ($( $(#[$doc:meta])* $name:ident($wire:literal, $parse:ident, $decorate:ident): $request:ty => $response:ty; )*) => {
        /// Native service contract. Backends implement semantics; adapters own
        /// strict transport parsing and typed serialization. Unconfigured methods
        /// must be removed before finalizing a module, not exposed as stubs.
        pub trait NativeApi: Send + Sync + 'static {
            $(
                $(#[$doc])*
                fn $name(&self, request: $request)
                    -> impl std::future::Future<Output = jsonrpsee::core::RpcResult<$response>> + Send;
            )*
        }

        #[thruput_open_rpc_macros::open_rpc(params = "object")]
        pub trait NativeApi {
            $(
                $(#[$doc])*
                #[method(name = $wire)]
                async fn $name(&self, request: $request) -> jsonrpsee::core::RpcResult<$response>;
            )*
        }

        /// All core and optional native method names, in declaration order.
        pub const METHOD_NAMES: &[&str] = &[$($wire),*];
    };
}
native_methods!(declare_api);
