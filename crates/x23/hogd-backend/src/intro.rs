//! Classify a hogd x23 service's protocol handshake.
//!
//! Every hogd service answers its intro with its own outcome enum, and all
//! of them carry the same three answers: accepted, protocol mismatch, or a
//! discriminant this build does not know. Callers phrase the failure
//! themselves — the gateway points an operator at `--hogd-protocol`, the
//! supervisor at the hogd-dev binary it launched — so this module classifies
//! the outcome and names it, and stops there.

use thruput_db_gateway::DbGatewayIntroOutcome;
use thruput_indexer::IndexerBlockExplorerIntroOutcome;
use thruput_node_rpc::SchedGatewayIntroOutcome;
use thruput_view::ViewGatewayIntroOutcome;

/// One service's answer to the protocol intro handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntroOutcome {
    service: &'static str,
    name: &'static str,
    code: i32,
    accepted: bool,
    mismatched_protocol: bool,
}

impl IntroOutcome {
    /// The x23 service that answered, as it is named in its protocol
    /// definition (for example `SchedGateway`).
    pub const fn service(self) -> &'static str {
        self.service
    }

    /// The outcome's name, or a stable placeholder when this build does not
    /// know the discriminant.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The raw wire discriminant.
    pub const fn code(self) -> i32 {
        self.code
    }

    /// Whether the service accepted the protocol this build speaks.
    pub const fn accepted(self) -> bool {
        self.accepted
    }

    /// Whether the service rejected the handshake specifically because it
    /// speaks a different protocol — the one failure an operator can fix by
    /// matching versions rather than by debugging the service.
    pub const fn mismatched_protocol(self) -> bool {
        self.mismatched_protocol
    }
}

/// The three answers a service's intro enum can give, named for this
/// service. Each caller passes its own generated discriminants so no
/// assumption about how hogd numbers them lives here.
struct IntroSpelling {
    service: &'static str,
    success: (i32, &'static str),
    mismatch: (i32, &'static str),
    unknown: &'static str,
}

fn classify(code: i32, spelling: &IntroSpelling) -> IntroOutcome {
    let (name, accepted, mismatched_protocol) = if code == spelling.success.0 {
        (spelling.success.1, true, false)
    } else if code == spelling.mismatch.0 {
        (spelling.mismatch.1, false, true)
    } else {
        (spelling.unknown, false, false)
    };
    IntroOutcome {
        service: spelling.service,
        name,
        code,
        accepted,
        mismatched_protocol,
    }
}

/// Classify a `SchedGateway` intro answer.
pub fn sched_gateway(outcome: SchedGatewayIntroOutcome) -> IntroOutcome {
    classify(
        outcome.0,
        &IntroSpelling {
            service: "SchedGateway",
            success: (
                SchedGatewayIntroOutcome::SchedGatewayIntroSuccess.0,
                "SchedGatewayIntroSuccess",
            ),
            mismatch: (
                SchedGatewayIntroOutcome::SchedGatewayIntroMismatchedProtocol.0,
                "SchedGatewayIntroMismatchedProtocol",
            ),
            unknown: "UnknownSchedGatewayIntroOutcome",
        },
    )
}

/// Classify an `IndexerBlockExplorer` intro answer.
pub fn indexer_block_explorer(outcome: IndexerBlockExplorerIntroOutcome) -> IntroOutcome {
    classify(
        outcome.0,
        &IntroSpelling {
            service: "IndexerBlockExplorer",
            success: (
                IndexerBlockExplorerIntroOutcome::IntroSuccess.0,
                "IntroSuccess",
            ),
            mismatch: (
                IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol.0,
                "IntroMismatchedProtocol",
            ),
            unknown: "UnknownIndexerBlockExplorerIntroOutcome",
        },
    )
}

/// Classify a `DbGateway` intro answer.
pub fn db_gateway(outcome: DbGatewayIntroOutcome) -> IntroOutcome {
    classify(
        outcome.0,
        &IntroSpelling {
            service: "DbGateway",
            success: (
                DbGatewayIntroOutcome::DbGatewayIntroSuccess.0,
                "DbGatewayIntroSuccess",
            ),
            mismatch: (
                DbGatewayIntroOutcome::DbGatewayIntroMismatchedProtocol.0,
                "DbGatewayIntroMismatchedProtocol",
            ),
            unknown: "UnknownDbGatewayIntroOutcome",
        },
    )
}

/// Classify a `ViewGateway` intro answer.
pub fn view_gateway(outcome: ViewGatewayIntroOutcome) -> IntroOutcome {
    classify(
        outcome.0,
        &IntroSpelling {
            service: "ViewGateway",
            success: (
                ViewGatewayIntroOutcome::ViewGatewayIntroSuccess.0,
                "ViewGatewayIntroSuccess",
            ),
            mismatch: (
                ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol.0,
                "ViewGatewayIntroMismatchedProtocol",
            ),
            unknown: "UnknownViewGatewayIntroOutcome",
        },
    )
}
