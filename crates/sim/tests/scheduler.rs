use groundhog_sim::manifest::ChaosEventKind;
use groundhog_sim::manifest::SimulationManifest;
use groundhog_sim::manifest::WorkloadKind;

use serde_json::json;

use groundhog_sim::scheduler::*;

fn manifest() -> SimulationManifest {
    serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "schedule-test",
        "seed": "seed-a",
        "target": {"provider": "existing"},
        "loadShape": [
            {"duration": "2s", "users": 2, "ratePerUserHz": 1.0},
            {"duration": "1s", "users": 1, "ratePerUserHz": 2.0}
        ],
        "workloads": [
            {"name": "chain-tip", "weight": 7},
            {"name": "receipt-poll", "weight": 3}
        ],
        "chaos": [{"at": "1s", "kind": "restart-target"}]
    }))
    .expect("manifest fixture should parse")
}

#[test]
fn workload_selection_is_deterministic() {
    let manifest = manifest();
    let a = generate_schedule(&manifest).expect("schedule should generate");
    let b = generate_schedule(&manifest).expect("schedule should generate again");

    assert_eq!(a, b);
    assert!(a.iter().any(|event| matches!(
        event,
        ScheduledSimulationEvent::ActorTick(tick)
            if select_workload_for_tick(&manifest, tick).unwrap() == WorkloadKind::ReceiptPoll
    )));
}

#[test]
fn schedule_order_includes_chaos_by_simulation_time() {
    let manifest = manifest();
    let events = generate_schedule(&manifest).expect("schedule should generate");
    let times: Vec<_> = events
        .iter()
        .map(ScheduledSimulationEvent::simulation_time_ms)
        .collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();

    assert_eq!(times, sorted);
    assert!(matches!(
        events
            .iter()
            .find(|event| event.simulation_time_ms() == 1_000),
        Some(ScheduledSimulationEvent::Chaos(ScheduledChaos {
            kind: ChaosEventKind::RestartTarget,
            ..
        }))
    ));
}

#[test]
fn schedule_cursor_matches_generated_schedule() {
    let manifest = manifest();
    let generated = generate_schedule(&manifest).expect("schedule should generate");
    let cursor: Vec<_> = ScheduleCursor::new(&manifest)
        .expect("cursor should build")
        .collect();

    assert_eq!(cursor, generated);
}

#[test]
fn actors_remain_synchronized_without_staggering() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "schedule-sync-test",
        "seed": "seed-a",
        "target": {"provider": "existing"},
        "loadShape": [
            {"duration": "1s", "users": 4, "ratePerUserHz": 2.0}
        ],
        "workloads": [
            {"name": "chain-tip", "weight": 1}
        ]
    }))
    .expect("manifest fixture should parse");
    let actor_times: Vec<_> = generate_schedule(&manifest)
        .expect("schedule should generate")
        .into_iter()
        .filter_map(|event| match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                Some((tick.actor_id, tick.actor_sequence, tick.simulation_time_ms))
            }
            _ => None,
        })
        .collect();

    assert_eq!(
        actor_times,
        vec![
            (0, 0, 0),
            (1, 0, 0),
            (2, 0, 0),
            (3, 0, 0),
            (0, 1, 500),
            (1, 1, 500),
            (2, 1, 500),
            (3, 1, 500),
        ]
    );
}

#[test]
fn staggered_actors_are_spread_across_tick_interval() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "schedule-stagger-test",
        "seed": "seed-a",
        "target": {"provider": "existing"},
        "loadShape": [
            {
                "duration": "1s",
                "users": 4,
                "ratePerUserHz": 2.0,
                "staggerActors": true
            }
        ],
        "workloads": [
            {"name": "chain-tip", "weight": 1}
        ]
    }))
    .expect("manifest fixture should parse");
    let actor_times: Vec<_> = generate_schedule(&manifest)
        .expect("schedule should generate")
        .into_iter()
        .filter_map(|event| match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                Some((tick.actor_id, tick.actor_sequence, tick.simulation_time_ms))
            }
            _ => None,
        })
        .collect();
    let cursor_actor_times: Vec<_> = ScheduleCursor::new(&manifest)
        .expect("cursor should build")
        .filter_map(|event| match event {
            ScheduledSimulationEvent::ActorTick(tick) => {
                Some((tick.actor_id, tick.actor_sequence, tick.simulation_time_ms))
            }
            _ => None,
        })
        .collect();
    let summary = schedule_summary(&manifest).expect("summary should generate");

    assert_eq!(
        actor_times,
        vec![
            (0, 0, 0),
            (1, 0, 125),
            (2, 0, 250),
            (3, 0, 375),
            (0, 1, 500),
            (1, 1, 625),
            (2, 1, 750),
            (3, 1, 875),
        ]
    );
    assert_eq!(cursor_actor_times, actor_times);
    assert_eq!(summary.scheduled_workloads, 8);
    assert_eq!(summary.shutdown_simulation_time_ms, 875);
}

