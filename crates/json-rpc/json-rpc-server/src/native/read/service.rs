pub(super) mod history;

use super::{NativeSimulationPlane, NativeStatePlane, StateKey, invalid, projection, rpc_error};
use crate::{BuildVersion, ChainPlane, RpcRequestContext, RpcServerConfig, RpcServerError};
use jsonrpsee::core::RpcResult;
use std::{future::Future, time::Duration};
use thruput_bundle_identity::BundleIdentity;
use thruput_json_rpc_api::WIRE_FORMAT_VERSION;
use thruput_json_rpc_types::{BlockHeight, NetworkHash, requests as input, responses as out};

/// Configured state and simulation sources, independent of their health.
/// `NativeService` owns indexer and submission registration separately.
#[derive(Debug, Clone, Copy)]
pub struct NativeReadCapabilities {
    db: bool,
    simulation: bool,
}
impl NativeReadCapabilities {
    /// Select configured `DbGateway` and `ViewGateway` sources, not their health.
    pub const fn new(db: bool, simulation: bool) -> Self {
        Self { db, simulation }
    }
    /// Sorted implemented method names for these configured sources.
    pub fn method_names(self) -> Vec<&'static str> {
        let mut names = vec!["getVersionInfo", "getNetworkInfo", "getServerLimits"];
        if self.db {
            names.extend([
                "getChainTip",
                "getChainConfig",
                "getStorage",
                "getBalance",
                "getRegisteredAuth",
                "getContractDeployment",
                "getEffectiveContract",
                "getContractArtifact",
            ]);
        }
        if self.simulation {
            names.extend(["simulateInvocation", "simulateInvocations"]);
        }
        names.sort_unstable();
        names
    }
}

/// Read-only producers over caller-owned backend connections.
/// Tip/config calls reuse the backend coalescer and the shared submit cache.
#[derive(Debug)]
pub struct NativeReadService<C, R, V> {
    chain: C,
    state: R,
    simulation: V,
    network_hash: NetworkHash,
    config: RpcServerConfig,
    version: out::VersionInfo,
    capabilities: NativeReadCapabilities,
}

