//! Assemble implemented native handlers without selecting a listening endpoint.

use jsonrpsee::{core::RegisterMethodError, types::ErrorObjectOwned};
use thruput_bundle_identity::BundleIdentity;
use thruput_json_rpc_api::native::{METHOD_NAMES, NativeApi};
use thruput_json_rpc_types::{NetworkHash, network_info::NetworkInfo};

use super::{
    FinalizedNativeModule, NativeModule,
    read::{
        NativeBlockPlane, NativeHistoryPlane, NativeReadCapabilities, NativeReadService,
        NativeSimulationPlane, NativeStatePlane,
    },
    transaction::{NativeSubmitPlane, NativeTransactionReadPlane},
};
use crate::{BuildInfo, BuildVersion, ChainPlane, GroundhogRpcServer, RpcServerConfig};

/// Configured backend planes. Transient health does not change registration.
/// Optional funding remains outside this composition.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "Each independent backend enables a defined method group"
)]
pub struct NativeCapabilities {
    scheduler: bool,
    indexer: bool,
    db: bool,
    simulation: bool,
}

impl NativeCapabilities {
    /// Record configured sources after startup has validated their identity.
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "Each argument records an independent configured backend plane"
    )]
    pub const fn new(scheduler: bool, indexer: bool, db: bool, simulation: bool) -> Self {
        Self {
            scheduler,
            indexer,
            db,
            simulation,
        }
    }

    fn supports(self, method: &str) -> bool {
        match method {
            "getVersionInfo" | "getNetworkInfo" | "getServerLimits" => true,
            "sendTransaction" => self.scheduler,
            "getTransactionReceipt"
            | "getBlockByHeight"
            | "getEvents"
            | "getTransactionsByRootAddress"
            | "getTransactionsByBlock" => self.indexer,
            "getTransactionStatus" => self.indexer && self.scheduler,
            "getChainTip" | "getChainConfig" | "getStorage" | "getBalance"
            | "getRegisteredAuth" => self.db,
            "simulateInvocation" | "simulateInvocations" => self.simulation,
            _ => false,
        }
    }

    /// Sorted implemented methods enabled by these configured planes.
    pub fn method_names(self) -> Vec<&'static str> {
        let mut names: Vec<_> = METHOD_NAMES
            .iter()
            .copied()
            .filter(|name| self.supports(name))
            .collect();
        names.sort_unstable();
        names
    }
}

/// Shared native service over caller-owned backend connections.
///
/// This assembles discovery, state, simulation, history and transaction lifecycle methods.
/// It does not bind a socket or change either production binary's registration.
#[derive(Debug)]
pub struct NativeService<S, C, R, V> {
    reads: NativeReadService<C, R, V>,
    transactions: GroundhogRpcServer<S, C, R, ()>,
    capabilities: NativeCapabilities,
}

impl<S, C: ChainPlane + Clone, R: Clone, V> NativeService<S, C, R, V> {
    /// Use one configuration and network identity for every handler. Cloned
    /// backend handles must share their established connections and coalescers.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        submit: S,
        chain: C,
        read: R,
        simulation: V,
        network_hash: NetworkHash,
        mut config: RpcServerConfig,
        build: BuildVersion,
        capabilities: NativeCapabilities,
    ) -> Self {
        if !capabilities.db {
            // An absent DB cannot supply the optional admission precheck cache.
            config.tip_refresh_interval_ms = 0;
        }
        let reads = NativeReadService::new(
            chain.clone(),
            read.clone(),
            simulation,
            network_hash,
            config.clone(),
            build,
            NativeReadCapabilities::new(capabilities.db, capabilities.simulation),
        );
        let transactions = GroundhogRpcServer::new(
            submit,
            chain,
            read,
            (),
            NetworkInfo::new(network_hash),
            config,
            BuildInfo::from_build_version(build, None),
        );
        Self {
            reads,
            transactions,
            capabilities,
        }
    }
}

impl<S, C, R, V> NativeService<S, C, R, V> {
    /// Attach validated installed-bundle provenance to discovery.
    #[must_use]
    pub fn with_bundle_identity(mut self, identity: &BundleIdentity) -> Self {
        self.reads = self.reads.with_bundle_identity(identity);
        self.transactions = self.transactions.with_bundle_identity(identity);
        self
    }
}

