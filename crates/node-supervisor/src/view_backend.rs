//! hogd-dev ViewGateway-backed simulation adapter.

use std::{fmt, net::SocketAddr, time::Duration, time::SystemTime};

use async_trait::async_trait;
use thruput_hogd_backend::{intro, view as shared_view};
use thruput_json_rpc_server::{RpcRequestContext, RpcServerError};
use thruput_json_rpc_types::protocol_token::protocol_from_text;
use thruput_node_rpc::sched_coord::SchedulerCoord_SchedulerCoord1_Client;
use thruput_view::ViewGateway_ViewGateway1_Client;
use thruput_x23_transport::connect;
use thruput_xdr::types::TimePoint;

use crate::{
    NodeSupervisorError,
    block_driver::{NextBlockTime, SharedBlockClock, tip_parent},
    request_deadline::within_request_deadline,
    service::VIEW_GATEWAY,
    stub_backend::StubBackend,
};

const HOGD_DEV_POOL_SIZE: usize = 2;

/// Local-node simulation backend, using hogd-dev's optional `ViewGateway`.
#[derive(Clone)]
pub(crate) enum HogdDevViewBackend {
    Gateway {
        client: ViewGateway_ViewGateway1_Client,
        request_timeout: Duration,
        /// The block driver's clock: a view runs at the next block's time.
        clock: SharedBlockClock,
        /// Read for a view's tip before the block driver publishes one, and
        /// to rerun a stale view; see [`HogdDevViewBackend::fresh_view_time`].
        /// Without it, views use the wall clock until a tip is published.
        scheduler: Option<SchedulerCoord_SchedulerCoord1_Client>,
    },
    Stub(StubBackend),
}

impl fmt::Debug for HogdDevViewBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway { .. } => f.debug_tuple("HogdDevViewBackend::Gateway").finish(),
            Self::Stub(_) => f.debug_tuple("HogdDevViewBackend::Stub").finish(),
        }
    }
}

impl HogdDevViewBackend {
    pub(crate) async fn connect_optional(
        endpoint: Option<SocketAddr>,
        protocol_version: &str,
        expected_protocol: Option<&str>,
        request_timeout: Duration,
        clock: SharedBlockClock,
        scheduler: Option<SchedulerCoord_SchedulerCoord1_Client>,
    ) -> Result<Self, NodeSupervisorError> {
        let Some(endpoint) = endpoint else {
            tracing::info!(
                "hogd-dev did not advertise a ViewGateway endpoint; view is unavailable"
            );
            return Ok(Self::Stub(StubBackend::new()));
        };

        tracing::debug!(%endpoint, "connecting to hogd-dev ViewGateway");
        let client = connect(
            "hogd-dev ViewGateway",
            &endpoint.to_string(),
            HOGD_DEV_POOL_SIZE,
            ViewGateway_ViewGateway1_Client::new,
        )
        .await
        .map_err(|source| NodeSupervisorError::ViewGatewayConnect { endpoint, source })?;

        let protocol = protocol_from_text(protocol_version)?;
        let intro_outcome = client.viewgateway_intro(&protocol).await.map_err(|e| {
            NodeSupervisorError::ViewGatewayRpc {
                op: "viewgateway_intro",
                error: e.to_string(),
            }
        })?;
        let intro = intro::view_gateway(intro_outcome);
        if !intro.accepted() {
            return Err(NodeSupervisorError::intro_rejected(
                intro,
                protocol_version,
                expected_protocol,
            ));
        }

        Ok(Self::Gateway {
            client,
            request_timeout,
            clock,
            scheduler,
        })
    }

    /// The time a view runs at: the time the next block would get, and the
    /// height of the tip it builds on. Without a known tip or a scheduler,
    /// the wall clock and no height.
    async fn view_time(
        clock: &SharedBlockClock,
        scheduler: Option<&SchedulerCoord_SchedulerCoord1_Client>,
    ) -> (TimePoint, Option<i64>) {
        let next = match clock.next_block_time(SystemTime::now()) {
            Some(next) => Some(next),
            None => Self::fresh_view_time(clock, scheduler).await,
        };
        match next {
            Some(next) => (next.timepoint, Some(next.parent_height)),
            None => (shared_view::wall_clock_timepoint(), None),
        }
    }

    /// The next block's time after the scheduler's current tip.
    ///
    /// hogd opens a view's snapshot after the view's time is chosen, so a
    /// block committed in between can make that time stale: hogd refuses it
    /// when the new tip is later, or runs the view on the new block with the
    /// old time. Either way the view reruns once at this time.
    async fn fresh_view_time(
        clock: &SharedBlockClock,
        scheduler: Option<&SchedulerCoord_SchedulerCoord1_Client>,
    ) -> Option<NextBlockTime> {
        let tip = tip_parent(scheduler?).await.ok()?;
        Some(clock.next_block_time_after(tip, SystemTime::now()))
    }

    pub(crate) const fn plane_status(&self) -> crate::server::PlaneStatus {
        match self {
            Self::Gateway { .. } => crate::server::PlaneStatus::ViewGateway,
            Self::Stub(_) => crate::server::PlaneStatus::Stub,
        }
    }
}

#[cfg(test)]
mod tests;

mod native;