#[test]
fn schedule_cursor_preserves_tie_breaks_across_phases_and_chaos() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "schedule-tie-test",
        "seed": "seed-a",
        "target": {"provider": "existing"},
        "loadShape": [
            {"duration": "2s", "users": 2, "ratePerUserHz": 2000.0},
            {"duration": "1s", "users": 1, "ratePerUserHz": 2.0}
        ],
        "workloads": [
            {"name": "chain-tip", "weight": 1}
        ],
        "chaos": [{"at": "1ms", "kind": "restart-target"}]
    }))
    .expect("manifest fixture should parse");
    let generated = generate_schedule(&manifest).expect("schedule should generate");
    let cursor: Vec<_> = ScheduleCursor::new(&manifest)
        .expect("cursor should build")
        .collect();

    assert_eq!(cursor, generated);
    let one_ms_events: Vec<_> = cursor
        .iter()
        .filter(|event| event.simulation_time_ms() == 1)
        .collect();
    assert!(matches!(
        one_ms_events.first(),
        Some(ScheduledSimulationEvent::Chaos(_))
    ));
    assert!(
        one_ms_events
            .iter()
            .skip(1)
            .all(|event| matches!(event, ScheduledSimulationEvent::ActorTick(_)))
    );
}

#[test]
fn schedule_cursor_orders_chaos_resource_then_actor_at_same_time() {
    let manifest: SimulationManifest = serde_json::from_value(json!({
        "schemaVersion": 1,
        "kind": "groundhog-simulation",
        "name": "schedule-resource-tie-test",
        "seed": "seed-a",
        "target": {"provider": "existing"},
        "loadShape": [
            {"duration": "2ms", "users": 1, "ratePerUserHz": 1000.0}
        ],
        "workloads": [
            {"name": "chain-tip", "weight": 1}
        ],
        "chaos": [{"at": "1ms", "kind": "restart-target"}],
        "observability": {"resourceSampleInterval": "1ms"}
    }))
    .expect("manifest fixture should parse");
    let generated = generate_schedule(&manifest).expect("schedule should generate");
    let cursor: Vec<_> = ScheduleCursor::new(&manifest)
        .expect("cursor should build")
        .collect();

    assert_eq!(cursor, generated);
    let one_ms_events: Vec<_> = cursor
        .iter()
        .filter(|event| event.simulation_time_ms() == 1)
        .collect();
    assert!(matches!(
        one_ms_events.as_slice(),
        [
            ScheduledSimulationEvent::Chaos(_),
            ScheduledSimulationEvent::ResourceSample(_),
            ScheduledSimulationEvent::ActorTick(_)
        ]
    ));
}

#[test]
fn schedule_summary_matches_generated_schedule_counts_and_shutdown_time() {
    let manifest = manifest();
    let generated = generate_schedule(&manifest).expect("schedule should generate");
    let summary = schedule_summary(&manifest).expect("summary should generate");

    assert_eq!(
        summary.scheduled_workloads,
        generated
            .iter()
            .filter(|event| matches!(event, ScheduledSimulationEvent::ActorTick(_)))
            .count() as u64
    );
    assert_eq!(
        summary.shutdown_simulation_time_ms,
        generated
            .iter()
            .map(ScheduledSimulationEvent::simulation_time_ms)
            .max()
            .unwrap_or_default()
    );
}

#[test]
fn rejects_rates_below_millihz_resolution() {
    let mut manifest = manifest();
    manifest.load_shape[0].rate_per_user_hz = 0.0004;

    let err = generate_schedule(&manifest).expect_err("tiny rate should reject");
    assert!(matches!(err, ScheduleError::RateBelowResolution(_)));
}

#[test]
fn restart_mixed_catalog_keeps_submit_before_restart_and_receipt_after() {
    let manifest: SimulationManifest = serde_json::from_str(include_str!(
        "../../../examples/sim/local-restart-mixed-artifact.json"
    ))
    .expect("catalog manifest should parse");
    let events = generate_schedule(&manifest).expect("schedule should generate");

    let mut has_submit_before_restart = false;
    let mut has_receipt_after_restart = false;
    for event in events {
        let ScheduledSimulationEvent::ActorTick(tick) = event else {
            continue;
        };
        let workload = select_workload_for_tick(&manifest, &tick).expect("workload should select");
        if tick.simulation_time_ms < 2_000 && workload == WorkloadKind::WalletSubmitSmoke {
            has_submit_before_restart = true;
        }
        if tick.simulation_time_ms >= 2_000 && workload == WorkloadKind::ReceiptPoll {
            has_receipt_after_restart = true;
        }
    }

    assert!(has_submit_before_restart);
    assert!(has_receipt_after_restart);
}

#[test]
fn positive_event_scenarios_schedule_events_query_after_contract_call() {
    for manifest_json in [include_str!(
        "../../../examples/sim/local-fibonacci-contract-call-events-smoke.json"
    )] {
        let manifest: SimulationManifest =
            serde_json::from_str(manifest_json).expect("event scenario should parse");
        let events = generate_schedule(&manifest).expect("schedule should generate");
        let mut saw_contract_call = false;
        let mut saw_query_after_call = false;

        for event in events {
            let ScheduledSimulationEvent::ActorTick(tick) = event else {
                continue;
            };
            match select_workload_for_tick(&manifest, &tick).expect("workload should select") {
                WorkloadKind::ContractCall => saw_contract_call = true,
                WorkloadKind::EventsQuery if saw_contract_call => {
                    saw_query_after_call = true;
                }
                _ => {}
            }
        }

        assert!(
            saw_query_after_call,
            "{} should schedule an events-query after at least one contract-call",
            manifest.name
        );
    }
}