impl<C, R, V> NativeReadService<C, R, V> {
    /// Construct producers without registering public methods.
    pub fn new(
        chain: C,
        state: R,
        simulation: V,
        network_hash: NetworkHash,
        config: RpcServerConfig,
        build: BuildVersion,
        capabilities: NativeReadCapabilities,
    ) -> Self {
        let mut version = out::VersionInfo::new(WIRE_FORMAT_VERSION, vec![], build.crate_version());
        version.commit_hash = build.source_commit().map(str::to_owned);
        version.platform_version = build.platform_version().map(str::to_owned);
        Self {
            chain,
            state,
            simulation,
            network_hash,
            config,
            version,
            capabilities,
        }
    }
    /// Preserve installed-bundle provenance; release fields are not supplied by requests.
    #[must_use]
    pub fn with_bundle_identity(mut self, identity: &BundleIdentity) -> Self {
        identity
            .build_version()
            .crate_version()
            .clone_into(&mut self.version.implementation);
        self.version.commit_hash = identity.build_version().source_commit().map(str::to_owned);
        self.version.platform_version = identity
            .build_version()
            .platform_version()
            .map(str::to_owned);
        self.version.release_version = identity.release_version().map(str::to_owned);
        self.version.bundle_manifest_digest = identity.bundle_manifest_digest().map(str::to_owned);
        self
    }
    /// Implemented methods enabled by this service configuration.
    pub fn method_names(&self) -> Vec<&'static str> {
        self.capabilities.method_names()
    }
    fn context(&self) -> RpcRequestContext {
        RpcRequestContext::new(self.config.request_timeout_ms.map(Duration::from_millis))
    }
    fn require(configured: bool, method: &str) -> Result<(), RpcServerError> {
        if configured {
            Ok(())
        } else {
            Err(RpcServerError::NotImplemented(method.into()))
        }
    }
    async fn within<T>(
        &self,
        context: RpcRequestContext,
        future: impl Future<Output = Result<T, RpcServerError>>,
    ) -> RpcResult<T> {
        let result = if let Some(deadline) = context.deadline() {
            tokio::time::timeout_at(deadline.into(), future)
                .await
                .map_err(|_| RpcServerError::BackendTimeout)
                .and_then(std::convert::identity)
        } else {
            future.await
        };
        result.map_err(|error| rpc_error(&error))
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_version_info(&self, _: input::EmptyRequest) -> RpcResult<out::VersionInfo> {
        let mut result = self.version.clone();
        result.supported_methods = self.method_names().into_iter().map(str::to_owned).collect();
        Ok(result)
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_network_info(&self, _: input::EmptyRequest) -> RpcResult<out::NetworkInfo> {
        Ok(out::NetworkInfo {
            network_hash: self.network_hash,
        })
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_server_limits(&self, _: input::EmptyRequest) -> RpcResult<out::ServerLimits> {
        let config = &self.config;
        let mut result = out::ServerLimits::new(
            config.view_gas_limit,
            config.max_view_calldata_bytes as u64,
            config.event_page_cap(),
            config.transaction_page_cap(),
            config.max_cursor_bytes as u64,
        );
        if self.capabilities.simulation {
            result.simulation_batch = Some(out::SimulationBatchLimits {
                max_invocations: config.view_batch.max_calls,
                max_calldata_bytes: config.view_batch.max_calldata_bytes,
                max_gas: config.view_batch.max_gas,
                max_backend_response_bytes: config.view_batch.max_response_bytes,
                timeout_ms: config
                    .view_timeout_ms
                    .min(config.request_timeout_ms.unwrap_or(u64::MAX)),
            });
        }
        Ok(result)
    }
}
impl<C, R: super::NativeBlockPlane, V> NativeReadService<C, R, V> {
    /// Read committed metadata from the configured indexer. `NativeService` owns
    /// the indexer capability gate; this producer does not register a method.
    #[tracing::instrument(skip_all)]
    pub async fn get_block_by_height(
        &self,
        request: input::BlockQuery,
    ) -> RpcResult<out::BlockResult> {
        let context = self.context();
        self.within(context, async {
            if *request.block_height < 0 {
                return Err(RpcServerError::InvalidParam(
                    "blockHeight must be nonnegative".into(),
                ));
            }
            let result = self
                .state
                .native_block(context, request.block_height.clone())
                .await?;
            if let Some(block) = &result {
                if *block.block_height <= 0 || block.block_height != request.block_height {
                    return Err(invalid("block metadata has an unexpected height"));
                }
                if block.network_hash != self.network_hash {
                    return Err(invalid("block metadata has a different network hash"));
                }
            }
            Ok(result)
        })
        .await
    }
}
impl<C: ChainPlane, R: NativeStatePlane, V: NativeSimulationPlane> NativeReadService<C, R, V> {
    pub async fn get_effective_contract(
        &self,
        request: input::EffectiveContractQuery,
    ) -> RpcResult<out::EffectiveContractResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getEffectiveContract")?;
            // Both reads are committed snapshots. Equal heights bind the fallback
            // configuration and deployment absence to the same state. Bound retries
            // rather than return a mixed-state selection during commit churn.
            for _ in 0..3 {
                let deployed = self
                    .state
                    .native_contract_deployment(context, request.address)
                    .await?;
                snapshot(&deployed.state_block_height)?;
                if deployed.address != request.address {
                    return Err(invalid("deployment read returned another address"));
                }
                let selection = if let Some(contract) = deployed.deployment {
                    Some(out::EffectiveContract::ExplicitDeployment { contract })
                } else if request.context == input::InvocationContext::Root {
                    let tip = self.chain.chain_tip(context).await?;
                    snapshot(&tip.tip.height)?;
                    if tip.tip.height != deployed.state_block_height {
                        continue;
                    }
                    let contract = tip.default_wallet.ok_or_else(|| {
                        RpcServerError::BackendUnavailable(
                            "chain default contract configuration unavailable".into(),
                        )
                    })?;
                    Some(out::EffectiveContract::NetworkDefault { contract })
                } else {
                    None
                };
                return Ok(out::EffectiveContractResult {
                    address: request.address,
                    context: request.context,
                    state_block_height: deployed.state_block_height,
                    selection,
                });
            }
            Err(RpcServerError::BackendUnavailable(
                "could not read contract selection at one committed height".into(),
            ))
        })
        .await
    }

    pub async fn get_contract_deployment(
        &self,
        request: input::AddressQuery,
    ) -> RpcResult<out::ContractDeploymentResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getContractDeployment")?;
            let result = self
                .state
                .native_contract_deployment(context, request.address)
                .await?;
            snapshot(&result.state_block_height)?;
            if result.address != request.address {
                return Err(invalid("deployment read returned another address"));
            }
            Ok(result)
        })
        .await
    }

    pub async fn get_contract_artifact(
        &self,
        request: input::ContractArtifactQuery,
    ) -> RpcResult<out::ContractArtifactResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getContractArtifact")?;
            let result = self
                .state
                .native_contract_artifact(context, request.hash)
                .await?;
            if result.hash != request.hash {
                return Err(invalid("artifact read returned another hash"));
            }
            Ok(result)
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_chain_tip(&self, _: input::EmptyRequest) -> RpcResult<out::ChainTip> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getChainTip")?;
            projection::tip(&self.chain.chain_tip(context).await?.tip)
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_chain_config(&self, _: input::EmptyRequest) -> RpcResult<out::ChainConfig> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getChainConfig")?;
            projection::config(self.chain.chain_tip(context).await?)
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_storage(&self, request: input::StorageQuery) -> RpcResult<out::StorageResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getStorage")?;
            let key = request.key.0.as_slice().try_into().map_err(|_| {
                RpcServerError::InvalidParam("storage key must be exactly 32 bytes".into())
            })?;
            let result = self
                .state
                .native_storage(context, request.address, StateKey::ContractData(key))
                .await?;
            snapshot(&result.state_block_height)?;
            Ok(result)
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_balance(&self, request: input::AddressQuery) -> RpcResult<out::BalanceResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getBalance")?;
            let result = self
                .state
                .native_storage(context, request.address, StateKey::FreeNativeTokenBalance)
                .await?;
            snapshot(&result.state_block_height)?;
            let raw_balance = match result.item {
                None => 0,
                Some(out::StorageItem::NonnegativeInt { value }) if value >= 0 => value,
                _ => return Err(invalid("balance row is not a nonnegative integer")),
            };
            Ok(out::BalanceResult {
                address: request.address,
                state_block_height: result.state_block_height,
                raw_balance,
            })
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn get_registered_auth(
        &self,
        request: input::AddressQuery,
    ) -> RpcResult<out::RegisteredAuthResult> {
        let context = self.context();
        self.within(context, async {
            Self::require(self.capabilities.db, "getRegisteredAuth")?;
            let result = self
                .state
                .native_registered_auth(context, request.address)
                .await?;
            snapshot(&result.state_block_height)?;
            if result.address != request.address {
                return Err(invalid("authorization response has a different address"));
            }
            Ok(result)
        })
        .await
    }
    /// Run a view that has a `minStateBlockHeight`, and repeat it while the
    /// view snapshot is below that height but the height is committed.
    ///
    /// A block's receipt can arrive before the view plane serves its state, so
    /// a read at a committed height waits here for the view to catch up. The
    /// wait ends after [`VIEW_HEIGHT_MAX_WAIT`] or at the request's view
    /// deadline, whichever is first. Then the last `ViewHeightUnavailable` is
    /// returned, not a timeout.
    ///
    /// A floor above the committed tip fails at once, as Anvil does for a
    /// future block and Solana does for `minContextSlot`. So does any floor
    /// when this service cannot read the tip.
    async fn await_view_height<T, F, Fut>(
        &self,
        context: RpcRequestContext,
        mut attempt: F,
    ) -> Result<T, RpcServerError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, RpcServerError>>,
    {
        let Some(deadline) = context.deadline() else {
            return attempt().await;
        };
        let deadline = tokio::time::Instant::from_std(deadline);
        let mut pause = VIEW_HEIGHT_RETRY_FIRST;
        let mut behind = None;
        // Until a view is known to be behind, an attempt may use the whole
        // view deadline. After that, every retry ends by `wait_until`.
        let mut wait_until = deadline;
        loop {
            let Ok(outcome) = tokio::time::timeout_at(wait_until, attempt()).await else {
                return Err(behind.unwrap_or(RpcServerError::BackendTimeout));
            };
            let (error, requested) = match outcome {
                Err(RpcServerError::ViewHeightUnavailable(value))
                    if *value.observed >= 0 && value.observed < value.requested =>
                {
                    let requested = *value.requested;
                    (RpcServerError::ViewHeightUnavailable(value), requested)
                }
                // A retry that reached the deadline keeps the observed height.
                Err(RpcServerError::BackendTimeout) => {
                    return Err(behind.unwrap_or(RpcServerError::BackendTimeout));
                }
                other => return other,
            };
            if behind.is_none() {
                // A tip read still running at the deadline counts as not
                // committed, so the client gets this error, not a timeout.
                let committed =
                    tokio::time::timeout_at(deadline, self.is_committed(context, requested)).await;
                if committed != Ok(true) {
                    return Err(error);
                }
                wait_until = deadline.min(tokio::time::Instant::now() + VIEW_HEIGHT_MAX_WAIT);
            }
            let now = tokio::time::Instant::now();
            if now >= wait_until {
                return Err(error);
            }
            tokio::time::sleep_until(wait_until.min(now + pause)).await;
            // No retry starts once the wait is over.
            if tokio::time::Instant::now() >= wait_until {
                return Err(error);
            }
            behind = Some(error);
            pause = (pause * 2).min(VIEW_HEIGHT_RETRY_MAX);
        }
    }
    /// Whether the committed tip has reached `height`. An unreadable tip
    /// counts as not reached, so the caller keeps its own error.
    async fn is_committed(&self, context: RpcRequestContext, height: i64) -> bool {
        if !self.capabilities.db {
            return false;
        }
        match self.chain.chain_tip(context).await {
            Ok(reading) => *reading.tip.height >= height,
            Err(error) => {
                tracing::debug!(%error, "tip unreadable; floored view is not retried");
                false
            }
        }
    }
    #[tracing::instrument(skip_all)]
    pub async fn simulate_invocation(
        &self,
        request: input::SimulateInvocationRequest,
    ) -> RpcResult<out::SimulationResult> {
        let context = self
            .context()
            .capped_from_start(Duration::from_millis(self.config.view_timeout_ms));
        self.within(context, async {
            Self::require(self.capabilities.simulation, "simulateInvocation")?;
            floor(request.min_state_block_height.as_ref())?;
            let gas = validate_call(
                &request.entrypoint.0,
                request.calldata.0.len(),
                request.gas_limit,
                &self.config,
            )?;
            let min = request.min_state_block_height.clone();
            self.await_view_height(context, || async {
                let result = self
                    .simulation
                    .simulate(context, request.clone(), gas)
                    .await?;
                snapshot(&result.state_block_height)?;
                if let Some(requested) = min.clone().filter(|min| result.state_block_height < *min)
                {
                    return Err(RpcServerError::ViewHeightUnavailable(
                        thruput_json_rpc_types::view::ViewHeightUnavailable {
                            requested,
                            observed: result.state_block_height,
                        },
                    ));
                }
                Ok(result)
            })
            .await
        })
        .await
    }
    #[tracing::instrument(skip_all)]
    pub async fn simulate_invocations(
        &self,
        request: input::SimulateInvocationsRequest,
    ) -> RpcResult<out::SimulationResults> {
        let context = self
            .context()
            .capped_from_start(Duration::from_millis(self.config.view_timeout_ms));
        self.within(context, async {
            Self::require(self.capabilities.simulation, "simulateInvocations")?;
            floor(request.min_state_block_height.as_ref())?;
            let count = request.invocations.len();
            if count == 0 || count > self.config.view_batch.max_calls as usize {
                return Err(RpcServerError::InvalidParam(
                    "invocation count exceeds endpoint limits or is empty".into(),
                ));
            }
            let mut gas_left = self.config.view_batch.max_gas;
            let mut bytes_left = self.config.view_batch.max_calldata_bytes as usize;
            for call in &request.invocations {
                let gas = validate_call(
                    &call.entrypoint.0,
                    call.calldata.0.len(),
                    call.gas_limit,
                    &self.config,
                )?;
                gas_left = gas_left.checked_sub(gas).ok_or_else(|| {
                    RpcServerError::InvalidParam(
                        "aggregate simulation gas exceeds endpoint limit".into(),
                    )
                })?;
                bytes_left = bytes_left
                    .checked_sub(call.calldata.0.len())
                    .ok_or_else(|| {
                        RpcServerError::InvalidParam(
                            "aggregate simulation calldata exceeds endpoint limit".into(),
                        )
                    })?;
            }
            let min = request.min_state_block_height.clone();
            let result = self
                .await_view_height(context, || {
                    self.simulation.simulate_batch(
                        context,
                        request.clone(),
                        self.config.view_gas_limit,
                        self.config.view_batch,
                    )
                })
                .await?;
            snapshot(&result.state_block_height)?;
            if result.results.len() != count {
                return Err(RpcServerError::invalid_data(
                    super::BackendDataInvalidReason::InvalidCardinality,
                    "simulation result count mismatch",
                ));
            }
            if min.is_some_and(|min| result.state_block_height < min) {
                return Err(invalid(
                    "batch success is below the requested snapshot floor",
                ));
            }
            Ok(result)
        })
        .await
    }
}
/// First pause before a floored view is retried. The pause doubles up to
/// [`VIEW_HEIGHT_RETRY_MAX`].
const VIEW_HEIGHT_RETRY_FIRST: Duration = Duration::from_millis(10);
/// Longest pause between floored view attempts. hogd's view gateway checks
/// for a newer committed tip about every 100 ms.
const VIEW_HEIGHT_RETRY_MAX: Duration = Duration::from_millis(100);
/// Longest wait for the view to reach a committed floor. A healthy view
/// gateway catches up in about 100 ms; a view still behind after this is not
/// catching up, and the client gets `StateHeightUnavailable` to retry.
const VIEW_HEIGHT_MAX_WAIT: Duration = Duration::from_secs(2);

