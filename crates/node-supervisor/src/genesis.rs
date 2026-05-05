//! hogd-dev genesis compatibility probe.

use groundhog_scheduler::{
    DbSchedResult, GenesisConfiguration, SchedulerGetTipResult,
    sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};
use groundhog_xdr::{
    genesis::InitialConfigurationList,
    storage_cost::StorageCostModelXdr,
    types::{ExtensionPoint, int128_xdr},
};

use crate::NodeSupervisorError;

/// Ensure v1 hogd-dev has a genesis block.
///
/// Current hogd-dev bootstrap normally loads account genesis before readiness,
/// so this is a no-op after `schedcoord_get_tip` succeeds. The empty-genesis
/// fallback remains for older scheduler-only development binaries. The
/// get-tip/load/re-tip flow follows
/// `chuckd-rs/crates/coordinator/src/system.rs:524-557`.
#[tracing::instrument(skip_all)]
pub(crate) async fn ensure_dev_genesis(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<(), NodeSupervisorError> {
    match scheduler_get_tip(scheduler).await? {
        SchedulerGetTipResult::SchedulerGetTipOK { .. } => Ok(()),
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
            load_dev_genesis(scheduler).await?;
            match scheduler_get_tip(scheduler).await? {
                SchedulerGetTipResult::SchedulerGetTipOK { .. } => Ok(()),
                SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
                    tracing::error!(
                        "hogd-dev scheduler still reports no genesis after load_genesis"
                    );
                    Err(NodeSupervisorError::SchedulerRejected {
                        op: "schedcoord_get_tip",
                        result: "scheduler still reports no genesis after load_genesis".into(),
                    })
                }
                other => unexpected_tip_result(&other),
            }
        }
        other => unexpected_tip_result(&other),
    }
}

async fn scheduler_get_tip(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<SchedulerGetTipResult, NodeSupervisorError> {
    scheduler
        .schedcoord_get_tip()
        .await
        .map_err(|source| NodeSupervisorError::SchedulerRpc {
            op: "schedcoord_get_tip",
            source,
        })
}

async fn load_dev_genesis(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<(), NodeSupervisorError> {
    let result = scheduler
        .schedcoord_load_genesis_configuration(&dev_genesis_config())
        .await
        .map_err(|source| NodeSupervisorError::SchedulerRpc {
            op: "schedcoord_load_genesis_configuration",
            source,
        })?;
    if result == DbSchedResult::DbSchedResultOK {
        return Ok(());
    }
    Err(NodeSupervisorError::SchedulerRejected {
        op: "schedcoord_load_genesis_configuration",
        result: result.to_string(),
    })
}

fn unexpected_tip_result<T>(result: &SchedulerGetTipResult) -> Result<T, NodeSupervisorError> {
    Err(NodeSupervisorError::SchedulerRejected {
        op: "schedcoord_get_tip",
        result: format!("{result:?}"),
    })
}

/// Build the deterministic empty fallback genesis loaded by `groundhog node`.
///
/// The empty initial-config shape follows
/// `chuckd-rs/crates/scheduler/src/client.rs:324-346`. The storage cost
/// constants mirror hogd's defaults from
/// `hogd/storage_cost/storage_cost_model.h:83-96` so PR 4b submits validate
/// against the same economics as hogd-dev's scheduler.
fn dev_genesis_config() -> GenesisConfiguration {
    GenesisConfiguration {
        initial_config: InitialConfigurationList::default(),
        storage_cost_model: StorageCostModelXdr {
            ext: ExtensionPoint::_0,
            base_data_item_cost: int128(12_000),
            bytestring_cost_per_byte: 10,
            base_contract_deployment_cost: int128(12_000),
            contract_deployment_cost_per_byte: int128(10),
            auth_per_key_cost: int128(1_111),
            rcq_cost_per_entry: int128(8),
            replay_cache_cost_per_entry: int128(7),
            balance_cost: int128(1_234),
        },
    }
}

fn int128(lowbits: u64) -> int128_xdr {
    int128_xdr {
        highbits: 0,
        lowbits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_genesis_has_empty_initial_config_and_hogd_default_costs() {
        let genesis = dev_genesis_config();
        assert!(genesis.initial_config.is_empty());

        let costs = genesis.storage_cost_model;
        assert_eq!(costs.base_data_item_cost.lowbits, 12_000);
        assert_eq!(costs.bytestring_cost_per_byte, 10);
        assert_eq!(costs.base_contract_deployment_cost.lowbits, 12_000);
        assert_eq!(costs.contract_deployment_cost_per_byte.lowbits, 10);
        assert_eq!(costs.auth_per_key_cost.lowbits, 1_111);
        assert_eq!(costs.rcq_cost_per_entry.lowbits, 8);
        assert_eq!(costs.replay_cache_cost_per_entry.lowbits, 7);
        assert_eq!(costs.balance_cost.lowbits, 1_234);
    }
}