impl<S, C, R, V> NativeService<S, C, R, V>
where
    S: NativeSubmitPlane,
    C: ChainPlane,
    R: NativeStatePlane + NativeTransactionReadPlane + NativeBlockPlane + NativeHistoryPlane,
    V: NativeSimulationPlane,
{
    /// Remove unavailable and unimplemented methods, then freeze discovery.
    /// The caller supplies transport policy and owns the returned module.
    pub fn into_module(self) -> Result<FinalizedNativeModule<Self>, RegisterMethodError> {
        let capabilities = self.capabilities;
        let mut module = NativeModule::new(self)?;
        for name in METHOD_NAMES {
            if !capabilities.supports(name) {
                module.remove_method(name);
            }
        }
        Ok(module.finalize())
    }
}

fn method_not_found() -> ErrorObjectOwned {
    ErrorObjectOwned::owned(
        jsonrpsee::types::error::METHOD_NOT_FOUND_CODE,
        "Method not found",
        None::<()>,
    )
}

macro_rules! dispatch {
    (get_version_info, $s:expr, $r:expr) => {{
        let mut result = $s.reads.get_version_info($r).await?;
        result.supported_methods = $s
            .capabilities
            .method_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Ok(result)
    }};
    (get_network_info, $s:expr, $r:expr) => {
        $s.reads.get_network_info($r).await
    };
    (get_server_limits, $s:expr, $r:expr) => {
        $s.reads.get_server_limits($r).await
    };
    (get_events, $s:expr, $r:expr) => {
        $s.reads.get_events($r).await
    };
    (get_transactions_by_root_address, $s:expr, $r:expr) => {
        $s.reads.get_transactions_by_root_address($r).await
    };
    (get_transactions_by_block, $s:expr, $r:expr) => {
        $s.reads.get_transactions_by_block($r).await
    };
    (get_block_by_height, $s:expr, $r:expr) => {
        $s.reads.get_block_by_height($r).await
    };
    (get_chain_tip, $s:expr, $r:expr) => {
        $s.reads.get_chain_tip($r).await
    };
    (get_chain_config, $s:expr, $r:expr) => {
        $s.reads.get_chain_config($r).await
    };
    (get_storage, $s:expr, $r:expr) => {
        $s.reads.get_storage($r).await
    };
    (get_balance, $s:expr, $r:expr) => {
        $s.reads.get_balance($r).await
    };
    (get_registered_auth, $s:expr, $r:expr) => {
        $s.reads.get_registered_auth($r).await
    };
    (simulate_invocation, $s:expr, $r:expr) => {
        $s.reads.simulate_invocation($r).await
    };
    (simulate_invocations, $s:expr, $r:expr) => {
        $s.reads.simulate_invocations($r).await
    };
    (send_transaction, $s:expr, $r:expr) => {
        $s.transactions.native_send_transaction($r).await
    };
    (get_transaction_status, $s:expr, $r:expr) => {
        $s.transactions.native_transaction_status($r).await
    };
    (get_transaction_receipt, $s:expr, $r:expr) => {
        $s.transactions.native_transaction_receipt($r).await
    };
    ($name:ident, $s:expr, $r:expr) => {{
        let _ = $r;
        Err(method_not_found())
    }};
}

macro_rules! implement {
    ($( $(#[$doc:meta])* $name:ident($wire:literal,$parse:ident,$decorate:ident): $request:ty => $response:ty; )*) => {
        impl<S, C, R, V> NativeApi for NativeService<S, C, R, V>
        where
            S: NativeSubmitPlane,
            C: ChainPlane,
            R: NativeStatePlane + NativeTransactionReadPlane + NativeBlockPlane + NativeHistoryPlane,
            V: NativeSimulationPlane,
        {
            $(
                #[tracing::instrument(skip_all)]
                async fn $name(&self, request: $request) -> jsonrpsee::core::RpcResult<$response> {
                    if !self.capabilities.supports($wire) { return Err(method_not_found()); }
                    dispatch!($name, self, request)
                }
            )*
        }
    };
}
thruput_json_rpc_api::native_methods!(implement);
