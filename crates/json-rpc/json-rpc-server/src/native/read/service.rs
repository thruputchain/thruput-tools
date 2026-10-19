use super::{NativeSimulationPlane, NativeStatePlane, StateKey, invalid, projection, rpc_error};
use crate::{BuildVersion, ChainPlane, RpcRequestContext, RpcServerConfig, RpcServerError};
use jsonrpsee::core::RpcResult;
use std::{future::Future, time::Duration};
use thruput_bundle_identity::BundleIdentity;
use thruput_json_rpc_api::native::CANDIDATE_WIRE_FORMAT_VERSION;
use thruput_json_rpc_types::{BlockHeight, NetworkHash, requests as input, responses as out};

/// Configured sources, independent of their current health. This slice has no
/// submit, indexer or funding handlers and cannot advertise those methods.
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
    /// Sorted implemented method names for candidate test composition.
    pub fn method_names(self) -> Vec<&'static str> {
        let mut names = vec!["getVersionInfo", "getNetworkInfo", "getServerLimits"];
        if self.db {
            names.extend([
                "getChainTip",
                "getChainConfig",
                "getStorage",
                "getBalance",
                "getRegisteredAuth",
            ]);
        }
        if self.simulation {
            names.extend(["simulateInvocation", "simulateInvocations"]);
        }
        names.sort_unstable();
        names
    }
}

/// Native read-only producers over the same backend connections used by wire 13.
/// Register them only in a candidate test composition until activation supplies
/// the complete public contract. Tip/config calls reuse the backend coalescer;
/// they do not change the legacy submit cache or its refresh task.
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
    /// Construct candidate producers without registering any public methods.
    pub fn new(
        chain: C,
        state: R,
        simulation: V,
        network_hash: NetworkHash,
        config: RpcServerConfig,
        build: BuildVersion,
        capabilities: NativeReadCapabilities,
    ) -> Self {
        let mut version =
            out::VersionInfo::new(CANDIDATE_WIRE_FORMAT_VERSION, vec![], build.crate_version());
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
            config.max_events_per_page,
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
impl<C: ChainPlane, R: NativeStatePlane, V: NativeSimulationPlane> NativeReadService<C, R, V> {
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
            let result = self.simulation.simulate(context, request, gas).await?;
            snapshot(&result.state_block_height)?;
            if let Some(requested) = min.filter(|min| result.state_block_height < *min) {
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
                .simulation
                .simulate_batch(
                    context,
                    request,
                    self.config.view_gas_limit,
                    self.config.view_batch,
                )
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