pub(super) fn snapshot(height: &BlockHeight) -> Result<(), RpcServerError> {
    if **height == -1 {
        return Err(RpcServerError::BackendUnavailable(
            "no committed snapshot".into(),
        ));
    }
    if **height < 0 {
        Err(invalid("negative committed snapshot height"))
    } else {
        Ok(())
    }
}
fn floor(height: Option<&BlockHeight>) -> Result<(), RpcServerError> {
    if height.is_some_and(|height| **height < 0) {
        Err(RpcServerError::InvalidParam(
            "minStateBlockHeight must be nonnegative".into(),
        ))
    } else {
        Ok(())
    }
}
fn validate_call(
    entrypoint: &[u8],
    bytes: usize,
    gas: Option<u64>,
    config: &RpcServerConfig,
) -> Result<u64, RpcServerError> {
    if entrypoint.len() != 8 {
        return Err(RpcServerError::InvalidParam(
            "entrypoint must be exactly 8 bytes".into(),
        ));
    }
    if bytes > config.max_view_calldata_bytes {
        return Err(RpcServerError::InvalidParam(
            "simulation calldata exceeds endpoint limit".into(),
        ));
    }
    let gas = gas.unwrap_or(config.view_gas_limit);
    if gas == 0 || gas > config.view_gas_limit {
        return Err(RpcServerError::InvalidParam(
            "gasLimit must be positive and within the endpoint limit".into(),
        ));
    }
    Ok(gas)
}

impl<C: ChainPlane, R, V> NativeReadService<C, R, V> {
    /// Read this local runtime's pending pool count, without claiming network-wide state.
    #[tracing::instrument(skip_all)]
    pub async fn get_pending_transaction_count(
        &self,
        _: input::EmptyRequest,
    ) -> RpcResult<out::PendingTransactionCount> {
        let context = self.context();
        self.within(context, async {
            Ok(out::PendingTransactionCount {
                count: self.chain.pending_transaction_count(context).await?,
            })
        })
        .await
    }
}
