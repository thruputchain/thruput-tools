use thruput_db_gateway::DbGatewayIntroOutcome;
use thruput_indexer::IndexerBlockExplorerIntroOutcome;
use thruput_scheduler::SchedGatewayIntroOutcome;
use thruput_view::ViewGatewayIntroOutcome;

use thruput_hogd_backend::intro::*;

#[test]
fn every_service_reports_success_for_its_own_success_discriminant() {
    for outcome in [
        sched_gateway(SchedGatewayIntroOutcome::SchedGatewayIntroSuccess),
        indexer_block_explorer(IndexerBlockExplorerIntroOutcome::IntroSuccess),
        db_gateway(DbGatewayIntroOutcome::DbGatewayIntroSuccess),
        view_gateway(ViewGatewayIntroOutcome::ViewGatewayIntroSuccess),
    ] {
        assert!(outcome.accepted(), "{outcome:?}");
        assert!(!outcome.mismatched_protocol(), "{outcome:?}");
    }
}

#[test]
fn a_protocol_mismatch_is_distinguished_from_any_other_rejection() {
    let mismatch = sched_gateway(SchedGatewayIntroOutcome::SchedGatewayIntroMismatchedProtocol);
    assert!(!mismatch.accepted());
    assert!(mismatch.mismatched_protocol());
    assert_eq!(mismatch.name(), "SchedGatewayIntroMismatchedProtocol");
    assert_eq!(mismatch.service(), "SchedGateway");

    let unknown = sched_gateway(SchedGatewayIntroOutcome(4242));
    assert!(!unknown.accepted());
    assert!(!unknown.mismatched_protocol());
    assert_eq!(unknown.name(), "UnknownSchedGatewayIntroOutcome");
    assert_eq!(unknown.code(), 4242);
}

#[test]
fn each_service_names_its_own_mismatch_variant() {
    assert_eq!(
        indexer_block_explorer(IndexerBlockExplorerIntroOutcome::IntroMismatchedProtocol).name(),
        "IntroMismatchedProtocol"
    );
    assert_eq!(
        db_gateway(DbGatewayIntroOutcome::DbGatewayIntroMismatchedProtocol).name(),
        "DbGatewayIntroMismatchedProtocol"
    );
    assert_eq!(
        view_gateway(ViewGatewayIntroOutcome::ViewGatewayIntroMismatchedProtocol).name(),
        "ViewGatewayIntroMismatchedProtocol"
    );
}
