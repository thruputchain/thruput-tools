//! hogd-dev genesis readiness probe.

use thruput_scheduler::{
    DbTip, SchedulerGetTipResult, sched_coord::SchedulerCoord_SchedulerCoord1_Client,
};

use crate::NodeSupervisorError;

/// Require hogd-dev to have completed genesis before reporting readiness.
#[tracing::instrument(skip_all)]
pub(crate) async fn require_dev_genesis(
    scheduler: &SchedulerCoord_SchedulerCoord1_Client,
) -> Result<DbTip, NodeSupervisorError> {
    match scheduler_get_tip(scheduler).await? {
        SchedulerGetTipResult::SchedulerGetTipOK { db_tip } => Ok(db_tip),
        SchedulerGetTipResult::SchedulerGetTipNoGenesisBlock => {
            tracing::error!("hogd-dev reported readiness before completing genesis");
            Err(NodeSupervisorError::SchedulerRejected {
                op: "schedcoord_get_tip",
                result: "scheduler has no genesis after hogd-dev readiness".into(),
            })
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

fn unexpected_tip_result<T>(result: &SchedulerGetTipResult) -> Result<T, NodeSupervisorError> {
    Err(NodeSupervisorError::SchedulerRejected {
        op: "schedcoord_get_tip",
        result: format!("{result:?}"),
    })
}
